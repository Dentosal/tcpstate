//! TCP as per https://www.ietf.org/rfc/rfc793.txt

#![cfg_attr(not(any(test, feature = "std")), no_std)]
#![feature(default_free_fn, duration_constants, let_else, drain_filter)]
#![allow(dead_code, unreachable_code)]
#![deny(unused_must_use)]

//! TODO: Return Error vs Signal User
//! TODO: Window scaling
//! TODO: Wrapping comparisons with windows
//! TODO: std-compat
//! TODO: buffer size limits (configurable)
//! TODO: don't clear_range for SYN,FIN
//! TODO: re_tx FIN
//! TODO: Accept new connection in TimeWait state as per RFC 1122 page 88

extern crate alloc;

use core::time::Duration;

use alloc::vec::Vec;
use hashbrown::HashSet;

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

use event::WaitUntil;
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

#[derive(Debug, Clone)]
pub struct Socket {
    connection_state: ConnectionState,
    remote_address: Option<RemoteAddr>,
    dup_ack_count: u8,
    congestation_window: u16,
    exp_backoff: u16,
    tx: TxBuffer,
    rx: RxBuffer,
    timings: Timings,
    timers: Timers,
    /// Suspended user queries waiting for condition
    events_suspended: HashSet<(WaitUntil, Cookie)>,
    /// Suspended user queries ready for continuing
    events_ready: HashSet<Cookie>,
    next_cookie: Cookie,
    pub options: SocketOptions,
}

impl Socket {
    /// New socket in initial state
    pub fn new() -> Self {
        Self {
            connection_state: ConnectionState::Closed,
            remote_address: None,
            dup_ack_count: 0,
            congestation_window: INITIAL_WINDOW_SIZE,
            exp_backoff: 1,
            rx: RxBuffer::default(),
            tx: TxBuffer::default(),
            timings: Timings::default(),
            timers: Timers::default(),
            events_suspended: HashSet::new(),
            events_ready: HashSet::new(),
            next_cookie: Cookie::ZERO,
            options: SocketOptions::default(),
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

        // Reduce state
        if self.connection_state
            == (ConnectionState::Established {
                tx_state: ChannelState::Closed,
                rx_state: ChannelState::Closed,
                close_called: true,
            })
        {
            self.set_state(ConnectionState::TimeWait);
            self.timers.clear();
            self.timers.timewait = Some(Instant::now().add(MAX_SEGMENT_LIFETIME * 2));
        }

        // Triggers
        if self.connection_state.output_buffer_guaranteed_clear() {
            self.trigger(|w| w == WaitUntil::OutputBufferClear);
        }

        match self.connection_state {
            ConnectionState::Established {
                tx_state, rx_state, ..
            } => {
                self.trigger(|w| w == WaitUntil::Established);
            }
            _ => {}
        }
    }

    fn clear(&mut self) {
        log::trace!(
            "Clear: State {:?} => {:?}",
            self.connection_state,
            ConnectionState::Closed
        );
        *self = Self::new();
    }

    fn reset(&mut self, error: Error) {
        log::trace!(
            "Error: State {:?} => {:?}",
            self.connection_state,
            ConnectionState::Closed
        );
        *self = Self::new();
        self.connection_state = ConnectionState::Reset;
    }

    /// RFC793, page 25
    fn check_segment_seq_ok(&self, seg: &SegmentMeta) -> bool {
        if self.rx.window_size() == 0 {
            seg.data.len() == 0 && seg.seqn == self.rx.curr_ackn()
        } else {
            self.rx.in_window(seg.seqn)
                && if seg.data.len() == 0 {
                    true
                } else {
                    self.rx.in_window(seg.seqn + (seg.data.len() as u32) - 1)
                }
        }
    }

    /// Remove ACK'd segments from re_tx queue
    fn clear_re_tx_range(&mut self, start: u32, end: u32) {
        log::trace!("Clearing range {}..={} from re_tx buffer", start, end);

        // Trigger events
        self.trigger(|w| {
            if let WaitUntil::Acknowledged { seqn } = w {
                // TODO: wrapping
                start <= seqn && seqn <= end
            } else {
                false
            }
        });

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

    pub fn take_outbound(&mut self) -> Option<SegmentMeta> {
        self.tx.send_now.pop_front()
    }

    /// Establish a connection
    pub fn call_connect(&mut self, remote: RemoteAddr) -> Result<(), Error> {
        log::trace!("state: {:?}", self.connection_state);
        log::trace!("call_connect {:?}", remote);
        debug_assert!(self.events_ready.is_empty());
        if matches!(
            self.connection_state,
            ConnectionState::Closed | ConnectionState::Reset
        ) {
            self.remote_address = Some(remote);
            self.set_state(ConnectionState::SynSent);
            self.tx.init();
            self.tx.send_now.push_back(SegmentMeta {
                seqn: self.tx.init_seqn,
                ackn: 0,
                window: self.rx.window_size(),
                flags: SegmentFlags::SYN,
                data: Vec::new(),
            });
            Ok(())
        } else {
            Err(Error::InvalidStateTransition)
        }
    }

    /// Listen for connection
    pub fn call_listen(&mut self) -> Result<(), Error> {
        log::trace!("call_listen");
        log::trace!("state: {:?}", self.connection_state);
        debug_assert!(self.events_ready.is_empty());
        if matches!(
            self.connection_state,
            ConnectionState::Closed | ConnectionState::Reset
        ) {
            self.set_state(ConnectionState::Listen);
            Ok(())
        } else {
            Err(Error::InvalidStateTransition)
        }
    }

    /// Send some data
    /// RFC 793 page 55
    pub fn call_send(&mut self, input_data: Vec<u8>) -> Result<(), Error> {
        log::trace!("call_send {:?}", input_data);
        log::trace!("state: {:?}", self.connection_state);
        assert!(!input_data.is_empty());
        debug_assert!(self.events_ready.is_empty());
        match self.connection_state {
            ConnectionState::Reset => Err(Error::ConnectionReset),
            ConnectionState::Closed | ConnectionState::Listen => Err(Error::NotConnected),
            ConnectionState::TimeWait => Err(Error::ConnectionClosing),
            ConnectionState::SynSent | ConnectionState::SynReceived => {
                let len = input_data.len() as u32;
                self.tx.tx.write_bytes(input_data);
                return self.return_continue_after(WaitUntil::Acknowledged {
                    seqn: self.tx.next.wrapping_add(len),
                });
            }
            ConnectionState::Established { tx_state, .. } => {
                debug_assert!(tx_state == ChannelState::Open);
                self.tx.tx.write_bytes(input_data);
                if self.tx.unack_has_space() {
                    // TODO: sub MAX_SEGMENT_SIZE segment header
                    // TODO: condition
                    let avail = self.tx.tx.available_bytes();
                    if avail >= MAX_SEGMENT_SIZE
                        || (avail > 0 && self.options.nagle_delay == Duration::ZERO)
                    {
                        self.send_ack();
                    }
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
        debug_assert!(self.events_ready.is_empty());
        match self.connection_state {
            ConnectionState::Reset => Err(Error::ConnectionReset),
            ConnectionState::Closed => Err(Error::NotConnected),
            ConnectionState::TimeWait => Err(Error::ConnectionClosing),
            ConnectionState::Listen | ConnectionState::SynSent | ConnectionState::SynReceived => {
                log::debug!("Suspending read request until the connection is established");
                self.return_retry_after(WaitUntil::Established)
            }
            ConnectionState::Established {
                tx_state,
                rx_state,
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
                        self.return_retry_after(WaitUntil::Recv {
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

    /// Send FIN after all data is sent
    fn close_tx_channel(&mut self, close_socket: bool) {
        let tx_state = if let ConnectionState::Established { tx_state, .. } = self.connection_state
        {
            match tx_state {
                ChannelState::Open => ChannelState::Fin,
                ChannelState::Fin => ChannelState::Fin,
                ChannelState::Closed => ChannelState::Closed,
            }
        } else {
            ChannelState::Fin
        };

        self.tx.tx.mark_fin();

        // Attempt to send any data left and FIN
        self.send_ack();

        if let ConnectionState::Established {
            rx_state,
            close_called,
            ..
        } = self.connection_state
        {
            self.set_state(ConnectionState::Established {
                tx_state,
                rx_state,
                close_called: close_called || close_socket,
            });
        } else {
            self.set_state(ConnectionState::Established {
                tx_state,
                rx_state: ChannelState::Open,
                close_called: close_socket,
            });
        }
    }

    /// Closes the send channel, sending a FIN packet after the current data buffer
    pub fn call_shutdown(&mut self) -> Result<(), Error> {
        log::trace!("call_shutdown");
        log::trace!("state: {:?}", self.connection_state);
        debug_assert!(self.events_ready.is_empty());
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
                    self.close_tx_channel(false);
                    Ok(())
                }
            },
        }
    }

    /// Request closing the socket
    pub fn call_close(&mut self) -> Result<(), Error> {
        log::trace!("call_close");
        log::trace!("state: {:?}", self.connection_state);
        debug_assert!(self.events_ready.is_empty());
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
                    self.close_tx_channel(true);
                    Ok(())
                }
            }
            ConnectionState::Established { close_called, .. } => {
                if close_called {
                    Err(Error::ConnectionClosing)
                } else {
                    self.close_tx_channel(true);
                    Ok(())
                }
            }
        }
    }

    /// Request closing the socket
    pub fn call_abort(&mut self) -> Result<(), Error> {
        log::trace!("call_abort");
        log::trace!("state: {:?}", self.connection_state);
        debug_assert!(self.events_ready.is_empty());
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
                self.tx.send_now.push_back(SegmentMeta {
                    seqn: self.tx.next,
                    ackn: 0,
                    window: self.rx.window_size(),
                    flags: SegmentFlags::RST,
                    data: Vec::new(),
                });
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

        let (tx_data, tx_fin) = self.tx.tx.read_bytes(MAX_SEGMENT_SIZE);
        if tx_data.len() == 0 {
            self.trigger(|w| w == WaitUntil::OutputBufferClear);
        }

        let len = tx_data.len();

        if self.connection_state.fin_sent() {
            debug_assert!(len == 0, "Data must not be sent after FIN");
        }

        let flags = if tx_fin && !self.connection_state.fin_sent() {
            SegmentFlags::FIN | SegmentFlags::ACK
        } else {
            SegmentFlags::ACK
        };

        self.tx.send(SegmentMeta {
            seqn: self.tx.next,
            ackn: self.rx.curr_ackn(),
            window: self.rx.window_size(),
            flags,
            data: tx_data,
        });

        if len != 0 {
            self.timers.re_tx = Some(Instant::now().add(self.timings.rto));
        }
    }

    /// Called on incoming segment
    /// See RFC 793 page 64
    pub fn on_segment(&mut self, seg: SegmentMeta) {
        log::trace!("state: {:?}", self.connection_state);
        log::trace!("on_segment {:?}", seg);

        match self.connection_state {
            ConnectionState::Closed | ConnectionState::Reset => {
                self.tx.send_now.push_back(response_to_closed(seg));
            }
            ConnectionState::Listen => {
                if seg.flags.contains(SegmentFlags::RST) {
                    return;
                }

                if seg.flags.contains(SegmentFlags::ACK) {
                    self.tx.send_now.push_back(SegmentMeta {
                        seqn: seg.ackn,
                        ackn: 0,
                        window: 0,
                        flags: SegmentFlags::RST,
                        data: Vec::new(),
                    });
                    return;
                }

                if !seg.flags.contains(SegmentFlags::SYN) {
                    log::warn!("Invalid packet received, dropping");
                    return;
                }

                // TODO: Store remote peer address
                self.rx.init(seg.seqn);
                // TODO: queue data, control if any available
                assert!(seg.data.len() == 0, "TODO");
                let seqn = random_seqnum();
                self.tx.next = seqn.wrapping_add(1);
                self.tx.unack = seqn;
                self.set_state(ConnectionState::SynReceived);
                self.tx.send_now.push_back(SegmentMeta {
                    seqn,
                    ackn: self.rx.curr_ackn(),
                    window: self.rx.window_size(),
                    flags: SegmentFlags::SYN | SegmentFlags::ACK,
                    data: Vec::new(),
                });
            }

            ConnectionState::SynSent => {
                let ack_acceptable = if seg.flags.contains(SegmentFlags::ACK) {
                    if seg.flags.contains(SegmentFlags::RST) {
                        return;
                    }

                    if seg.ackn <= self.tx.init_seqn || seg.ackn > self.tx.next {
                        self.tx.send_now.push_back(SegmentMeta {
                            seqn: seg.ackn,
                            ackn: 0,
                            window: self.rx.window_size(),
                            flags: SegmentFlags::RST,
                            data: Vec::new(),
                        });
                        return;
                    }

                    seg.ackn <= self.tx.unack || seg.ackn <= self.tx.next
                } else {
                    false
                };

                if seg.flags.contains(SegmentFlags::RST) {
                    if ack_acceptable {
                        self.clear();
                        self.reset(Error::ConnectionReset);
                    }
                    return;
                }

                if seg.flags.contains(SegmentFlags::SYN) {
                    self.rx.init(seg.seqn);
                    self.clear_re_tx_range(self.tx.unack, seg.ackn);
                    self.tx.unack = seg.ackn;
                    if self.tx.unack > self.tx.init_seqn {
                        // Our SYN has been ACK'd
                        self.set_state(ConnectionState::FULLY_OPEN);
                        self.send_ack();
                    } else {
                        todo!("If packet has data, queue it until ESTABLISHED");
                        self.set_state(ConnectionState::SynReceived);
                        self.tx.send_now.push_back(SegmentMeta {
                            seqn: self.tx.init_seqn,
                            ackn: self.rx.curr_ackn(),
                            window: self.rx.window_size(),
                            flags: SegmentFlags::SYN | SegmentFlags::ACK,
                            data: Vec::new(),
                        });
                    }
                }
            }
            ConnectionState::TimeWait => {
                if !self.check_segment_seq_ok(&seg) {
                    if !seg.flags.contains(SegmentFlags::RST) {
                        self.tx.send_now.push_back(SegmentMeta {
                            seqn: self.tx.next,
                            ackn: self.rx.curr_ackn(),
                            window: 0,
                            flags: SegmentFlags::ACK,
                            data: Vec::new(),
                        });
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
                    self.tx.send_now.push_back(SegmentMeta {
                        seqn: self.tx.next,
                        ackn: 0,
                        window: 0,
                        flags: SegmentFlags::RST,
                        data: Vec::new(),
                    });

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
                        self.tx.send_now.push_back(SegmentMeta {
                            seqn: self.tx.next,
                            ackn: self.rx.curr_ackn(),
                            window: self.rx.window_size(),
                            flags: SegmentFlags::ACK,
                            data: Vec::new(),
                        });
                    }
                    return;
                }

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
                    self.tx.send_now.push_back(SegmentMeta {
                        seqn: self.tx.next,
                        ackn: 0,
                        window: self.rx.window_size(),
                        flags: SegmentFlags::RST,
                        data: Vec::new(),
                    });

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
                        self.trigger(|w| {
                            if let WaitUntil::Recv { count } = w {
                                (count as usize) <= available
                            } else {
                                false
                            }
                        });

                        self.clear_re_tx_range(self.tx.unack, seg.ackn);
                        self.tx.unack = seg.ackn;
                        self.exp_backoff = 1;
                        self.timers.re_tx = None;

                        self.trigger(|until| {
                            if let WaitUntil::Acknowledged { seqn } = until {
                                seqn <= seg.ackn
                            } else {
                                false
                            }
                        });
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
                    self.trigger(|until| matches!(until, WaitUntil::Recv { .. }));

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
