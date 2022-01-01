use alloc::collections::VecDeque;

use crate::queue::BlobQueue;
use crate::SegmentMeta;

use crate::mock::random_seqnum;

#[derive(Debug, Clone)]
pub struct TxBuffer {
    /// Output data stream buffer. User-sent data is written here,
    /// and segmentized into the network and the re_tx buffer.
    pub(crate) tx: BlobQueue,
    /// Retransmit queue
    /// Invariant: ordered
    pub(crate) re_tx: VecDeque<SegmentMeta>,
    /// Oldest unacknowledged sequence number
    pub unack: u32,
    /// Send sequence number to use when sending
    pub next: u32,
    pub window: u16,
    pub init_seqn: u32,
    /// Last sent (seqn, ackn, window) tuple, to make sure we wont send duplicates without reason
    pub last_sent: (u32, u32, u16),
}

impl TxBuffer {
    pub fn init(&mut self) {
        assert!(self.init_seqn == 0, "reinit"); // XXX
        self.init_seqn = random_seqnum();
        log::trace!("Init seqn={}", self.init_seqn);
        self.unack = self.init_seqn;
        self.next = self.init_seqn;
    }

    /// Next index is within unack window
    pub fn unack_has_space(&self) -> bool {
        // TODO: handle wrapping
        self.next < self.unack + (self.window as u32)
    }

    /// Number of bytes available in unack window
    pub fn space_available(&self) -> u32 {
        // TODO: handle wrapping
        let end = self.unack.wrapping_add(self.window as u32);
        end.wrapping_sub(self.next)
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

    pub fn on_send(&mut self, seg: SegmentMeta) {
        log::trace!("Sending {:?}", seg);

        self.last_sent = (seg.seqn, seg.ackn, seg.window);

        self.next = self.next.wrapping_add(seg.seq_size());

        if seg.seq_size() != 0 {
            self.re_tx.push_back(seg);
        }
    }
}

impl Default for TxBuffer {
    fn default() -> Self {
        Self {
            tx: BlobQueue::default(),
            re_tx: VecDeque::new(),
            unack: 0,
            next: 0,
            window: 0,
            init_seqn: 0,
            last_sent: (0, 0, 0), // TODO: None
        }
    }
}
