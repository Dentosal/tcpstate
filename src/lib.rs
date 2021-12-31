//! TCP as per https://www.ietf.org/rfc/rfc793.txt

#![cfg_attr(not(any(test, feature = "std")), no_std)]
#![feature(default_free_fn, duration_constants, let_else, drain_filter)]
#![allow(dead_code, unreachable_code)]
#![deny(unused_must_use)]
#![forbid(unsafe_code)]

//! TODO: don't clear_range for SYN,FIN
//! TODO: re_tx FIN
//! TODO: persist timer
//! TODO: Accept new connection in TimeWait state as per RFC 1122 page 88

extern crate alloc;

use core::time::Duration;

use alloc::collections::VecDeque;
use alloc::vec::Vec;

pub mod mock;
pub mod options;

mod event;
mod queue;
mod result;
mod rtt;
mod rx;
mod segment;
mod state;
mod timers;
mod tx;

use crate::mock::*;

use event::{Events, WaitUntil};
use options::*;
use rx::*;
use timers::*;
use tx::*;

pub use event::Cookie;
pub use options::SocketOptions;
pub use result::Error;
pub use rtt::Timings;
pub use segment::{SegmentFlags, SegmentMeta};
pub use state::ConnectionState;

use crate::state::ChannelState;

/// Response to a packet when the socket is closed
/// RFC 793 page 64
pub fn response_to_closed(seg: SegmentMeta) -> SegmentMeta {
    if seg.flags.contains(SegmentFlags::ACK) {
        SegmentMeta {
            seqn: 0,
            ackn: seg.seqn.wrapping_add(seg.data.len() as u32),
            window: 0,
            flags: SegmentFlags::RST | SegmentFlags::ACK,
            data: Vec::new(),
        }
    } else {
        SegmentMeta {
            seqn: seg.ackn,
            ackn: 0,
            window: 0,
            flags: SegmentFlags::RST,
            data: Vec::new(),
        }
    }
}

pub type EventHandler = dyn FnMut(Cookie, Result<(), Error>) + Send;
pub type PacketHandler = dyn FnMut(RemoteAddr, SegmentMeta) + Send;

pub trait UserData {
    fn send(&mut self, dst: RemoteAddr, seg: SegmentMeta);
    fn event(&mut self, cookie: Cookie, result: Result<(), Error>);
}

pub struct Socket<U: UserData> {
    remote: Option<RemoteAddr>,
    connection_state: ConnectionState,
    dup_ack_count: u8,
    congestation_window: u16,
    exp_backoff: u16,
    tx: TxBuffer,
    rx: RxBuffer,
    timings: Timings,
    timers: Timers,
    events: Events<WaitUntil>,
    pub options: SocketOptions,
    pub user_data: U,
}

impl<U: UserData> Socket<U> {
    /// New socket in initial state
    pub fn new(user_data: U) -> Self {
        Self {
            remote: None,
            connection_state: ConnectionState::Closed,
            dup_ack_count: 0,
            congestation_window: INITIAL_WINDOW_SIZE,
            exp_backoff: 1,
            rx: RxBuffer::default(),
            tx: TxBuffer::default(),
            timings: Timings::default(),
            timers: Timers::default(),
            events: Events::new(),
            options: SocketOptions::default(),
            user_data,
        }
    }

    pub(crate) fn trigger_event<F>(&mut self, f: F, r: Result<(), Error>)
    where
        F: Fn(WaitUntil) -> bool,
    {
        for (_, cookie) in self.events.suspended.drain_filter(|(wait, _)| f(*wait)) {
            log::trace!("Triggered cookie {:?} with result {:?}", cookie, r);
            self.user_data.event(cookie, r);
        }
    }

    pub fn state(&self) -> ConnectionState {
        self.connection_state
    }

    /// All state accesses are done using this so they can be logged
    /// and relevant events triggered
    fn set_state(&mut self, state: ConnectionState) {
        log::trace!("State {:?} => {:?}", self.connection_state, state);

        // Check that the transition is valid
        self.connection_state.debug_validate_transition(state);

        // Transition
        self.connection_state = state;

        // Triggers
        if self.connection_state.output_buffer_guaranteed_clear() {
            self.trigger_event(|w| w == WaitUntil::OutputBufferClear, Ok(()));
        }

        match self.connection_state {
            ConnectionState::Established {
                tx_state, rx_state, ..
            } => {
                self.trigger_event(|w| w == WaitUntil::Established, Ok(()));

                if tx_state == ChannelState::Closed {
                    self.trigger_event(|w| w == WaitUntil::TxClosed, Ok(()));
                }
                if rx_state == ChannelState::Closed {
                    self.trigger_event(|w| w == WaitUntil::RxClosed, Ok(()));
                }
                if tx_state == ChannelState::Closed && rx_state == ChannelState::Closed {
                    self.trigger_event(|w| w == WaitUntil::BothClosed, Ok(()));
                }
            }
            _ => {}
        }

        // Reduce state
        if self.connection_state
            == (ConnectionState::Established {
                tx_state: ChannelState::Closed,
                rx_state: ChannelState::Closed,
                close_called: true,
            })
        {
            // TODO: detect passive close and skip timewait
            self.set_state(ConnectionState::TimeWait);
            self.timers.clear();
            self.timers.timewait = Some(Instant::now().add(MAX_SEGMENT_LIFETIME * 2));
        }
    }

    fn clear(&mut self) {
        log::trace!(
            "Clear: State {:?} => {:?}",
            self.connection_state,
            ConnectionState::Closed
        );

        self.connection_state = ConnectionState::Closed;
        self.dup_ack_count = 0;
        self.congestation_window = INITIAL_WINDOW_SIZE;
        self.exp_backoff = 1;
        self.rx = RxBuffer::default();
        self.tx = TxBuffer::default();
        self.timings = Timings::default();
        self.timers = Timers::default();
        self.trigger_event(|_| true, Err(Error::ConnectionClosing));
    }

    fn reset(&mut self) {
        log::trace!(
            "Reset: State {:?} => {:?}",
            self.connection_state,
            ConnectionState::Reset
        );
        self.clear();
        self.connection_state = ConnectionState::Reset;
    }

    /// RFC793, page 26
    fn check_segment_seq_ok(&self, seg: &SegmentMeta) -> bool {
        if self.rx.curr_window() == 0 {
            seg.seq_size() == 0 && seg.seqn == self.rx.next_seqn()
        } else {
            self.rx.in_window(seg.seqn)
                && if seg.seq_size() == 0 {
                    true
                } else {
                    self.rx.in_window(seg.seqn + (seg.seq_size() as u32) - 1)
                }
        }
    }

    /// Remove ACK'd segments from re_tx queue
    fn clear_re_tx_range(&mut self, start: u32, end: u32) {
        log::trace!("Clearing range {}..={} from re_tx buffer", start, end);

        // Trigger events
        self.trigger_event(
            |w| {
                if let WaitUntil::Acknowledged { seqn } = w {
                    // TODO: wrapping
                    start <= seqn && seqn <= end
                } else {
                    false
                }
            },
            Ok(()),
        );

        // Clear queue
        loop {
            let Some(head) = self.tx.re_tx.get(0) else {
                return;
            };

            let start_ok = start <= head.ackn; // TODO: omit, wrap around
            let end_ok = head.ackn + (head.data.len() as u32) <= end;
            if !start_ok || !end_ok {
                return;
            }

            let _ = self.tx.re_tx.pop_front();
        }
    }

    /// Establish a connection
    pub fn call_connect(&mut self, remote: RemoteAddr) -> Result<(), Error> {
        log::trace!("state: {:?}", self.connection_state);
        log::trace!("call_connect {:?}", remote);
        if matches!(
            self.connection_state,
            ConnectionState::Closed | ConnectionState::Reset
        ) {
            self.remote = Some(remote);
            self.set_state(ConnectionState::SynSent);
            self.tx.init();
            self.user_data.send(
                remote,
                SegmentMeta {
                    seqn: self.tx.init_seqn,
                    ackn: 0,
                    window: self.rx.curr_window(),
                    flags: SegmentFlags::SYN,
                    data: Vec::new(),
                },
            );
            self.events.return_continue_after(WaitUntil::Established)
        } else {
            Err(Error::InvalidStateTransition)
        }
    }

    /// Send some data
    /// RFC 793 page 55
    pub fn call_send(&mut self, input_data: Vec<u8>) -> Result<(), Error> {
        if let Ok(s) = core::str::from_utf8(&input_data) {
            log::trace!("call_send {:?}", s);
        } else {
            log::trace!("call_send {:?}", input_data);
        }
        log::trace!("state: {:?}", self.connection_state);
        assert!(!input_data.is_empty());
        match self.connection_state {
            ConnectionState::Reset => Err(Error::ConnectionReset),
            ConnectionState::Closed | ConnectionState::Listen => Err(Error::NotConnected),
            ConnectionState::TimeWait => Err(Error::ConnectionClosing),
            ConnectionState::SynSent | ConnectionState::SynReceived => {
                let len = input_data.len() as u32;
                self.tx.tx.write_bytes(input_data);
                log::trace!("send: buffered");
                return self.events.return_continue_after(WaitUntil::Acknowledged {
                    seqn: self.tx.next.wrapping_add(len),
                });
            }
            ConnectionState::Established { tx_state, .. } => {
                debug_assert!(tx_state == ChannelState::Open);
                self.tx.tx.write_bytes(input_data);
                log::trace!("send: buffered hs={:?}", self.tx.unack_has_space());
                // TODO: sub MAX_SEGMENT_SIZE segment header
                // TODO: Nagle algo
                let avail = self.tx.tx.available_bytes();
                if self.tx.unack_has_space()
                    && (avail >= MAX_SEGMENT_SIZE
                        || (avail > 0 && self.options.nagle_delay == Duration::ZERO))
                {
                    self.send_ack();
                }
                Ok(())
            }
        }
    }

    /// Amount of data available for receiving without blocking
    pub fn recv_available(&mut self) -> usize {
        self.rx.available_bytes()
    }

    /// Receive some data, if any available
    pub fn call_recv(&mut self, buffer: &mut [u8]) -> Result<usize, Error> {
        log::trace!("call_recv len={}", buffer.len());
        log::trace!("state: {:?}", self.connection_state);
        match self.connection_state {
            ConnectionState::Reset => Err(Error::ConnectionReset),
            ConnectionState::Closed => Err(Error::NotConnected),
            ConnectionState::TimeWait => Err(Error::ConnectionClosing),
            ConnectionState::Listen | ConnectionState::SynSent | ConnectionState::SynReceived => {
                log::debug!("Suspending read request until the connection is established");
                self.events.return_retry_after(WaitUntil::Established)
            }
            ConnectionState::Established {
                rx_state,
                tx_state,
                close_called,
            } => match rx_state {
                ChannelState::Closed => Ok(0),
                ChannelState::Open => {
                    if self.rx.available_bytes() >= buffer.len() {
                        // Move data to caller buffer
                        let data = self.rx.read_bytes(buffer.len());
                        buffer.copy_from_slice(&data);

                        // ACK the data
                        self.send_ack();

                        Ok(data.len())
                    } else {
                        log::debug!("Suspending read request until enough data is available");
                        self.events.return_retry_after(WaitUntil::Recv {
                            count: buffer.len() as u32,
                        })
                    }
                }
                ChannelState::Fin => {
                    // Return the rest of the data, or as mucb as we can fit into the buffer
                    let data = self.rx.read_bytes(buffer.len());

                    buffer[..data.len()].copy_from_slice(&data);

                    if data.len() < buffer.len() {
                        self.set_state(ConnectionState::Established {
                            tx_state,
                            rx_state: ChannelState::Closed,
                            close_called,
                        });
                    }

                    self.send_ack();

                    Ok(data.len())

                    // TODO: Signal ConnectionClosing when FIN has been reached?
                }
            },
        }
    }

    /// Closes the send channel, sending a FIN packet after the current data buffer
    pub fn call_shutdown(&mut self) -> Result<(), Error> {
        log::trace!("call_shutdown");
        log::trace!("state: {:?}", self.connection_state);
        match self.connection_state {
            ConnectionState::Reset => Err(Error::ConnectionReset),
            ConnectionState::Closed => Err(Error::NotConnected),
            ConnectionState::TimeWait => Err(Error::ConnectionClosing),
            ConnectionState::Listen | ConnectionState::SynSent => {
                let has_queued_recvs = todo!("RECV queue");
                if has_queued_recvs {
                    todo!("Send ConnectionClosing to the RECVs");
                }
                self.clear();
                Ok(())
            }
            ConnectionState::SynReceived => {
                todo!("queue if any new/queued sends");
                todo!("send fin");
            }
            ConnectionState::Established { tx_state, .. } => match tx_state {
                ChannelState::Closed | ChannelState::Fin => Err(Error::ConnectionClosing),
                ChannelState::Open => {
                    self.tx.tx.mark_fin();
                    self.send_ack();
                    Ok(())
                }
            },
        }
    }

    /// Request closing the socket.
    pub fn call_close(&mut self) -> Result<(), Error> {
        log::trace!("call_close");
        log::trace!("state: {:?}", self.connection_state);
        match self.connection_state {
            ConnectionState::Reset => Err(Error::ConnectionReset),
            ConnectionState::Closed => Err(Error::NotConnected),
            ConnectionState::TimeWait => Err(Error::ConnectionClosing),
            ConnectionState::Listen | ConnectionState::SynSent => {
                let has_queued_recvs = todo!("RECV queue");
                if has_queued_recvs {
                    todo!("Send ConnectionClosing to the RECVs");
                }
                self.clear();
                Ok(())
            }
            ConnectionState::SynReceived => {
                let new_sends = todo!("New sends");
                let queued_sends = todo!("Queued sends");
                if new_sends || queued_sends {
                    // Cannot close socket yet, as some items are queued
                    todo!("Queue sends");
                } else {
                    self.tx.tx.mark_fin();
                    self.send_ack();

                    Ok(())
                }
            }
            ConnectionState::Established { tx_state, .. } => {
                if tx_state == ChannelState::Open {
                    self.tx.tx.mark_fin();
                    self.send_ack();
                }

                let tx_state = self.connection_state.tx();
                let rx_state = self.connection_state.rx();

                self.set_state(ConnectionState::Established {
                    tx_state,
                    rx_state,
                    close_called: true,
                });

                match tx_state {
                    ChannelState::Open | ChannelState::Fin => {
                        return self.events.return_continue_after(WaitUntil::BothClosed);
                    }
                    ChannelState::Closed => {}
                }

                match rx_state {
                    ChannelState::Open | ChannelState::Fin => {
                        return self.events.return_continue_after(WaitUntil::BothClosed)
                    }
                    ChannelState::Closed => {}
                }

                Ok(())
            }
        }
    }

    /// Request closing the socket
    /// Will never block to wait for completion of ongoing calls
    pub fn call_abort(&mut self) -> Result<(), Error> {
        log::trace!("call_abort");
        log::trace!("state: {:?}", self.connection_state);
        match self.connection_state {
            ConnectionState::Reset => Err(Error::ConnectionReset),
            ConnectionState::Closed => Err(Error::NotConnected),
            ConnectionState::Listen | ConnectionState::SynSent => {
                let has_queued_recvs = todo!("RECV queue");
                if has_queued_recvs {
                    todo!("Send ConnectionReset to the RECVs");
                }
                self.clear();
                Ok(())
            }
            ConnectionState::SynReceived | ConnectionState::Established { .. } => {
                self.user_data.send(
                    self.remote.expect("No remote set"),
                    SegmentMeta {
                        seqn: self.tx.next,
                        ackn: 0,
                        window: self.rx.curr_window(),
                        flags: SegmentFlags::RST,
                        data: Vec::new(),
                    },
                );
                let mut r = Ok(());

                let queued_sends = todo!("Queued sends");
                let queued_recvs = todo!("Queued recvs");
                if queued_sends || queued_recvs {
                    r = Err(Error::ConnectionReset);
                }

                self.clear();
                r
            }
            ConnectionState::TimeWait => {
                self.clear();
                Ok(())
            }
        }
    }

    /// Sends an ack packet, including any data form buffer if possible.
    fn send_ack(&mut self) {
        // TODO: check window size before sending any payload

        let limit = MAX_SEGMENT_SIZE.min(self.tx.space_available() as usize);

        let (tx_data, tx_fin) = if limit == 0 {
            (Vec::new(), false)
        } else {
            let (tx_data, tx_fin) = self.tx.tx.read_bytes(limit);
            if tx_data.len() == 0 {
                self.trigger_event(|w| w == WaitUntil::OutputBufferClear, Ok(()));
            }
            (tx_data, tx_fin)
        };

        if self.connection_state.fin_sent() {
            debug_assert!(tx_data.is_empty(), "Data must not be sent after FIN");
        }

        let flags = if tx_fin && !self.connection_state.fin_sent() {
            self.set_state(ConnectionState::Established {
                tx_state: ChannelState::Fin,
                rx_state: self.connection_state.rx(),
                close_called: self.connection_state.close_called(),
            });
            SegmentFlags::FIN | SegmentFlags::ACK
        } else {
            SegmentFlags::ACK
        };

        let needs_resend = flags.contains(SegmentFlags::FIN) || !tx_data.is_empty();

        let seg = SegmentMeta {
            seqn: self.tx.next,
            ackn: self.rx.curr_ackn(),
            window: self.rx.curr_window(),
            flags,
            data: tx_data,
        };

        if seg.seq_size() == 0 && (seg.seqn, seg.ackn, seg.window) == self.tx.last_sent {
            log::warn!("Skipping duplicate resend");
            return;
        }

        self.tx.on_send(seg.clone());
        self.user_data
            .send(self.remote.expect("No remote set"), seg);

        if needs_resend {
            self.timers.re_tx = Some(Instant::now().add(self.timings.rto));
        }
    }

    /// Called on incoming segment
    /// See RFC 793 page 64
    pub fn on_segment(&mut self, seg: SegmentMeta) {
        log::trace!("on_segment {:?}", seg);
        log::trace!("state: {:?}", self.connection_state);

        match self.connection_state {
            ConnectionState::Closed | ConnectionState::Reset => {
                self.user_data
                    .send(self.remote.expect("No remote set"), response_to_closed(seg));
            }
            ConnectionState::Listen => {
                if seg.flags.contains(SegmentFlags::RST) {
                    return;
                }

                if seg.flags.contains(SegmentFlags::ACK) {
                    self.user_data.send(
                        self.remote.expect("No remote set"),
                        SegmentMeta {
                            seqn: seg.ackn,
                            ackn: 0,
                            window: 0,
                            flags: SegmentFlags::RST,
                            data: Vec::new(),
                        },
                    );
                    return;
                }

                if !seg.flags.contains(SegmentFlags::SYN) {
                    log::warn!("Invalid packet received, dropping");
                    return;
                }

                // TODO: Store remote peer address?
                self.rx.init(seg.seqn);
                // TODO: queue data, control if any available
                assert!(seg.data.len() == 0, "TODO");
                let seqn = random_seqnum();
                self.tx.next = seqn.wrapping_add(1);
                self.tx.unack = seqn;
                self.tx.window = seg.window;
                self.set_state(ConnectionState::SynReceived);
                self.user_data.send(
                    self.remote.expect("No remote set"),
                    SegmentMeta {
                        seqn,
                        ackn: self.rx.curr_ackn(),
                        window: self.rx.curr_window(),
                        flags: SegmentFlags::SYN | SegmentFlags::ACK,
                        data: Vec::new(),
                    },
                );
            }

            ConnectionState::SynSent => {
                if seg.flags.contains(SegmentFlags::ACK) {
                    if (seg.ackn <= self.tx.init_seqn || seg.ackn > self.tx.next)
                        && !(seg.ackn <= self.tx.unack && seg.ackn <= self.tx.next)
                    {
                        self.user_data.send(
                            self.remote.expect("No remote set"),
                            SegmentMeta {
                                seqn: seg.ackn,
                                ackn: 0,
                                window: self.rx.curr_window(),
                                flags: SegmentFlags::RST,
                                data: Vec::new(),
                            },
                        );
                        return;
                    }
                }

                if seg.flags.contains(SegmentFlags::RST) {
                    self.reset();
                    return;
                }

                if seg.flags.contains(SegmentFlags::SYN) {
                    self.rx.init(seg.seqn);
                    self.clear_re_tx_range(self.tx.unack, seg.ackn);
                    self.tx.unack = seg.ackn;
                    self.tx.window = seg.window;
                    if self.tx.unack > self.tx.init_seqn {
                        // Our SYN has been ACK'd
                        self.set_state(ConnectionState::FULLY_OPEN);
                        self.send_ack();
                    } else {
                        todo!("If packet has data, queue it until ESTABLISHED");
                        self.set_state(ConnectionState::SynReceived);
                        self.user_data.send(
                            self.remote.expect("No remote set"),
                            SegmentMeta {
                                seqn: self.tx.init_seqn,
                                ackn: self.rx.curr_ackn(),
                                window: self.rx.curr_window(),
                                flags: SegmentFlags::SYN | SegmentFlags::ACK,
                                data: Vec::new(),
                            },
                        );
                    }
                }
            }
            ConnectionState::TimeWait => {
                if !self.check_segment_seq_ok(&seg) {
                    log::trace!("RST!!");
                    if !seg.flags.contains(SegmentFlags::RST) {
                        self.user_data.send(
                            self.remote.expect("No remote set"),
                            SegmentMeta {
                                seqn: self.tx.next,
                                ackn: self.rx.curr_ackn(),
                                window: 0,
                                flags: SegmentFlags::ACK,
                                data: Vec::new(),
                            },
                        );
                    }
                } else if seg.flags.contains(SegmentFlags::RST) {
                    self.clear();
                } else if !seg.flags.contains(SegmentFlags::SYN) {
                    if seg.flags.contains(SegmentFlags::ACK)
                        && seg.flags.contains(SegmentFlags::FIN)
                        && self.tx.is_non_ordered_ack(seg.ackn)
                    {
                        todo!("FIN bit processing");
                        self.timers.clear();
                        todo!("set timewait 2MSL");
                    }
                } else {
                    self.user_data.send(
                        self.remote.expect("No remote set"),
                        SegmentMeta {
                            seqn: self.tx.next,
                            ackn: 0,
                            window: 0,
                            flags: SegmentFlags::RST,
                            data: Vec::new(),
                        },
                    );

                    todo!("Signal connection reset to pending reads/writes");

                    self.clear();
                }
            }
            ConnectionState::SynReceived | ConnectionState::Established { .. } => {
                // Acceptability check (RFC 793 page 36 and 68)
                // TODO: process valid ACKs and RSTs even if receive_window == 0?
                if !self.check_segment_seq_ok(&seg) {
                    log::trace!("Non-acceptable segment {:#?}", seg);
                    if !seg.flags.contains(SegmentFlags::RST) {
                        self.user_data.send(
                            self.remote.expect("No remote set"),
                            SegmentMeta {
                                seqn: self.tx.next,
                                ackn: self.rx.curr_ackn(),
                                window: self.rx.curr_window(),
                                flags: SegmentFlags::ACK,
                                data: Vec::new(),
                            },
                        );
                    }
                    return;
                }

                self.tx.window = seg.window;

                // self.tx.window = seg.window.min(self.congestation_window);

                // if self.connection_state != ConnectionState::SynSent
                //     && seg.flags.contains(SegmentFlags::RST)
                // {
                //     todo!("RFC 793 page 36 Reset processing");
                // }

                if seg.flags.contains(SegmentFlags::RST) {
                    match self.connection_state {
                        ConnectionState::SynReceived => {
                            todo!("If actively connecting, error ConnectionRefused");
                            self.clear();
                            self.set_state(ConnectionState::Listen);
                        }
                        ConnectionState::Established { .. } => {
                            todo!("Pending RECV/SEND calls: signal error ConnectionReset");
                            self.clear();
                            todo!("Return error?");
                        }
                        _ => unreachable!(),
                    }
                    return;
                }

                if seg.flags.contains(SegmentFlags::SYN) {
                    log::warn!("Peer error: SYN");

                    todo!("Pending RECV/SEND calls: signal error ConnectionReset");

                    // Error! Send reset, flush all queues
                    self.user_data.send(
                        self.remote.expect("No remote set"),
                        SegmentMeta {
                            seqn: self.tx.next,
                            ackn: 0,
                            window: self.rx.curr_window(),
                            flags: SegmentFlags::RST,
                            data: Vec::new(),
                        },
                    );

                    todo!("Return error?");
                }

                if seg.flags.contains(SegmentFlags::ACK) {
                    if self.connection_state == ConnectionState::SynReceived {
                        // TODO: wrapping
                        if self.tx.unack <= seg.ackn && seg.ackn <= self.tx.next {
                            self.set_state(ConnectionState::FULLY_OPEN);
                        }
                    }

                    if seg.ackn < self.tx.unack || seg.ackn > self.tx.next {
                        // log::warn!("Dropping invalid ACK");
                        panic!("Dropping invalid ACK"); //XXX
                        return;
                    }

                    if self.tx.unack != seg.ackn {
                        // New valid ACK
                        debug_assert!(self.tx.unack < seg.ackn && seg.ackn <= self.tx.next);

                        // Window update subroutine
                        if self.dup_ack_count > 0 {
                            self.congestation_window = SLOW_START_TRESHOLD;
                        } else if self.congestation_window <= SLOW_START_TRESHOLD {
                            self.congestation_window *= 2;
                        } else {
                            // TODO: cast better
                            self.congestation_window += seg.data.len() as u16;
                        }

                        // End of window update
                        self.tx.window = seg.window.min(self.congestation_window);

                        // Signal that data is readable for any calls waiting for that
                        let available = self.rx.available_bytes();
                        self.trigger_event(
                            |w| {
                                if let WaitUntil::Recv { count } = w {
                                    (count as usize) <= available
                                } else {
                                    false
                                }
                            },
                            Ok(()),
                        );

                        self.clear_re_tx_range(self.tx.unack, seg.ackn);
                        self.tx.unack = seg.ackn;
                        self.exp_backoff = 1;
                        self.timers.re_tx = None;

                        self.trigger_event(
                            |until| {
                                if let WaitUntil::Acknowledged { seqn } = until {
                                    seqn <= seg.ackn
                                } else {
                                    false
                                }
                            },
                            Ok(()),
                        );
                    }
                    if let ConnectionState::Established {
                        rx_state,
                        tx_state,
                        close_called,
                    } = self.connection_state
                    {
                        if tx_state == ChannelState::Fin {
                            if seg.ackn == self.tx.unack {
                                log::trace!("ACK for our FIN received");
                                self.set_state(ConnectionState::Established {
                                    tx_state: ChannelState::Closed,
                                    rx_state,
                                    close_called,
                                });
                            }
                        }
                    }

                    // Store segment data
                    if seg.seqn == self.rx.next_seqn() {
                        self.rx.write_bytes(seg.data);
                        if seg.flags.contains(SegmentFlags::FIN) {
                            self.rx.mark_network_fin();
                        }
                    } else if seg.seqn < self.rx.next_seqn() {
                        log::trace!("Past data, discard");
                    } else {
                        todo!("Out-of-order data");
                    }
                }

                if matches!(
                    self.connection_state,
                    ConnectionState::Closed | ConnectionState::Listen | ConnectionState::SynSent
                ) {
                    return;
                }

                if seg.flags.contains(SegmentFlags::FIN) {
                    log::trace!("Got FIN packet");

                    // Retry all recvs, which will not return ConnectionClosing after the buffer is empty
                    self.trigger_event(|until| matches!(until, WaitUntil::Recv { .. }), Ok(()));

                    match self.connection_state {
                        ConnectionState::SynReceived => {
                            self.set_state(ConnectionState::Established {
                                tx_state: ChannelState::Open,
                                rx_state: ChannelState::Fin,
                                close_called: false,
                            });
                        }
                        ConnectionState::Established {
                            tx_state,
                            rx_state,
                            close_called,
                        } => {
                            self.set_state(ConnectionState::Established {
                                tx_state,
                                rx_state: match rx_state {
                                    ChannelState::Open => ChannelState::Fin,
                                    ChannelState::Fin | ChannelState::Closed => {
                                        log::warn!("Duplicate FIN received");
                                        ChannelState::Fin
                                    }
                                },
                                close_called,
                            });
                        }
                        ConnectionState::TimeWait => {
                            todo!("Restart 2MSL time-wait timeout");
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}

pub struct ListenSocket<U: UserData> {
    queue: VecDeque<(RemoteAddr, SegmentMeta)>,
    backlog: usize,
    open: bool,
    events: Events<()>,
    pub user_data: U,
}

impl<U: UserData> ListenSocket<U> {
    /// New socket in initial state
    pub fn call_listen(backlog: usize, user_data: U) -> Self {
        Self {
            queue: VecDeque::new(),
            backlog: backlog.max(1),
            open: true,
            events: Events::new(),
            user_data,
        }
    }

    /// Request closing the socket
    /// Will cancel ongoing listen if any in progress
    pub fn call_close(&mut self) {
        log::trace!("call_close");

        self.open = false;

        for (_, cookie) in self.events.suspended.drain() {
            log::trace!("Closing pending listen call {:?}", cookie);
            self.user_data.event(cookie, Err(Error::ListenClosed));
        }

        // RST all queued connections
        if !self.queue.is_empty() {
            log::trace!("Listen socket closed, sending RST to all queued connections");
        }
        for (addr, seg) in self.queue.drain(..) {
            self.user_data.send(
                addr,
                SegmentMeta {
                    seqn: seg.ackn,
                    ackn: 0,
                    window: 0,
                    flags: SegmentFlags::RST,
                    data: Vec::new(),
                },
            );
        }
    }

    /// Accept a new connection from the backlog
    pub fn call_accept<N: UserData, D: FnOnce() -> N>(
        &mut self,
        make_user_data: D,
    ) -> Result<(RemoteAddr, Socket<N>), Error> {
        log::trace!("call_accept");

        if !self.open {
            return Err(Error::ConnectionClosing);
        }

        if let Some((addr, seg)) = self.queue.pop_front() {
            let user_data = make_user_data();
            let mut socket = Socket::new(user_data);
            socket.remote = Some(addr);
            socket.set_state(ConnectionState::Listen);
            socket.on_segment(seg);
            Ok((addr, socket))
        } else {
            self.events.return_retry_after(())
        }
    }

    /// Called on incoming segment
    /// See RFC 793 page 64
    pub fn on_segment(&mut self, addr: RemoteAddr, seg: SegmentMeta) {
        log::trace!("on_segment {:?}", seg);

        if seg.flags.contains(SegmentFlags::RST) {
            return;
        }

        if seg.flags.contains(SegmentFlags::ACK) || !self.open {
            if self.open {
                log::trace!("Listen socket received ACK, responding with RST");
            } else {
                log::trace!("Listen socket iu closed, responding with RST");
            }
            self.user_data.send(
                addr,
                SegmentMeta {
                    seqn: seg.ackn,
                    ackn: 0,
                    window: 0,
                    flags: SegmentFlags::RST,
                    data: Vec::new(),
                },
            );
            return;
        }

        if !seg.flags.contains(SegmentFlags::SYN) {
            log::warn!("Invalid packet received, dropping");
            return;
        }

        let queue_full = self.queue.len() >= self.backlog;

        if queue_full {
            log::debug!("Listen backlog full, dropping incoming connection");
            self.user_data.send(
                addr,
                SegmentMeta {
                    seqn: seg.ackn.wrapping_add(1),
                    ackn: 0,
                    window: 0,
                    flags: SegmentFlags::ACK | SegmentFlags::RST,
                    data: Vec::new(),
                },
            );
            return;
        }

        self.queue.push_back((addr, seg));

        // Awake an arbitrarily selected single listener
        if let Some((_, cookie)) = self.events.suspended.iter().next().copied() {
            log::trace!("Awaking pending close call call {:?}", cookie);
            self.events.suspended.remove(&((), cookie));
            self.user_data.event(cookie, Ok(()));
        };
    }
}
