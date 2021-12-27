use alloc::collections::VecDeque;

use crate::options::INITIAL_WINDOW_SIZE;
use crate::queue::BlobQueue;
use crate::{SegmentFlags, SegmentMeta};

use crate::mock::random_seqnum;

#[derive(Debug, Clone)]
pub struct TxBuffer {
    /// Output data stream buffer. User-sent data is written here,
    /// and segmentized into the network and the re_tx buffer.
    pub(crate) tx: BlobQueue,
    /// Retransmit queue
    /// Invariant: ordered
    pub(crate) re_tx: VecDeque<SegmentMeta>,
    /// Packets to be sent after the call
    pub(crate) send_now: VecDeque<SegmentMeta>,
    /// Oldest unacknowledged sequence number
    pub unack: u32,
    /// Send sequence number to use when sending
    pub next: u32,
    pub window: u16,
    pub init_seqn: u32,
}

impl TxBuffer {
    pub fn init(&mut self) {
        self.init_seqn = random_seqnum();
        log::trace!("Init seqn={}", self.init_seqn);
        self.unack = self.init_seqn;
        self.next = self.init_seqn.wrapping_add(1);
    }

    /// Next index is within unack window
    pub fn unack_has_space(&self) -> bool {
        // TODO: handle wrapping
        self.next < self.unack + (self.window as u32)
    }

    /// This is the ACK we are waiting for
    pub fn is_curr_ack(&self, ackn: u32) -> bool {
        self.unack == ackn
    }

    /// This ACK packet for packet we sent, but not the next unack one
    pub fn is_non_ordered_ack(&self, ackn: u32) -> bool {
        // TODO: wrapping
        self.unack < ackn && ackn <= self.next
    }

    pub fn send(&mut self, seg: SegmentMeta) {
        log::trace!("Sending {:?}", seg);

        self.next = self.next.wrapping_add(seg.seq_size());

        self.re_tx.push_back(seg.clone());
        self.send_now.push_back(seg);
    }
}

impl Default for TxBuffer {
    fn default() -> Self {
        Self {
            tx: BlobQueue::default(),
            re_tx: VecDeque::new(),
            send_now: VecDeque::new(),
            unack: 0,
            next: 0,
            window: INITIAL_WINDOW_SIZE,
            init_seqn: 0,
        }
    }
}
