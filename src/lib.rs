//! TCP as per https://www.ietf.org/rfc/rfc793.txt

#![cfg_attr(not(any(test, feature = "std")), no_std)]
#![feature(default_free_fn, duration_constants, let_else, drain_filter)]
#![allow(dead_code, unreachable_code, unused)]

//! TODO: Return Error vs Signal User
//! TODO: Window scaling
//! TODO: Wrapping comparisons with windows
//! TODO: std-compat

#[macro_use]
extern crate alloc;

use core::sync::atomic::{AtomicU64, Ordering};
use core::time::Duration;

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use hashbrown::HashSet;

mod event;
mod result;
mod segment;

use event::WaitUntil;

pub use event::Cookie;
pub use result::Error;
pub use segment::{SegmentFlags, SegmentMeta};

#[cfg(feature = "std")]
mod log {
    pub use std::println as error;
    pub use std::println as warn;
    pub use std::println as info;
    pub use std::println as debug;
    pub use std::println as trace;
}

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

pub const MAX_SEGMENT_LIFETIME: Duration = Duration::from_secs(120);
const INITIAL_WINDOW_SIZE: u16 = 0x1000;
const MAX_SEGMENT_SIZE: usize = 512;
const SLOW_START_TRESHOLD: u16 = 512;

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

#[derive(Debug, Clone)]
pub struct SendSeq {
    /// Oldest unacknowledged sequence number
    unack: u32,
    /// Send sequence number to use when sending
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
impl Default for SendSeq {
    fn default() -> Self {
        Self {
            unack: 0,
            next: 0,
            window: INITIAL_WINDOW_SIZE,
            init_seqn: 0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RecvSeq {
    /// Next sequence number to expect
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
    readable: VecDeque<Vec<u8>>,
    /// Packets to be sent after the call
    send_now: VecDeque<SegmentMeta>,
}

fn duration_absdiff(a: Duration, b: Duration) -> Duration {
    if a > b {
        a - b
    } else {
        b - a
    }
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
impl Timers {
    fn clear(&mut self) {
        *self = Self::default();
    }
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
            tx_seq: SendSeq::default(),
            rx_seq: RecvSeq::default(),
            buffers: Buffers::default(),
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
        self.connection_state = state;
        match self.connection_state {
            ConnectionState::Established => {
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

    fn new_cookie(&mut self) -> Cookie {
        // let result = self.next_cookie;
        // self.next_cookie = self.next_cookie.next();
        // result
        self.next_cookie = self.next_cookie.next();
        self.next_cookie
    }

    fn return_retry_after<T>(&mut self, until: WaitUntil) -> Result<T, Error> {
        let cookie = self.new_cookie();
        log::trace!("Suspend/retry {:?} {:?}", until, cookie);
        self.events_suspended.insert((until, cookie));
        Err(Error::RetryAfter(cookie))
    }

    fn return_continue_after<T>(&mut self, until: WaitUntil) -> Result<T, Error> {
        let cookie = self.new_cookie();
        log::trace!("Suspend/continue {:?} {:?}", until, cookie);
        self.events_suspended.insert((until, cookie));
        Err(Error::ContinueAfter(cookie))
    }

    fn trigger<F>(&mut self, f: F)
    where
        F: Fn(WaitUntil) -> bool,
    {
        for (_, cookie) in self.events_suspended.drain_filter(|(wait, _)| f(*wait)) {
            log::trace!("Triggered cookie {:?}", cookie);
            self.events_ready.insert(cookie);
        }
    }

    /// Clear event if it is active.
    /// Returns `Ok(())` if the event was active, an `Err(())` otherwise.
    #[must_use]
    pub fn try_wait_event(&mut self, cookie: Cookie) -> Result<(), ()> {
        if self.events_ready.remove(&cookie) {
            log::trace!("Clearing cookie {:?}", cookie);
            Ok(())
        } else {
            debug_assert!(
                self.events_suspended.iter().any(|(_, c)| *c == cookie),
                "Requested cookie is not present in either list"
            );
            log::trace!("Cookie {:?} was not ready", cookie);
            Err(())
        }
    }

    /// RFC793, page 25
    fn check_segment_seq_ok(&self, seg: &SegmentMeta) -> bool {
        dbg!(&self.rx_seq);
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

    fn readable_buf_len(&self) -> usize {
        self.buffers.readable.iter().map(|b| b.len()).sum()
    }

    pub fn take_outbound(&mut self) -> Option<SegmentMeta> {
        self.buffers.send_now.pop_front()
    }

    /// Takes up to `limit` bytes of input
    fn readable_buf_take(&mut self, limit: usize) -> Vec<u8> {
        let mut result = Vec::new();

        while let Some(value) = self.buffers.readable.pop_front() {
            result.extend(value);
            if result.len() >= limit {
                let rest = result.split_off(limit);
                self.buffers.readable.push_front(rest);
                return result;
            }
        }

        result
    }

    /// Establish a connection
    pub fn call_connect(&mut self, remote: RemoteAddr) -> Result<(), Error> {
        log::trace!("state: {:?}", self.connection_state);
        log::trace!("call_connect {:?}", remote);
        if self.connection_state == ConnectionState::Closed {
            self.remote_address = Some(remote);
            self.set_state(ConnectionState::SynSent);
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
            Ok(())
        } else {
            Err(Error::InvalidStateTransition)
        }
    }

    /// Listen for connection
    pub fn call_listen(&mut self) -> Result<(), Error> {
        log::trace!("state: {:?}", self.connection_state);
        log::trace!("call_listen");
        if self.connection_state == ConnectionState::Closed {
            self.set_state(ConnectionState::Listen);
            Ok(())
        } else {
            Err(Error::InvalidStateTransition)
        }
    }

    /// Send some data
    /// RFC 793 page 55
    pub fn call_send(&mut self, input_data: Vec<u8>) -> Result<(), Error> {
        log::trace!("state: {:?}", self.connection_state);
        log::trace!("call_send {:?}", input_data);
        match self.connection_state {
            ConnectionState::Listen => Err(Error::NotConnected),
            ConnectionState::SynSent | ConnectionState::SynReceived => {
                let len = input_data.len() as u32;
                self.buffers.tx.push_back(input_data);
                return self.return_continue_after(WaitUntil::Acknowledged {
                    seqn: self.tx_seq.next.wrapping_add(len),
                });
            }
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
                        || (self.tx_buf_len() > 0 && self.options.nagle_delay == Duration::ZERO)
                    {
                        self.send_ack();
                        return Ok(());
                    }
                }
                Ok(())
            }
            ConnectionState::FinWait1
            | ConnectionState::FinWait2
            | ConnectionState::Closing
            | ConnectionState::TimeWait
            | ConnectionState::LastAck => Err(Error::ConnectionClosing),
            ConnectionState::Closed => Err(Error::NotConnected),
        }
    }

    /// Amount of data available for receiving without blocking
    pub fn recv_available(&mut self) -> usize {
        self.readable_buf_len()
    }

    /// Receive some data, if any available
    pub fn call_recv(&mut self, buffer: &mut [u8]) -> Result<usize, Error> {
        log::trace!("state: {:?}", self.connection_state);
        log::trace!("call_recv len={}", buffer.len());
        match self.connection_state {
            ConnectionState::Listen | ConnectionState::SynSent | ConnectionState::SynReceived => {
                return self.return_retry_after(WaitUntil::Established);
            }
            ConnectionState::Established => {
                if self.readable_buf_len() >= buffer.len() {
                    let data = self.readable_buf_take(buffer.len());
                    buffer.copy_from_slice(&data);
                    Ok(data.len())
                } else {
                    todo!("QUEUE reg");
                }
            }
            ConnectionState::FinWait1 | ConnectionState::FinWait2 => {
                let has_data = todo!("queue_has_enough_data");
                // if !(has_data) {
                //     todo!("QUEUE reg");
                //     return Ok(());
                // }

                todo!("Reassemble");
                todo!("Signal user for data");
            }
            ConnectionState::CloseWait => {
                if todo!("data waiting to be delivered to the user") {
                    todo!("Reassemble remaining data into RCV buffer");
                    todo!("Signal user to read recv buffer");
                } else {
                    Err(Error::ConnectionClosing)
                }
            }
            ConnectionState::Closing | ConnectionState::TimeWait | ConnectionState::LastAck => {
                Err(Error::ConnectionClosing)
            }
            ConnectionState::Closed => Err(Error::NotConnected),
        }
    }

    /// Request closing the socket
    pub fn call_close(&mut self) -> Result<(), Error> {
        log::trace!("state: {:?}", self.connection_state);
        log::trace!("call_close");
        match self.connection_state {
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
                    self.set_state(ConnectionState::FinWait1);
                    self.buffers.send_now.push_back(SegmentMeta {
                        seqn: self.tx_seq.next,
                        ackn: 0, // ??
                        window: self.rx_seq.window,
                        flags: SegmentFlags::FIN,
                        data: Vec::new(),
                    });
                    Ok(())
                }
            }
            ConnectionState::Established => {
                // All sends must be segmentized first
                if !self.buffers.tx.is_empty() {
                    log::debug!("Delaying close until all sends are segmentized");
                    return self.return_retry_after(WaitUntil::OutputBufferClear);
                }

                self.set_state(ConnectionState::FinWait1);
                self.buffers.send_now.push_back(SegmentMeta {
                    seqn: self.tx_seq.next,
                    ackn: self.rx_seq.next,
                    window: self.rx_seq.window,
                    flags: SegmentFlags::FIN,
                    data: Vec::new(),
                });
                self.tx_seq.next = self.tx_seq.next.wrapping_add(1); // FIN
                Ok(())
            }
            ConnectionState::FinWait1
            | ConnectionState::FinWait2
            | ConnectionState::Closing
            | ConnectionState::TimeWait
            | ConnectionState::LastAck => Err(Error::ConnectionClosing),
            ConnectionState::CloseWait => {
                // All sends must be segmentized first
                if !self.buffers.tx.is_empty() {
                    log::debug!("Delaying close until all sends are segmentized");
                    return self.return_retry_after(WaitUntil::OutputBufferClear);
                }

                self.set_state(ConnectionState::LastAck);
                self.buffers.send_now.push_back(SegmentMeta {
                    seqn: self.tx_seq.next,
                    ackn: self.rx_seq.next,
                    window: self.rx_seq.window,
                    flags: SegmentFlags::FIN,
                    data: Vec::new(),
                });
                self.tx_seq.next = self.tx_seq.next.wrapping_add(1); // FIN
                Ok(())
            }
            ConnectionState::Closed => Err(Error::NotConnected),
        }
    }

    /// Request closing the socket
    pub fn call_abort(&mut self) -> Result<(), Error> {
        log::trace!("state: {:?}", self.connection_state);
        log::trace!("call_abort");
        match self.connection_state {
            ConnectionState::Listen | ConnectionState::SynSent => {
                let has_queued_recvs = todo!("RECV queue");
                if has_queued_recvs {
                    todo!("Send ConnectionReset to the RECVs");
                }
                self.clear();
                Ok(())
            }
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
                let mut r = Ok(());

                let queued_sends = todo!("Queued sends");
                let queued_recvs = todo!("Queued recvs");
                if queued_sends || queued_recvs {
                    r = Err(Error::ConnectionReset);
                }

                self.clear();
                r
            }
            ConnectionState::Closing | ConnectionState::LastAck | ConnectionState::TimeWait => {
                self.clear();
                Ok(())
            }
            ConnectionState::Closed => Err(Error::NotConnected),
        }
    }

    /// Sends an ack packet, including any data form buffer if possible
    fn send_ack(&mut self) {
        // TODO: check window size before sending any payload

        let data_to_send = self.tx_buf_take(MAX_SEGMENT_SIZE);
        let len = data_to_send.len();
        log::trace!("Sending ACK with data {:?}", data_to_send);
        let seg = SegmentMeta {
            seqn: self.tx_seq.next,
            ackn: self.rx_seq.next,
            window: self.rx_seq.window,
            flags: SegmentFlags::ACK,
            data: data_to_send,
        };
        self.buffers.re_tx.push_back(seg.clone());
        self.buffers.send_now.push_back(seg);

        self.tx_seq.next = self.tx_seq.next.wrapping_add(len as u32); // XXX: is this before or after creating seg?

        if len != 0 {
            self.timers.re_tx = Some(Instant::now().add(self.timings.rto));
        }
    }

    /// Handles packets valid ACKs (including those with duplicates)
    /// Section ES3 in the PDF
    fn handle_received(&mut self, seg: SegmentMeta) -> Result<(), Error> {
        assert!(seg.seqn >= self.rx_seq.next); // TODO: wrapping
        log::trace!("handle_received");

        // TODO: PSH flag handling

        let has_fin = seg.flags.contains(SegmentFlags::FIN);
        let any_new_data = has_fin || !seg.data.is_empty();

        if seg.seqn == self.rx_seq.next {
            log::trace!("This packet is in-order");
            // Add seg to user-readable buffer
            if !seg.data.is_empty() {
                self.rx_seq.next = seg.seqn.wrapping_add(seg.data.len() as u32);
                self.buffers.readable.push_back(seg.data);
            }

            // Process out-of-order packets
            if !self.buffers.oo_rx.is_empty() {
                todo!("out-of-order packets");
            }

            // todo!("signal: Data readable");
            log::trace!("Data readable");
        } else {
            panic!("This packet is out-of-order");
            let len = seg.data.len();
            self.buffers.oo_rx.push_back(seg);
            self.rx_seq.window += len as u16;
        }

        if has_fin {
            self.rx_seq.next = seg.seqn.wrapping_add(1);
        }

        // Do not ack ack-only packets
        if any_new_data {
            log::trace!("ACK'ing received packet");
            self.send_ack();
        }

        Ok(())
    }

    /// Called on incoming segment
    /// See RFC 793 page 64
    pub fn on_segment(&mut self, seg: SegmentMeta) -> Result<(), Error> {
        log::trace!("state: {:?}", self.connection_state);
        log::trace!("on_segment {:?}", seg);

        if self.connection_state == ConnectionState::Closed {
            self.buffers.send_now.push_back(response_to_closed(seg));
            return Ok(());
        } else if self.connection_state == ConnectionState::Listen {
            if seg.flags.contains(SegmentFlags::RST) {
                return Ok(());
            }

            if seg.flags.contains(SegmentFlags::ACK) {
                self.buffers.send_now.push_back(SegmentMeta {
                    seqn: seg.ackn,
                    ackn: 0,
                    window: 0,
                    flags: SegmentFlags::RST,
                    data: Vec::new(),
                });
                return Ok(());
            }

            if !seg.flags.contains(SegmentFlags::SYN) {
                log::warn!("Invalid packet received, dropping");
                return Ok(());
            }

            // TODO: Store remote peer address
            self.rx_seq.next = seg.seqn.wrapping_add(1);
            self.rx_seq.init_seqn = seg.seqn;
            // TODO: queue data, control if any available
            assert!(seg.data.len() == 0, "TODO");
            let seqn = random_seqnum();
            self.tx_seq.next = seqn.wrapping_add(1);
            self.tx_seq.unack = seqn;
            self.set_state(ConnectionState::SynReceived);
            self.buffers.send_now.push_back(SegmentMeta {
                seqn,
                ackn: self.rx_seq.next,
                window: self.rx_seq.window,
                flags: SegmentFlags::SYN | SegmentFlags::ACK,
                data: Vec::new(),
            });
            return Ok(());
        } else if self.connection_state == ConnectionState::SynSent {
            let ack_acceptable = if seg.flags.contains(SegmentFlags::ACK) {
                if seg.flags.contains(SegmentFlags::RST) {
                    return Ok(());
                }

                if seg.ackn <= self.tx_seq.init_seqn || seg.ackn > self.tx_seq.next {
                    self.buffers.send_now.push_back(SegmentMeta {
                        seqn: seg.ackn,
                        ackn: 0,
                        window: self.rx_seq.window,
                        flags: SegmentFlags::RST,
                        data: Vec::new(),
                    });
                    return Ok(());
                }

                seg.ackn <= self.tx_seq.unack || seg.ackn <= self.tx_seq.next
            } else {
                false
            };

            if seg.flags.contains(SegmentFlags::RST) {
                return if ack_acceptable {
                    self.clear();
                    Err(Error::ConnectionReset)
                } else {
                    Ok(())
                };
            }

            if seg.flags.contains(SegmentFlags::SYN) {
                self.rx_seq.next = seg.seqn.wrapping_add(1);
                self.rx_seq.init_seqn = seg.seqn;
                self.clear_re_tx_range(self.tx_seq.unack, seg.ackn);
                self.tx_seq.unack = seg.ackn;
                if self.tx_seq.unack > self.tx_seq.init_seqn {
                    // Our SYN has been ACK'd
                    self.set_state(ConnectionState::Established);
                    self.send_ack();
                } else {
                    todo!("If packet has data, queue it until ESTABLISHED");
                    self.set_state(ConnectionState::SynReceived);
                    self.buffers.send_now.push_back(SegmentMeta {
                        seqn: self.tx_seq.init_seqn,
                        ackn: self.rx_seq.next,
                        window: self.rx_seq.window,
                        flags: SegmentFlags::SYN | SegmentFlags::ACK,
                        data: Vec::new(),
                    });
                }
            }

            return Ok(());
        }

        // Acceptability check (RFC 793 page 36 and 68)
        // TODO: process valid ACKs and RSTs even if receive_window == 0?
        if !self.check_segment_seq_ok(&seg) {
            log::trace!("Non-acceptable segment {:#?}", seg);
            if !seg.flags.contains(SegmentFlags::RST) {
                self.buffers.send_now.push_back(SegmentMeta {
                    seqn: self.tx_seq.next,
                    ackn: self.rx_seq.next,
                    window: self.rx_seq.window,
                    flags: SegmentFlags::ACK,
                    data: Vec::new(),
                });
            }
            return Ok(());
        }

        // self.tx_seq.window = seg.window.min(self.congestation_window);

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
                ConnectionState::Established
                | ConnectionState::FinWait1
                | ConnectionState::FinWait2
                | ConnectionState::CloseWait => {
                    todo!("Pending RECV/SEND calls: signal error ConnectionReset");
                    self.clear();
                    todo!("Return error?");
                }
                ConnectionState::Closing | ConnectionState::LastAck | ConnectionState::TimeWait => {
                    self.clear();
                }
                _ => unreachable!(),
            }
            return Ok(());
        }

        if seg.flags.contains(SegmentFlags::SYN) {
            log::warn!("Peer error: SYN");

            todo!("Pending RECV/SEND calls: signal error ConnectionReset");

            // Error! Send reset, flush all queues
            self.buffers.send_now.push_back(SegmentMeta {
                seqn: self.tx_seq.next,
                ackn: 0,
                window: self.rx_seq.window,
                flags: SegmentFlags::RST,
                data: Vec::new(),
            });

            todo!("Return error?");
        }

        if seg.flags.contains(SegmentFlags::ACK) {
            if self.connection_state == ConnectionState::LastAck {
                if seg.ackn == self.tx_seq.next {
                    log::trace!("Last ACK received");
                    self.clear();
                } else {
                    log::trace!("Last ACK not received ??");
                }
                return Ok(());
            }

            if self.connection_state == ConnectionState::TimeWait {
                todo!("Is this FIN?");
                todo!("Ack FIN");
                todo!("Restart MSL 2 timeout");
                return Ok(());
            }

            if self.connection_state == ConnectionState::SynReceived {
                // TODO: wrapping
                if self.tx_seq.unack <= seg.ackn && seg.ackn <= self.tx_seq.next {
                    self.set_state(ConnectionState::Established);
                }
                // TODO: should we return here?
                dbg!("cont ConnectionState::SynReceived");
            }

            if seg.ackn < self.tx_seq.unack || seg.ackn > self.tx_seq.next {
                log::warn!("Dropping invalid ACK");
                return Ok(());
            }

            if seg.ackn == self.tx_seq.unack {
                // Duplicate ACK for the current segment
                dbg!("Duplicate");
                // self.dup_ack_count += 1;
                // log::trace!("new duplicate, count={}", self.dup_ack_count);
                // if self.dup_ack_count > 2 {
                //     // Fast recovery
                //     todo!("Fast recovery");
                // } else if self.dup_ack_count == 2 {
                //     // Fast retransmit
                //     todo!("Fast retransmit");
                // }
            } else {
                // New valid ACK
                debug_assert!(self.tx_seq.unack < seg.ackn && seg.ackn <= self.tx_seq.next);

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
                self.tx_seq.window = seg.window.min(self.congestation_window);

                self.clear_re_tx_range(self.tx_seq.unack, seg.ackn);
                self.tx_seq.unack = seg.ackn;
                self.exp_backoff = 1;
                self.timers.re_tx = None;

                self.trigger(|until| {
                    if let WaitUntil::Acknowledged { seqn } = until {
                        seqn <= seg.ackn
                    } else {
                        false
                    }
                });

                if self.connection_state == ConnectionState::Closing {
                    if seg.ackn == self.tx_seq.next {
                        assert!(seg.ackn == self.tx_seq.unack, "???");
                        // TODO: cleanup&document condition
                        log::trace!("Out FIN has been ACK'd");
                        self.set_state(ConnectionState::TimeWait);
                        self.timers.clear();
                        self.timers.timewait = Some(Instant::now().add(MAX_SEGMENT_LIFETIME * 2));
                    }
                }

                if self.connection_state == ConnectionState::FinWait1 {
                    if seg.ackn == self.tx_seq.next {
                        // TODO: cleanup&document condition
                        assert!(seg.ackn == self.tx_seq.unack, "???");
                        log::trace!("Out FIN has been ACK'd");
                        self.set_state(ConnectionState::FinWait2);
                    }
                }

                if self.connection_state == ConnectionState::FinWait2 {
                    self.trigger(|w| w == WaitUntil::OutputBufferClear);
                }
            }
        }

        // Process segment data, if any
        match self.connection_state {
            ConnectionState::Established
            | ConnectionState::FinWait1
            | ConnectionState::FinWait2 => {
                // TODO: window size adjustment
                self.handle_received(seg.clone());
            }
            other => {
                if seg.flags.contains(SegmentFlags::FIN) || !seg.data.is_empty() {
                    log::warn!("Peer error: sends data or FIN after FIN");
                }
            }
        }

        if matches!(
            self.connection_state,
            ConnectionState::Closed | ConnectionState::Listen | ConnectionState::SynSent
        ) {
            return Ok(());
        }

        if seg.flags.contains(SegmentFlags::FIN) {
            log::trace!("Got FIN packet");

            // TODO: Signal user: ConnectionClosing

            // Retry all recvs, which will not return ConnectionClosing after the buffer is empty
            self.trigger(|until| matches!(until, WaitUntil::Recv { .. }));

            self.rx_seq.next = seg.seqn.wrapping_add(1); // Make space for ACK

            match self.connection_state {
                ConnectionState::SynReceived | ConnectionState::Established => {
                    log::trace!("XXX: 1");
                    self.set_state(ConnectionState::CloseWait);
                }
                ConnectionState::FinWait1 => {
                    dbg!("fw1 ck", self.tx_seq.unack, self.tx_seq.next);
                    if self.tx_seq.unack == self.tx_seq.next {
                        // Our FIN has been ACK'd
                        self.set_state(ConnectionState::TimeWait);
                        self.timers.clear();
                        self.timers.timewait = Some(Instant::now().add(MAX_SEGMENT_LIFETIME * 2));
                    } else {
                        self.set_state(ConnectionState::Closing);
                    }
                }
                ConnectionState::FinWait2 => {
                    self.set_state(ConnectionState::TimeWait);
                    self.timers.clear();
                    self.timers.timewait = Some(Instant::now().add(MAX_SEGMENT_LIFETIME * 2));
                }
                ConnectionState::TimeWait => {
                    todo!("Restart 2MSL time-wait timeout");
                }
                _ => {}
            }
        }

        Ok(())
    }

    fn on_timer_re_tx(&mut self) {
        log::trace!("on_timer_re_tx");
        match self.connection_state {
            ConnectionState::Listen
            | ConnectionState::SynSent
            | ConnectionState::SynReceived
            | ConnectionState::Established
            | ConnectionState::FinWait1
            | ConnectionState::Closing
            | ConnectionState::CloseWait
            | ConnectionState::LastAck => {
                todo!("CALC RTO");
                todo!("REARM TIMER");
                todo!("SEND FROM RE_TX QUEUE");
            }
            ConnectionState::FinWait2 | ConnectionState::TimeWait | ConnectionState::Closed => {
                unreachable!()
            }
        }
    }

    fn on_timer_timewait(&mut self) {
        log::trace!("on_timer_timewait");
        assert!(self.connection_state == ConnectionState::TimeWait);
        self.clear();
    }

    fn on_timer_usertime(&mut self) {
        log::trace!("on_timer_usertime");
        todo!("Signal error to all outstanding calls");
        self.clear();
    }

    /// To be called whenever a timer expires, or simply periodically
    pub fn on_time_tick(&mut self, time: Instant) {
        log::trace!("state: {:?}", self.connection_state);
        log::trace!("on_timeout");

        if self.timers.re_tx.map_or(false, |t| t <= time) {
            self.timers.re_tx = None;
            self.on_timer_re_tx();
        }

        if self.timers.timewait.map_or(false, |t| t <= time) {
            self.timers.timewait = None;
            self.on_timer_timewait();
        }

        if self.timers.usertime.map_or(false, |t| t <= time) {
            self.timers.usertime = None;
            self.on_timer_usertime();
        }
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
