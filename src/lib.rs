//! TCP as per https://www.ietf.org/rfc/rfc793.txt

#![cfg_attr(not(any(test, feature = "std")), no_std)]
#![feature(default_free_fn, duration_constants, let_else, drain_filter)]
#![allow(dead_code, unreachable_code)]
#![deny(unused_must_use)]
#![forbid(unsafe_code)]

extern crate alloc;

use core::time::Duration;

use alloc::collections::VecDeque;
use alloc::vec::Vec;

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
mod user_data;

use event::{Events, WaitUntil};
use options::*;
use rx::*;
use timers::*;
use tx::*;

pub use event::Cookie;
pub use options::SocketOptions;
pub use result::Error;
pub use rtt::Timings;
pub use segment::{SegmentFlags, SegmentMeta, SeqN};
pub use state::ConnectionState;
pub use user_data::{UserData, UserTime};

use crate::state::ChannelState;

/// Response to a packet when the socket is closed
/// RFC 793 page 64
pub fn response_to_closed(seg: SegmentMeta) -> Option<SegmentMeta> {
    if seg.flags.contains(SegmentFlags::RST) {
        None
    } else if seg.flags.contains(SegmentFlags::ACK) {
        Some(SegmentMeta {
            seqn: SeqN::ZERO,
            ackn: seg.seqn.wrapping_add(seg.data.len().try_into().unwrap()),
            window: 0,
            flags: SegmentFlags::RST | SegmentFlags::ACK,
            data: Vec::new(),
        })
    } else {
        Some(SegmentMeta {
            seqn: seg.ackn,
            ackn: SeqN::ZERO,
            window: 0,
            flags: SegmentFlags::RST,
            data: Vec::new(),
        })
    }
}

pub struct Socket<U: UserData> {
    remote: Option<U::Addr>,
    connection_state: ConnectionState,
    dup_ack_count: u8,
    congestation_window: u16,
    exp_backoff: u64,
    tx: TxBuffer,
    rx: RxBuffer,
    timings: Timings,
    timers: Timers<U::Time>,
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
            self.trigger_event(|w| w == WaitUntil::TxQueueEmpty, Ok(()));
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
        if let ConnectionState::Established {
            tx_state: ChannelState::Closed,
            rx_state: ChannelState::Closed,
            close_called: true,
            close_active,
        } = self.connection_state
        {
            if close_active {
                self.set_state(ConnectionState::TimeWait);
                self.timers.clear();
                self.set_timer_timewait(MAX_SEGMENT_LIFETIME * 2);
            } else {
                self.clear();
            }
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

    /// RFC793, page 26 and 68
    fn check_segment_seq_ok(&self, seg: &SegmentMeta) -> bool {
        if self.rx.curr_window() == 0 {
            seg.seq_size() == 0 && seg.seqn == self.rx.ackd
        } else {
            self.rx.in_window(seg.seqn)
                && if seg.seq_size() == 0 {
                    true
                } else {
                    self.rx
                        .in_window(seg.seqn.wrapping_add(seg.seq_size() as u32).wrapping_sub(1))
                }
        }
    }

    /// Remove ACK'd segments from re_tx queue
    fn clear_re_tx_range(&mut self, end: SeqN) {
        log::trace!("Clearing re_tx buffer until {}", end);

        // Trigger events
        self.trigger_event(
            |w| {
                if let WaitUntil::Acknowledged { seqn } = w {
                    seqn.lte(end)
                } else {
                    false
                }
            },
            Ok(()),
        );

        // Clear queue
        loop {
            let Some(head) = self.tx.re_tx.get(0) else {
                log::trace!("re_tx queue empty");
                return;
            };

            if !head.seqn_after().lte(end) {
                log::trace!(
                    "Not removing item from re_tx queue {:?}",
                    (end, head.seqn_after())
                );
                return;
            }

            log::trace!("Removing item from re_tx queue {:?}", head);
            let _ = self.tx.re_tx.pop_front();
        }
    }

    /// Establish a connection
    pub fn call_connect(&mut self, remote: U::Addr) -> Result<(), Error> {
        log::trace!("state: {:?}", self.connection_state);
        log::trace!("call_connect {:?}", remote);
        if matches!(
            self.connection_state,
            ConnectionState::Closed | ConnectionState::Reset
        ) {
            self.remote = Some(remote);
            self.set_state(ConnectionState::SynSent);
            self.tx.init(SeqN::new(self.user_data.new_seqn()));
            let seg = SegmentMeta {
                seqn: self.tx.init_seqn,
                ackn: SeqN::ZERO,
                window: self.rx.curr_window(),
                flags: SegmentFlags::SYN,
                data: Vec::new(),
            };
            self.user_data.send(remote, seg.clone());
            self.tx.on_send(seg);
            self.set_timer_re_tx(self.timings.rto);
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
                close_active,
            } => match rx_state {
                ChannelState::Closed => Ok(0),
                ChannelState::Open => {
                    if self.rx.available_bytes() >= buffer.len() {
                        // Move data to caller buffer
                        let data = self.rx.read_bytes(buffer.len());
                        buffer.copy_from_slice(&data);

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
                            close_active,
                        });
                    }

                    Ok(data.len())
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
                self.trigger_event(|_| true, Err(Error::ConnectionClosing));
                self.clear();
                Ok(())
            }
            ConnectionState::SynReceived => {
                if self
                    .events
                    .any_suspended(|w| matches!(w, WaitUntil::Acknowledged { .. }))
                {
                    // Cannot send FIN yet, as some call_send are queued
                    // TODO: we could continue when the buffer has data but no calls are queued
                    log::trace!("Delaying shutdown until no more senders");
                    return self.events.return_retry_after(WaitUntil::TxQueueEmpty);
                }

                self.tx.tx.mark_fin();
                self.send_ack();

                self.set_state(ConnectionState::Established {
                    tx_state: ChannelState::Fin,
                    rx_state: ChannelState::Open,
                    close_called: false,
                    close_active: true,
                });

                Ok(())
            }
            ConnectionState::Established {
                tx_state,
                rx_state,
                close_called,
                ..
            } => match tx_state {
                ChannelState::Closed | ChannelState::Fin => Err(Error::ConnectionClosing),
                ChannelState::Open => {
                    if self.tx.tx.available_bytes() != 0
                        || self
                            .events
                            .any_suspended(|w| matches!(w, WaitUntil::Acknowledged { .. }))
                    {
                        // Cannot send FIN yet, as some call_send are queued
                        // TODO: we could continue when the buffer has data but no calls are queued
                        log::trace!("Delaying shutdown until no more senders");
                        return self.events.return_retry_after(WaitUntil::TxQueueEmpty);
                    }

                    if rx_state == ChannelState::Open {
                        self.set_state(ConnectionState::Established {
                            tx_state,
                            rx_state,
                            close_called,
                            close_active: true,
                        })
                    }
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
                self.trigger_event(
                    |w| matches!(w, WaitUntil::Recv { .. }),
                    Err(Error::ConnectionClosing),
                );
                self.clear();
                Ok(())
            }
            ConnectionState::SynReceived => {
                if self.tx.tx.available_bytes() != 0
                    || self
                        .events
                        .any_suspended(|w| matches!(w, WaitUntil::Acknowledged { .. }))
                {
                    // Cannot close socket yet, as some items are queued
                    log::trace!("Delaying close until no more senders");
                    return self.events.return_retry_after(WaitUntil::TxQueueEmpty);
                }

                self.tx.tx.mark_fin();
                self.send_ack();

                self.set_state(ConnectionState::Established {
                    tx_state: ChannelState::Fin,
                    rx_state: ChannelState::Open,
                    close_called: true,
                    close_active: true,
                });

                self.events.return_continue_after(WaitUntil::BothClosed)
            }
            ConnectionState::Established {
                tx_state,
                rx_state,
                mut close_active,
                ..
            } => {
                if rx_state == ChannelState::Open {
                    close_active = true;
                }

                if tx_state == ChannelState::Open {
                    if self.tx.tx.available_bytes() != 0
                        || self
                            .events
                            .any_suspended(|w| matches!(w, WaitUntil::Acknowledged { .. }))
                    {
                        // Cannot close socket yet, as some items are queued
                        log::trace!("Delaying close until no more senders");
                        return self.events.return_retry_after(WaitUntil::TxQueueEmpty);
                    }

                    self.tx.tx.mark_fin();
                    self.send_ack();
                }

                let tx_state = self.connection_state.tx();
                let rx_state = self.connection_state.rx();

                self.set_state(ConnectionState::Established {
                    tx_state,
                    rx_state,
                    close_called: true,
                    close_active,
                });

                match tx_state {
                    ChannelState::Open | ChannelState::Fin => {
                        return self.events.return_continue_after(WaitUntil::BothClosed);
                    }
                    ChannelState::Closed => {}
                }

                match rx_state {
                    ChannelState::Open | ChannelState::Fin => {
                        return self.events.return_continue_after(WaitUntil::BothClosed);
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
                self.trigger_event(|_| true, Err(Error::ConnectionReset));
                self.clear();
                Ok(())
            }
            ConnectionState::SynReceived | ConnectionState::Established { .. } => {
                self.user_data.send(
                    self.remote.expect("No remote set"),
                    SegmentMeta {
                        seqn: self.tx.next,
                        ackn: SeqN::ZERO,
                        window: self.rx.curr_window(),
                        flags: SegmentFlags::RST,
                        data: Vec::new(),
                    },
                );
                self.trigger_event(|_| true, Err(Error::ConnectionReset));
                self.clear();
                Ok(())
            }
            ConnectionState::TimeWait => {
                self.clear();
                Ok(())
            }
        }
    }

    /// Sends an ack packet, including any data form buffer if possible.
    fn send_ack(&mut self) {
        let limit = MAX_SEGMENT_SIZE.min(self.tx.space_available() as usize);

        let (tx_data, tx_fin) = if limit == 0 {
            (Vec::new(), false)
        } else {
            let (tx_data, tx_fin) = self.tx.tx.read_bytes(limit);
            if tx_data.len() == 0 {
                if !self
                    .events
                    .any_suspended(|w| matches!(w, WaitUntil::Acknowledged { .. }))
                {
                    self.trigger_event(|w| w == WaitUntil::TxQueueEmpty, Ok(()));
                }
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
                close_active: self.connection_state.close_active(),
            });
            SegmentFlags::FIN | SegmentFlags::ACK
        } else {
            SegmentFlags::ACK
        };

        let needs_resend = flags.contains(SegmentFlags::FIN) || !tx_data.is_empty();

        let seg = SegmentMeta {
            seqn: self.tx.next,
            ackn: self.rx.ackd,
            window: self.rx.curr_window(),
            flags,
            data: tx_data,
        };

        if seg.seq_size() == 0 && Some((seg.seqn, seg.ackn, seg.window)) == self.tx.last_sent {
            log::trace!("Skipping duplicate resend");
            return;
        }

        self.tx.on_send(seg.clone());
        self.user_data
            .send(self.remote.expect("No remote set"), seg);

        if needs_resend {
            self.set_timer_re_tx(self.timings.rto);
        }
    }

    /// Called on incoming segment
    /// See RFC 793 page 64
    pub fn on_segment(&mut self, seg: SegmentMeta) {
        log::trace!("on_segment {:?}", seg);
        log::trace!("state: {:?}", self.connection_state);

        match self.connection_state {
            ConnectionState::Closed | ConnectionState::Reset => {
                log::trace!("Socket closed, replying");
                if let Some(resp) = response_to_closed(seg) {
                    self.user_data
                        .send(self.remote.expect("No remote set"), resp);
                }
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
                            ackn: SeqN::ZERO,
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

                assert!(
                    self.remote.is_some(),
                    "Listener should have stored remote address"
                );
                self.rx.init(seg.seqn);
                self.tx.init(SeqN::new(self.user_data.new_seqn()));
                // TODO: queue data, control if any available
                assert!(seg.data.len() == 0, "TODO");
                self.tx.window = seg.window;
                self.set_state(ConnectionState::SynReceived);
                let seg = SegmentMeta {
                    seqn: self.tx.init_seqn,
                    ackn: self.rx.ackd,
                    window: self.rx.curr_window(),
                    flags: SegmentFlags::SYN | SegmentFlags::ACK,
                    data: Vec::new(),
                };

                self.set_timer_re_tx(self.timings.rto);
                self.tx.on_send(seg.clone());
                self.user_data
                    .send(self.remote.expect("No remote set"), seg);
            }

            ConnectionState::SynSent => {
                if seg.flags.contains(SegmentFlags::ACK) {
                    if !seg.ackn.in_range_inclusive(self.tx.unack, self.tx.next) {
                        self.user_data.send(
                            self.remote.expect("No remote set"),
                            SegmentMeta {
                                seqn: seg.ackn,
                                ackn: SeqN::ZERO,
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

                if seg.flags.contains(SegmentFlags::SYN | SegmentFlags::ACK) {
                    self.rx.init(seg.seqn);
                    self.clear_re_tx_range(seg.ackn);
                    self.tx.unack = seg.ackn;
                    self.tx.window = seg.window;
                    if self.tx.unack == self.tx.init_seqn.wrapping_add(1) {
                        log::trace!("Our SYN has been SYN-ACK'd");
                        self.set_state(ConnectionState::FULLY_OPEN);
                        self.send_ack();
                    } else {
                        // TODO: cache out-of-order data packets
                        log::trace!("Invalid SYN-ACK sequence number");
                    }
                }
            }
            ConnectionState::TimeWait
            | ConnectionState::SynReceived
            | ConnectionState::Established { .. } => {
                if !self.check_segment_seq_ok(&seg) {
                    log::trace!("Non-acceptable segment {:#?}", seg);
                    if !seg.flags.contains(SegmentFlags::RST) {
                        self.user_data.send(
                            self.remote.expect("No remote set"),
                            SegmentMeta {
                                seqn: self.tx.next,
                                ackn: self.rx.ackd,
                                window: self.rx.curr_window(),
                                flags: SegmentFlags::ACK,
                                data: Vec::new(),
                            },
                        );
                    }
                    return;
                }

                self.tx.window = seg.window.min(self.congestation_window);

                if seg.flags.contains(SegmentFlags::RST) {
                    match self.connection_state {
                        ConnectionState::SynReceived => {
                            self.clear();
                            self.set_state(ConnectionState::Listen);
                        }
                        ConnectionState::Established { .. } => {
                            self.trigger_event(|_| true, Err(Error::ConnectionReset));
                            self.clear();
                            self.set_state(ConnectionState::Reset);
                        }
                        _ => unreachable!(),
                    }
                    return;
                }

                if seg.flags.contains(SegmentFlags::SYN) {
                    log::warn!("Peer error: SYN");

                    self.trigger_event(|_| true, Err(Error::ConnectionReset));

                    // Error! Send reset, flush all queues
                    self.user_data.send(
                        self.remote.expect("No remote set"),
                        SegmentMeta {
                            seqn: self.tx.next,
                            ackn: SeqN::ZERO,
                            window: self.rx.curr_window(),
                            flags: SegmentFlags::RST,
                            data: Vec::new(),
                        },
                    );

                    self.clear();
                    self.set_state(ConnectionState::Reset);
                    return;
                }

                let flags = seg.flags;
                let is_non_ordered_ack = if seg.flags.contains(SegmentFlags::ACK) {
                    self.tx.is_non_ordered_ack(seg.ackn)
                } else {
                    false
                };

                if seg.flags.contains(SegmentFlags::ACK) {
                    if self.connection_state == ConnectionState::SynReceived {
                        if seg.ackn.in_range_inclusive(self.tx.unack, self.tx.next) {
                            self.set_state(ConnectionState::FULLY_OPEN);
                        }
                    }

                    let ack_valid = seg.ackn.in_range_inclusive(self.tx.unack, self.tx.next);
                    if !ack_valid {
                        log::warn!(
                            "Dropping invalid ACK {:?}",
                            (self.tx.unack, seg.ackn, self.tx.next)
                        );
                        return;
                    }

                    if self.tx.unack != seg.ackn {
                        // New valid ACK

                        // Window update subroutine
                        if self.dup_ack_count > 0 {
                            self.congestation_window = SLOW_START_TRESHOLD;
                        } else if self.congestation_window <= SLOW_START_TRESHOLD {
                            self.congestation_window *= 2;
                        } else {
                            self.congestation_window += seg.data.len() as u16; // TODO: checked conversion
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

                        self.clear_re_tx_range(seg.ackn);
                        self.tx.unack = seg.ackn;
                        self.exp_backoff = 1;
                        if self.tx.re_tx.is_empty() {
                            self.timers.re_tx = None;
                        } else {
                            self.set_timer_re_tx(self.timings.rto);
                        }

                        self.trigger_event(
                            |until| {
                                if let WaitUntil::Acknowledged { seqn } = until {
                                    seqn.lte(seg.ackn)
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
                        close_active,
                    } = self.connection_state
                    {
                        if tx_state == ChannelState::Fin {
                            if seg.ackn == self.tx.next {
                                log::trace!("ACK for our FIN received");
                                debug_assert!(self.tx.tx.is_empty());
                                debug_assert!(self.tx.re_tx.is_empty());
                                self.set_state(ConnectionState::Established {
                                    tx_state: ChannelState::Closed,
                                    rx_state,
                                    close_called,
                                    close_active,
                                });
                            }
                        }
                    }

                    // Store segment data
                    if seg.seqn == self.rx.ackd {
                        self.rx.write(seg);
                        self.send_ack();
                    } else if seg.seqn.lt(self.rx.ackd) {
                        log::trace!("Past data, re-ack and discard");
                        let seg = SegmentMeta {
                            seqn: self.tx.next,
                            ackn: self.rx.ackd,
                            window: self.rx.curr_window(),
                            flags: SegmentFlags::ACK,
                            data: Vec::new(),
                        };
                        self.user_data
                            .send(self.remote.expect("No remote set"), seg);
                        return;
                    } else {
                        log::trace!("out-of-order {:?} != {:?}", seg.seqn, self.rx.ackd);
                        // TODO: cache out-of-order data for later
                        return;
                    }
                }

                if matches!(
                    self.connection_state,
                    ConnectionState::Closed | ConnectionState::Listen | ConnectionState::SynSent
                ) {
                    return;
                }

                if flags.contains(SegmentFlags::FIN) {
                    log::trace!("Got FIN packet");

                    if self.connection_state == ConnectionState::TimeWait {
                        if is_non_ordered_ack {
                            log::trace!("Got FIN packet");
                            self.trigger_event(|_| true, Err(Error::ConnectionClosing));
                            self.timers.clear();
                            self.set_timer_timewait(MAX_SEGMENT_LIFETIME * 2);
                        }
                    } else {
                        // Retry all recvs, which will not return ConnectionClosing after the buffer is empty
                        self.trigger_event(|until| matches!(until, WaitUntil::Recv { .. }), Ok(()));
                    }

                    match self.connection_state {
                        ConnectionState::SynReceived => {
                            self.set_state(ConnectionState::Established {
                                tx_state: ChannelState::Open,
                                rx_state: ChannelState::Fin,
                                close_called: false,
                                close_active: false,
                            });
                        }
                        ConnectionState::Established {
                            tx_state,
                            rx_state,
                            close_called,
                            close_active,
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
                                close_active,
                            });
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}

pub struct ListenSocket<U: UserData> {
    queue: VecDeque<(U::Addr, SegmentMeta)>,
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
                    ackn: SeqN::ZERO,
                    window: 0,
                    flags: SegmentFlags::RST,
                    data: Vec::new(),
                },
            );
        }
    }

    /// Accept a new connection from the backlog
    pub fn call_accept<D: FnOnce() -> U>(
        &mut self,
        make_user_data: D,
    ) -> Result<(U::Addr, Socket<U>), Error> {
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
    pub fn on_segment(&mut self, addr: U::Addr, seg: SegmentMeta) {
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
                    ackn: SeqN::ZERO,
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
                    ackn: SeqN::ZERO,
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
