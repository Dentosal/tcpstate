use alloc::collections::VecDeque;

use crate::queue::BlobQueue;
use crate::{SegmentMeta, SeqN};

#[derive(Debug, Clone)]
pub struct TxBuffer {
    /// Output data stream buffer. User-sent data is written here,
    /// and segmentized into the network and the re_tx buffer.
    pub(crate) tx: BlobQueue,
    /// Retransmit queue
    /// Invariant: ordered
    pub(crate) re_tx: VecDeque<SegmentMeta>,
    /// Oldest unacknowledged sequence number
    pub unack: SeqN,
    /// Send sequence number to use when sending
    pub next: SeqN,
    pub init_seqn: SeqN,
    pub window: u16,
    /// Last sent (seqn, ackn, window) tuple, to make sure we wont send duplicates without reason
    pub last_sent: Option<(SeqN, SeqN, u16)>,
}

impl TxBuffer {
    pub fn init(&mut self, seqn: SeqN) {
        log::trace!("Init seqn={}", seqn);
        self.init_seqn = seqn;
        self.unack = self.init_seqn;
        self.next = self.init_seqn;
    }

    /// Next index is within unack window
    pub fn unack_has_space(&self) -> bool {
        self.next.lt(self.unack.wrapping_add(self.window as u32))
    }

    /// Number of bytes available in unack window
    pub fn space_available(&self) -> u32 {
        let end = self.unack.wrapping_add(self.window as u32);
        end.raw().wrapping_sub(self.next.raw())
    }

    /// This is the ACK we are waiting for
    pub fn is_curr_ack(&self, ackn: SeqN) -> bool {
        self.unack == ackn
    }

    /// This ACK packet for packet we sent, but not the next unack one
    pub fn is_non_ordered_ack(&self, ackn: SeqN) -> bool {
        ackn != self.unack && ackn.in_range_inclusive(self.unack, self.next)
    }

    pub fn on_send(&mut self, seg: SegmentMeta) {
        log::trace!("Sending {:?}", seg);

        self.last_sent = Some((seg.seqn, seg.ackn, seg.window));

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
            unack: SeqN::ZERO,
            next: SeqN::ZERO,
            init_seqn: SeqN::ZERO,
            window: 0,
            last_sent: None,
        }
    }
}
