#![cfg_attr(not(any(test, feature = "std")), no_std)]
#![feature(default_free_fn, duration_constants, let_else)]
#![allow(dead_code, unreachable_code, unused)]

#[macro_use]
extern crate alloc;

use core::sync::atomic::{AtomicU64, Ordering};
use core::time::Duration;

use alloc::collections::VecDeque;
use alloc::vec::Vec;

mod event;
mod result;
mod segment;

pub use result::{Error, Res};
pub use segment::{SegmentFlags, SegmentMeta};

#[cfg(feature = "std")]
mod log {
    pub use std::println as error;
    pub use std::println as warn;
    pub use std::println as info;
    pub use std::println as debug;
    pub use std::println as trace;
}

// TODO: Return Error vs Signal User
// TODO: Window scaling
// TODO: Wrapping comparisons with windows?

///// MOCKS //////////////////////////////

static SEQ: AtomicU64 = AtomicU64::new(10_000);

fn random_seqnum() -> u32 {
    SEQ.fetch_add(500, Ordering::AcqRel) as u32
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoteAddr;

static NOW: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Instant(u64);
impl Instant {
    pub fn now() -> Self {
        Self(NOW.fetch_add(1, Ordering::AcqRel) << 32)
    }
    pub fn add(self, duration: Duration) -> Self {
        let m = duration.as_millis();
        assert!(m < u32::MAX as u128);
        Self(self.0 + m as u64)
    }
}

///// END OF MOCKS ///////////////////////

const MAX_RTO: Duration = Duration::from_secs(120);
const NAGLE_DELAY_DEFAULT: Duration = Duration::from_millis(200);

// TODO: move these constants to a config parameter?

const INITIAL_WINDOW_SIZE: u16 = 0x1000;
const MAX_SEGMENT_SIZE: usize = 512;
const SLOW_START_TRESHOLD: u16 = 512;

#[derive(Debug, Clone)]
pub struct SocketOptions {
    /// Set to Duration::ZERO for TCP_NODELAY
    pub nagle_delay: core::time::Duration,
}
impl Default for SocketOptions {
    fn default() -> Self {
        Self {
            nagle_delay: NAGLE_DELAY_DEFAULT,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SendSeq {
    unack: u32,
    next: u32,
    window: u16,
    init_seqn: u32,
}
impl SendSeq {
    /// Next index is within unack window
    fn unack_has_space(&self) -> bool {
        // TODO: handle wrapping
        self.next < self.unack + (self.window as u32)
    }
}

#[derive(Debug, Clone)]
pub struct RecvSeq {
    next: u32,
    window: u16,
    init_seqn: u32,
}
impl RecvSeq {
    fn in_window(&self, seqn: u32) -> bool {
        // TODO: handle wrapping
        self.next <= seqn && seqn < (self.next + (self.window as u32))
    }
}
impl Default for RecvSeq {
    fn default() -> Self {
        Self {
            next: 0,
            window: INITIAL_WINDOW_SIZE,
            init_seqn: 0,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Buffers {
    /// Output data stream buffer
    tx: VecDeque<Vec<u8>>,
    /// Retransmit queue
    /// Invariant: ordered
    re_tx: VecDeque<SegmentMeta>,
    /// Out-of-order received packets
    oo_rx: VecDeque<SegmentMeta>,
    /// Data ready for reading
    rx: VecDeque<Vec<u8>>,
    /// Packets to be sent after the call
    send_now: VecDeque<SegmentMeta>,
}

fn duration_absdiff(a: Duration, b: Duration) -> Duration {
    if a > b { a - b } else { b - a }
}

/// Timings per https://datatracker.ietf.org/doc/html/rfc6298
#[derive(Debug, Clone)]
pub struct Timings {
    /// Retransmission timeout
    rto: Duration,
    /// Smoothed round-trip time
    srtt: Duration,
    /// RTT variation
    rtt_var: Duration,
}
impl Timings {
    pub fn update_rtt(&mut self, rtt: Duration) {
        if self.srtt == Duration::ZERO {
            self.srtt = rtt;
            self.rtt_var = rtt / 2;
        } else {
            self.rtt_var = (3 * self.rtt_var + duration_absdiff(self.srtt, rtt)) / 4;
            self.srtt = (7 * self.srtt + rtt) / 8;
            let clock_granularity = Duration::ZERO; // TODO
            self.rto = (self.srtt + (4 * self.rtt_var).max(clock_granularity))
                .min(Duration::SECOND)
                .max(MAX_RTO);
        }
    }
}
impl Default for Timings {
    fn default() -> Self {
        Self {
            rto: Duration::SECOND,
            srtt: Duration::ZERO,
            rtt_var: Duration::ZERO,
        }
    }
}

/// Timings per https://datatracker.ietf.org/doc/html/rfc6298
#[derive(Debug, Clone, Default)]
pub struct Timers {
    re_tx: Option<Instant>,
    timewait: Option<Instant>,
    usertime: Option<Instant>,
}

#[derive(Debug, Clone)]
pub struct Socket {
    connection_state: ConnectionState,
    remote_address: Option<RemoteAddr>,
    dup_ack_count: u8,
    congestation_window: u16,
    exp_backoff: u16,
    tx_seq: SendSeq,
    rx_seq: RecvSeq,
    buffers: Buffers,
    timings: Timings,
    timers: Timers,
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
            tx_seq: SendSeq::default(),
            rx_seq: RecvSeq::default(),
            buffers: Buffers::default(),
            timings: Timings::default(),
            timers: Timers::default(),
            options: SocketOptions::default(),
        }
    }

    pub fn clear(&mut self) {
        *self = Self::new();
    }

    fn check_segment_seq_ok(&self, seg: &SegmentMeta) -> bool {
        if self.rx_seq.window == 0 {
            seg.data.len() == 0 && seg.seqn == self.rx_seq.next
        } else {
            self.rx_seq.in_window(seg.seqn)
                && if seg.data.len() == 0 {
                    true
                } else {
                    self.rx_seq
                        .in_window(seg.seqn + (seg.data.len() as u32) - 1)
                }
        }
    }

    /// Remove ACK'd segments from re_tx queue
    fn clear_re_tx_range(&mut self, start: u32, end: u32) {
        loop {
            let Some(head) = self.buffers.re_tx.get(0) else {
                return;
            };

            let start_ok = start <= head.ackn; // TODO: omit, wrap around
            let end_ok = head.ackn + (head.data.len() as u32) <= end;
            if !start_ok || !end_ok {
                return;
            }

            let _ = self.buffers.re_tx.pop_front();
        }
    }

    fn tx_buf_len(&self) -> usize {
        self.buffers.tx.iter().map(|b| b.len()).sum()
    }

    /// Takes up to `limit` bytes of input
    fn tx_buf_take(&mut self, limit: usize) -> Vec<u8> {
        let mut result = Vec::new();

        while let Some(value) = self.buffers.tx.pop_front() {
            result.extend(value);
            if result.len() >= limit {
                let rest = result.split_off(limit);
                self.buffers.tx.push_front(rest);
                return result;
            }
        }

        result
    }

    fn rx_buf_len(&self) -> usize {
        self.buffers.rx.iter().map(|b| b.len()).sum()
    }

    /// Takes up to `limit` bytes of input
    fn rx_buf_take(&mut self, limit: usize) -> Vec<u8> {
        let mut result = Vec::new();

        while let Some(value) = self.buffers.rx.pop_front() {
            result.extend(value);
            if result.len() >= limit {
                let rest = result.split_off(limit);
                self.buffers.rx.push_front(rest);
                return result;
            }
        }

        result
    }

    /// Establish a connection
    pub fn call_connect(&mut self, remote: RemoteAddr) -> Res {
        dbg!(self.connection_state);
        log::trace!("call_connect {:?}", remote);
        if self.connection_state == ConnectionState::Closed {
            self.remote_address = Some(remote);
            self.connection_state = ConnectionState::SynSent;
            self.tx_seq.init_seqn = random_seqnum();
            self.tx_seq.unack = self.tx_seq.init_seqn;
            self.tx_seq.next = self.tx_seq.init_seqn + 1;
            self.buffers.send_now.push_back(SegmentMeta {
                seqn: self.tx_seq.init_seqn,
                ackn: 0,
                window: self.rx_seq.window,
                flags: SegmentFlags::SYN,
                data: Vec::new(),
            });
            Res::empty()
        } else {
            Res::error(Error::InvalidStateTransition)
        }
    }

    /// Listen for connection
    pub fn call_listen(&mut self) -> Res {
        dbg!(self.connection_state);
        log::trace!("call_listen");
        if self.connection_state == ConnectionState::Closed {
            self.connection_state = ConnectionState::Listen;
            Res::empty()
        } else {
            Res::error(Error::InvalidStateTransition)
        }
    }

    /// Send some data
    pub fn call_send(&mut self, input_data: Vec<u8>) -> Res {
        dbg!(self.connection_state);
        log::trace!("call_send {:?}", input_data);
        match self.connection_state {
            ConnectionState::Listen => Res::error(Error::NotConnected),
            ConnectionState::SynSent | ConnectionState::SynReceived => {
                todo!("queue until ESTABLISHED");
            },
            ConnectionState::Established | ConnectionState::CloseWait => {
                self.buffers.tx.push_back(input_data);
                log::trace!("Cond1? {:?}", (self.tx_seq.unack_has_space()));
                if self.tx_seq.unack_has_space() {
                    // TODO: sub MAX_SEGMENT_SIZE segment header
                    // TODO: condition
                    log::trace!(
                        "Cond2? {:?}",
                        (
                            self.tx_buf_len() >= MAX_SEGMENT_SIZE,
                            self.options.nagle_delay == Duration::ZERO
                        )
                    );
                    if self.tx_buf_len() >= MAX_SEGMENT_SIZE
                        || self.options.nagle_delay == Duration::ZERO
                    {
                        let data_to_send = self.tx_buf_take(MAX_SEGMENT_SIZE);
                        let len = data_to_send.len();
                        self.tx_seq.next = self.tx_seq.next.wrapping_add(len as u32); // XXX: is this before or after creating seg?
                        log::trace!("Sending packet with data {:?}", data_to_send);
                        let seg = SegmentMeta {
                            seqn: self.tx_seq.next,
                            ackn: self.rx_seq.next,
                            window: self.rx_seq.window,
                            flags: SegmentFlags::ACK,
                            data: data_to_send,
                        };
                        self.buffers.re_tx.push_back(seg.clone());
                        self.buffers.send_now.push_back(seg);
                        self.timers.re_tx = Some(Instant::now().add(self.timings.rto));
                        // Piggybacked ACK in ESTABLISHED
                        // https://en.wikipedia.org/wiki/Piggybacking_(data_transmission)
                        return Res::empty();
                    }
                }
                Res::empty()
            },
            ConnectionState::FinWait1
            | ConnectionState::FinWait2
            | ConnectionState::Closing
            | ConnectionState::TimeWait
            | ConnectionState::LastAck => Res::error(Error::ConnectionClosing),
            ConnectionState::Closed => Res::error(Error::NotConnected),
        }
    }

    /// Receive some data, if any available
    pub fn call_recv(&mut self, buffer: &mut [u8]) -> Res<usize> {
        dbg!(self.connection_state);
        log::trace!("call_recv len={}", buffer.len());
        match self.connection_state {
            ConnectionState::Listen | ConnectionState::SynSent | ConnectionState::SynReceived => {
                todo!("queue until ESTABLISHED");
            },
            ConnectionState::Established => {
                if self.rx_buf_len() >= buffer.len() {
                    let data = self.rx_buf_take(buffer.len());
                    buffer.copy_from_slice(&data);
                    Res::value(data.len())
                } else {
                    todo!("QUEUE reg");
                }
            },
            ConnectionState::FinWait1 | ConnectionState::FinWait2 => {
                let has_data = todo!("queue_has_enough_data");
                // if !(has_data) {
                //     todo!("QUEUE reg");
                //     return Res::empty();
                // }

                todo!("Reassemble");
                todo!("Signal user for data");
                Res::empty()
            },
            ConnectionState::CloseWait => {
                if todo!("data waiting to be delivered to the user") {
                    todo!("Reassemble remaining data into RCV buffer");
                    todo!("Signal user to read recv buffer");
                    Res::empty()
                } else {
                    Res::error(Error::ConnectionClosing)
                }
            },
            ConnectionState::Closing | ConnectionState::TimeWait | ConnectionState::LastAck => {
                Res::error(Error::ConnectionClosing)
            },
            ConnectionState::Closed => Res::error(Error::NotConnected),
        }
    }

    /// Request closing the socket
    pub fn call_close(&mut self) -> Res {
        dbg!(self.connection_state);
        log::trace!("call_close");
        match self.connection_state {
            ConnectionState::Listen | ConnectionState::SynSent => {
                let has_queued_recvs = todo!("RECV queue");
                if has_queued_recvs {
                    todo!("Send ConnectionClosing to the RECVs");
                }
                self.clear();
                Res::empty()
            },
            ConnectionState::SynReceived => {
                let new_sends = todo!("New sends");
                let queued_sends = todo!("Queued sends");
                if new_sends || queued_sends {
                    // Cannot close socket yet, as some items are queued
                    todo!("Queue sends");
                } else {
                    self.connection_state = ConnectionState::FinWait1;
                    self.buffers.send_now.push_back(SegmentMeta {
                        seqn: self.tx_seq.next,
                        ackn: 0,
                        window: self.rx_seq.window,
                        flags: SegmentFlags::FIN,
                        data: Vec::new(),
                    });
                    Res::empty()
                }
            },
            ConnectionState::Established => {
                let queued_sends = todo!("Queued sends");
                if queued_sends {
                    // Cannot close socket yet, as some items are queued
                    todo!("Queue close until all sent");
                }

                self.connection_state = ConnectionState::FinWait1;
                self.buffers.send_now.push_back(SegmentMeta {
                    seqn: self.tx_seq.next,
                    ackn: 0,
                    window: self.rx_seq.window,
                    flags: SegmentFlags::FIN,
                    data: Vec::new(),
                });
                Res::empty()
            },
            ConnectionState::FinWait1
            | ConnectionState::FinWait2
            | ConnectionState::Closing
            | ConnectionState::TimeWait
            | ConnectionState::LastAck => Res::error(Error::ConnectionClosing),
            ConnectionState::CloseWait => {
                if todo!("any queued SENDs") {
                    todo!("queue this close until all sent");
                }
                self.connection_state = ConnectionState::LastAck;
                self.buffers.send_now.push_back(SegmentMeta {
                    seqn: self.tx_seq.next,
                    ackn: 0,
                    window: self.rx_seq.window,
                    flags: SegmentFlags::FIN,
                    data: Vec::new(),
                });
                Res::empty()
            },
            ConnectionState::Closed => Res::error(Error::NotConnected),
        }
    }

    /// Request closing the socket
    pub fn call_abort(&mut self) -> Res {
        dbg!(self.connection_state);
        log::trace!("call_abort");
        match self.connection_state {
            ConnectionState::Listen | ConnectionState::SynSent => {
                let has_queued_recvs = todo!("RECV queue");
                if has_queued_recvs {
                    todo!("Send ConnectionReset to the RECVs");
                }
                self.clear();
                Res::empty()
            },
            ConnectionState::SynReceived
            | ConnectionState::Established
            | ConnectionState::FinWait1
            | ConnectionState::FinWait2
            | ConnectionState::CloseWait => {
                self.buffers.send_now.push_back(SegmentMeta {
                    seqn: self.tx_seq.next,
                    ackn: 0,
                    window: self.rx_seq.window,
                    flags: SegmentFlags::RST,
                    data: Vec::new(),
                });
                let mut r = Res::empty();

                let queued_sends = todo!("Queued sends");
                let queued_recvs = todo!("Queued recvs");
                if queued_sends || queued_recvs {
                    r.error = Some(Error::ConnectionReset);
                }

                self.clear();
                r
            },
            ConnectionState::Closing | ConnectionState::LastAck | ConnectionState::TimeWait => {
                self.clear();
                Res::empty()
            },
            ConnectionState::Closed => Res::error(Error::NotConnected),
        }
    }

    /// Called on incoming segment
    pub fn on_segment(&mut self, seg: SegmentMeta) -> Res {
        dbg!(self.connection_state);
        log::trace!("on_segment {:?}", seg);

        self.tx_seq.window = seg.window.min(self.congestation_window);

        match self.connection_state {
            ConnectionState::Listen => {
                if seg.flags.contains(SegmentFlags::RST) {
                    todo!();
                } else if seg.flags.contains(SegmentFlags::ACK) {
                    self.buffers.send_now.push_back(SegmentMeta {
                        seqn: seg.ackn,
                        ackn: 0,
                        window: self.rx_seq.window,
                        flags: SegmentFlags::RST,
                        data: Vec::new(),
                    });
                    Res::empty()
                } else if !seg.flags.contains(SegmentFlags::SYN) {
                    todo!();
                } else {
                    // SYN packet
                    self.rx_seq.next = seg.seqn.wrapping_add(1);
                    self.rx_seq.init_seqn = seg.seqn;
                    let seqn = random_seqnum();
                    self.tx_seq.next = seqn.wrapping_add(1);
                    self.tx_seq.unack = seqn;
                    self.connection_state = ConnectionState::SynReceived;
                    self.buffers.send_now.push_back(SegmentMeta {
                        seqn,
                        ackn: self.rx_seq.next,
                        window: self.rx_seq.window,
                        flags: SegmentFlags::SYN | SegmentFlags::ACK,
                        data: Vec::new(),
                    });
                    Res::empty()
                }
            },
            ConnectionState::SynSent => {
                if seg.flags.contains(SegmentFlags::ACK) {
                    let cond1 = seg.ackn <= self.tx_seq.init_seqn || seg.ackn > self.tx_seq.next;
                    let cond2 = seg.ackn <= self.tx_seq.unack || seg.ackn <= self.tx_seq.next;

                    if cond1 || (!cond2) {
                        // ACK not acceptable
                        self.buffers.send_now.push_back(SegmentMeta {
                            seqn: seg.ackn,
                            ackn: 0,
                            window: self.rx_seq.window,
                            flags: SegmentFlags::RST,
                            data: Vec::new(),
                        });
                        Res::empty()
                    } else if seg.flags.contains(SegmentFlags::RST) {
                        self.clear();
                        return Res::error(Error::ConnectionReset);
                    } else if seg.flags.contains(SegmentFlags::SYN) {
                        self.rx_seq.next = seg.seqn.wrapping_add(1);
                        self.rx_seq.init_seqn = seg.seqn;
                        self.clear_re_tx_range(self.tx_seq.unack, seg.ackn);
                        self.tx_seq.unack = seg.ackn;
                        if self.tx_seq.unack <= self.tx_seq.init_seqn {
                            todo!("If packet has data, queue it until ESTABLISHED");
                            self.connection_state = ConnectionState::SynReceived;
                            self.buffers.send_now.push_back(SegmentMeta {
                                seqn: self.tx_seq.init_seqn,
                                ackn: self.rx_seq.next,
                                window: self.rx_seq.window,
                                flags: SegmentFlags::ACK,
                                data: Vec::new(),
                            });
                            Res::empty()
                        } else {
                            dbg!("EXEC");
                            // Our SYN has been ACK'd
                            self.connection_state = ConnectionState::Established;
                            // TODO: optimization: include queued data or controls here
                            self.buffers.send_now.push_back(SegmentMeta {
                                seqn: self.tx_seq.next,
                                ackn: self.rx_seq.next,
                                window: self.rx_seq.window,
                                flags: SegmentFlags::ACK,
                                data: Vec::new(),
                            });
                            Res::empty()
                        }
                    } else {
                        Res::empty()
                    }
                } else if seg.flags.contains(SegmentFlags::RST) {
                    Res::empty()
                } else if seg.flags.contains(SegmentFlags::SYN) {
                    todo!("If packet has data, queue it until ESTABLISHED");
                    self.connection_state = ConnectionState::SynReceived;
                    self.buffers.send_now.push_back(SegmentMeta {
                        seqn: self.tx_seq.init_seqn,
                        ackn: self.rx_seq.next,
                        window: self.rx_seq.window,
                        flags: SegmentFlags::ACK,
                        data: Vec::new(),
                    });
                    Res::empty()
                } else {
                    Res::empty()
                }
            },
            ConnectionState::SynReceived => {
                if self.check_segment_seq_ok(&seg) {
                    if seg.flags.contains(SegmentFlags::RST) {
                        // TODO: prevstate ?= LISTEN
                        todo!("clear rexmt queue");
                        self.connection_state = ConnectionState::Listen;
                        Res::empty()
                    } else if seg.flags.contains(SegmentFlags::SYN) {
                        log::warn!("Peer error: SYN");
                        // Error! Send reset, flush all queues
                        self.buffers.send_now.push_back(SegmentMeta {
                            seqn: self.tx_seq.next,
                            ackn: 0,
                            window: self.rx_seq.window,
                            flags: SegmentFlags::RST,
                            data: Vec::new(),
                        });
                        let mut r = Res::empty();
                        let queued_sends = todo!("Queued sends");
                        let queued_recvs = todo!("Queued recvs");
                        if queued_sends || queued_recvs {
                            r.error = Some(Error::ConnectionReset);
                        }
                        self.clear();
                        r
                    } else if seg.flags.contains(SegmentFlags::ACK) {
                        dbg!(self.tx_seq.unack, seg.ackn, seg.ackn, self.tx_seq.next);
                        if self.tx_seq.unack <= seg.ackn && seg.ackn <= self.tx_seq.next {
                            self.connection_state = ConnectionState::Established;
                            self.on_segment(seg) // recurse
                        } else if seg.flags.contains(SegmentFlags::FIN) {
                            todo!("FIN BIT PROCESSING");
                            self.connection_state = ConnectionState::CloseWait;
                            Res::empty()
                        } else {
                            log::warn!("Peer error");
                            todo!("Peer error");
                            // Error! Send a reset and flush all seg queues
                            self.buffers.send_now.push_back(SegmentMeta {
                                seqn: self.tx_seq.next,
                                ackn: 0,
                                window: self.rx_seq.window,
                                flags: SegmentFlags::RST,
                                data: Vec::new(),
                            });
                            let mut r = Res::empty();
                            let queued_sends = todo!("Queued sends");
                            let queued_recvs = todo!("Queued recvs");
                            if queued_sends || queued_recvs {
                                r.error = Some(Error::ConnectionReset);
                            }
                            self.clear();
                            r
                        }
                    } else {
                        // ACK bit is off! Drop segment and return
                        Res::empty()
                    }
                } else if seg.flags.contains(SegmentFlags::RST) {
                    Res::empty()
                } else {
                    self.buffers.send_now.push_back(SegmentMeta {
                        seqn: self.tx_seq.next,
                        ackn: self.rx_seq.next,
                        window: self.rx_seq.window,
                        flags: SegmentFlags::ACK,
                        data: Vec::new(),
                    });
                    Res::empty()
                }
            },
            ConnectionState::Established => {
                if self.check_segment_seq_ok(&seg) {
                    log::trace!("Established check_segment_seq_ok");
                    if seg.flags.contains(SegmentFlags::RST) {
                        let queued_sends = todo!("Queued sends");
                        let queued_recvs = todo!("Queued recvs");
                        let mut r = Res::empty();
                        if queued_sends || queued_recvs {
                            r.error = Some(Error::ConnectionReset);
                        }
                        self.clear();
                        r
                    } else if seg.flags.contains(SegmentFlags::SYN) {
                        let mut r = self.buffers.send_now.push_back(SegmentMeta {
                            seqn: self.tx_seq.next,
                            ackn: self.rx_seq.next,
                            window: self.rx_seq.window,
                            flags: SegmentFlags::ACK,
                            data: Vec::new(),
                        });
                        let mut r = Res::empty();
                        let queued_sends = todo!("Queued sends");
                        let queued_recvs = todo!("Queued recvs");
                        if queued_sends || queued_recvs {
                            r.error = Some(Error::ConnectionReset);
                        }
                        self.clear();
                        r
                    // <ES2>
                    } else if seg.flags.contains(SegmentFlags::ACK) {
                        if self.tx_seq.unack < seg.ackn && seg.ackn <= self.tx_seq.next {
                            // New valid ACK

                            // Window update subroutine
                            if self.dup_ack_count > 0 {
                                self.congestation_window = SLOW_START_TRESHOLD;
                            } else if self.congestation_window <= SLOW_START_TRESHOLD {
                                self.congestation_window *= 2;
                            } else {
                                self.congestation_window += seg.data.len() as u16; // TODO: cast better
                            }

                            // End of window update
                            self.tx_seq.window = seg.window.min(self.congestation_window);

                            self.clear_re_tx_range(self.tx_seq.unack, seg.ackn);
                            self.tx_seq.unack = seg.ackn;
                            self.exp_backoff = 1;
                            self.timers.re_tx = None;

                            let has_fin = seg.flags.contains(SegmentFlags::FIN);

                            // <ES3>
                            let r = if seg.seqn == self.rx_seq.next {
                                // Add seg to user-readable buffer
                                let len = seg.data.len();
                                self.buffers.rx.push_back(seg.data);
                                self.rx_seq.next = seg.seqn.wrapping_add(len as u32);

                                // Process out-of-order packets
                                if !self.buffers.oo_rx.is_empty() {
                                    todo!("out-of-order packets");
                                }
                                // todo!("signal: Data readable");
                                self.buffers.send_now.push_back(SegmentMeta {
                                    seqn: self.tx_seq.next,
                                    ackn: self.rx_seq.next,
                                    window: self.rx_seq.window,
                                    flags: SegmentFlags::ACK,
                                    data: Vec::new(),
                                });
                                Res::empty()
                            } else {
                                let len = seg.data.len();
                                self.buffers.oo_rx.push_back(seg);
                                self.rx_seq.window += len as u16;
                                self.buffers.send_now.push_back(SegmentMeta {
                                    seqn: self.tx_seq.next,
                                    ackn: self.rx_seq.next,
                                    window: self.rx_seq.window,
                                    flags: SegmentFlags::ACK,
                                    data: Vec::new(),
                                });
                                Res::empty()
                            };

                            if has_fin {
                                todo!("FIN bit processing");
                                self.connection_state = ConnectionState::CloseWait;
                            }
                            r
                            // </ES3>
                        } else if seg.ackn != self.tx_seq.unack {
                            // Invalid ACK, drop!
                            log::warn!("Dropping packet with invalid ACK value");
                            Res::empty()
                        } else {
                            // Duplicate ACK
                            self.dup_ack_count += 1;
                            if self.dup_ack_count > 2 {
                                // Fast recovery
                                todo!("Fast recovery");
                                // return self.fast_recovery();
                            } else if self.dup_ack_count == 2 {
                                // Fast retransmit
                                todo!("Fast retransmit");
                                // self.timers.re_tx = None;
                            }
                            // self.handle_received();
                            todo!("ES3");
                        }
                    } else {
                        // ACK bit is off! Drop segment and return
                        log::warn!("Invalid packet, ACK is off");
                        Res::empty()
                    }
                } else if seg.flags.contains(SegmentFlags::RST) {
                    Res::empty()
                } else {
                    self.buffers.send_now.push_back(SegmentMeta {
                        seqn: self.tx_seq.next,
                        ackn: self.rx_seq.next,
                        window: self.rx_seq.window,
                        flags: SegmentFlags::ACK,
                        data: Vec::new(),
                    });
                    Res::empty()
                }
            },
            ConnectionState::FinWait1 => {
                if self.check_segment_seq_ok(&seg) {
                    if seg.flags.contains(SegmentFlags::RST)
                        || seg.flags.contains(SegmentFlags::SYN)
                    {
                        let mut r = self.buffers.send_now.push_back(SegmentMeta {
                            seqn: self.tx_seq.next,
                            ackn: 0,
                            window: self.rx_seq.window,
                            flags: SegmentFlags::RST,
                            data: Vec::new(),
                        });

                        let mut r = Res::empty();
                        let queued_sends = todo!("Queued sends");
                        let queued_recvs = todo!("Queued recvs");
                        if queued_sends || queued_recvs {
                            r.error = Some(Error::ConnectionReset);
                        }

                        self.clear();
                        r
                    } else if seg.flags.contains(SegmentFlags::ACK) {
                        let ack_valid =
                            self.tx_seq.unack < seg.ackn && seg.ackn <= self.tx_seq.next;
                        // Ignore duplicate/invalid ACKs, lost segments are retx'd using timeout
                        if ack_valid {
                            todo!("window update");
                            todo!("wnd");
                            todo!("Remove from rexmt queue all fully ACK'd segments");
                            todo!("Release REXMT timer");
                        }

                        let r = if seg.seqn == self.rx_seq.next {
                            if todo!("out-of-order buffer empty") {
                                todo!("Copy incoming seg data to RCV buffer");
                                self.rx_seq.next = seg.seqn.wrapping_add(seg.data.len() as u32);
                            } else {
                                // Lost segment found
                                todo!("Drain out-of-order recv buffer to normal recv buffer");
                                // NOTE: HSEG = segment with highest seqn recv'd so far
                                todo!("rcv.nxt = hseg.seg + hseg.len");
                            }
                            todo!("Signal user data ready recv");
                            self.buffers.send_now.push_back(SegmentMeta {
                                seqn: self.tx_seq.next,
                                ackn: self.rx_seq.next,
                                window: self.rx_seq.window,
                                flags: SegmentFlags::ACK,
                                data: Vec::new(),
                            });
                            Res::empty()
                        } else {
                            todo!("Add recv'd data to out-of-order recv buffer");
                            todo!("self.rx_seq.window = RECV.WIND + seg.len");
                            self.buffers.send_now.push_back(SegmentMeta {
                                seqn: self.tx_seq.next,
                                ackn: self.rx_seq.next,
                                window: self.rx_seq.window,
                                flags: SegmentFlags::ACK,
                                data: Vec::new(),
                            });
                            Res::empty()
                        };

                        if todo!("seg.ack acknowledges our FIN?") {
                            if seg.flags.contains(SegmentFlags::FIN) {
                                todo!("FIN bit processing");
                                todo!("Turn off all timers");
                                todo!("Set timeout");
                                self.connection_state = ConnectionState::TimeWait;
                            } else {
                                self.connection_state = ConnectionState::FinWait2;
                            }
                        } else {
                            if seg.flags.contains(SegmentFlags::FIN) {
                                todo!("FIN bit processing");
                                self.connection_state = ConnectionState::Closing;
                            }
                        }
                        r
                    } else {
                        Res::empty()
                    }
                } else if seg.flags.contains(SegmentFlags::RST) {
                    Res::empty()
                } else {
                    self.buffers.send_now.push_back(SegmentMeta {
                        seqn: self.tx_seq.next,
                        ackn: self.rx_seq.next,
                        window: self.rx_seq.window,
                        flags: SegmentFlags::ACK,
                        data: Vec::new(),
                    });
                    Res::empty()
                }
            },
            ConnectionState::FinWait2 => {
                if self.check_segment_seq_ok(&seg) {
                    if seg.flags.contains(SegmentFlags::RST)
                        || seg.flags.contains(SegmentFlags::SYN)
                    {
                        self.buffers.send_now.push_back(SegmentMeta {
                            seqn: self.tx_seq.next,
                            ackn: 0,
                            window: self.rx_seq.window,
                            flags: SegmentFlags::RST,
                            data: Vec::new(),
                        });
                        let mut r = Res::empty();

                        let queued_sends = todo!("Queued sends");
                        let queued_recvs = todo!("Queued recvs");
                        if queued_sends || queued_recvs {
                            r.error = Some(Error::ConnectionReset);
                        }

                        self.clear();
                        r
                    } else if seg.flags.contains(SegmentFlags::ACK) {
                        todo!("Copy here from FinWait1 (this is FWT2)");
                        if seg.flags.contains(SegmentFlags::FIN) {
                            todo!("FIN bit processing");
                            todo!("Turn off all timers");
                            todo!("Set timeout 2MSL");
                            self.connection_state = ConnectionState::TimeWait;
                            Res::empty()
                        } else {
                            Res::empty()
                        }
                    } else {
                        Res::empty()
                    }
                } else if seg.flags.contains(SegmentFlags::RST) {
                    Res::empty()
                } else {
                    self.buffers.send_now.push_back(SegmentMeta {
                        seqn: self.tx_seq.next,
                        ackn: self.rx_seq.next,
                        window: self.rx_seq.window,
                        flags: SegmentFlags::ACK,
                        data: Vec::new(),
                    });
                    Res::empty()
                }
            },
            ConnectionState::CloseWait => {
                todo!();
            },
            ConnectionState::Closing | ConnectionState::TimeWait => {
                if self.check_segment_seq_ok(&seg) {
                    if seg.flags.contains(SegmentFlags::RST) {
                        self.clear();
                        Res::empty()
                    } else if seg.flags.contains(SegmentFlags::SYN) {
                        self.buffers.send_now.push_back(SegmentMeta {
                            seqn: self.tx_seq.next,
                            ackn: 0,
                            window: self.rx_seq.window,
                            flags: SegmentFlags::RST,
                            data: Vec::new(),
                        });

                        let mut r = Res::empty();
                        let queued_sends = todo!("Queued sends");
                        let queued_recvs = todo!("Queued recvs");
                        if queued_sends || queued_recvs {
                            r.error = Some(Error::ConnectionReset);
                        }

                        self.clear();
                        r
                    } else {
                        if seg.flags.contains(SegmentFlags::ACK)
                            && (self.tx_seq.unack < seg.ackn && seg.ackn < self.tx_seq.next)
                            && todo!("our FIN has been ACK'd")
                        {
                            todo!("Turn off all timers");
                            todo!("Set timer 2MSL");
                            self.connection_state = ConnectionState::TimeWait;
                        }
                        Res::empty()
                    }
                } else if seg.flags.contains(SegmentFlags::RST) {
                    Res::empty()
                } else {
                    self.buffers.send_now.push_back(SegmentMeta {
                        seqn: self.tx_seq.next,
                        ackn: self.rx_seq.next,
                        window: self.rx_seq.window,
                        flags: SegmentFlags::ACK,
                        data: Vec::new(),
                    });
                    Res::empty()
                }
            },
            ConnectionState::LastAck => {
                todo!();
            },
            ConnectionState::Closed => {
                if seg.flags.contains(SegmentFlags::ACK) {
                    self.buffers.send_now.push_back(SegmentMeta {
                        seqn: seg.seqn + seg.data.len() as u32,
                        ackn: 0,
                        window: self.rx_seq.window,
                        flags: SegmentFlags::RST | SegmentFlags::ACK,
                        data: Vec::new(),
                    });
                    Res::empty()
                } else {
                    self.buffers.send_now.push_back(SegmentMeta {
                        seqn: seg.ackn,
                        ackn: 0,
                        window: self.rx_seq.window,
                        flags: SegmentFlags::RST,
                        data: Vec::new(),
                    });
                    Res::empty()
                }
            },
        }
    }

    /// Called on timeout segment
    pub fn on_timeout(&mut self) -> Res {
        dbg!(self.connection_state);
        log::trace!("on_timeout");
        todo!();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    /// Server: Waiting for a connection request from any client.
    Listen,
    /// Client: SYN packet sent, waiting for SYN-ACK response.
    SynSent,
    /// Server: SYN packet received, SYN-ACK sent.
    SynReceived,
    /// Connection is open and operational.
    Established,
    /// Active closer: Has sent FIN packet. Waiting either for ACK reply, or FIN packet.
    FinWait1,
    /// Active closer: Has sent FIN packet, and got an ACK reply. Waiting for FIN packet.
    FinWait2,
    /// Waiting for a connection termination request from the local user.
    CloseWait,
    /// Waiting for a connection termination request acknowledgment from the remote TCP.
    Closing,
    /// Passive closer: Received FIN, replied with ACK and FIN packets. Waiting for ACK packet.
    LastAck,
    /// Waiting for to make sure the other side has time to close the connections.
    TimeWait,
    /// Connection has been closed, or has never been opened at all. Initial state.
    Closed,
}
