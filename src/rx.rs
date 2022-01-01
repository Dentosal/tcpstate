//! TODO: buffer size limit
//! TODO: buffers larger then 2GiB??

use alloc::vec::Vec;

use crate::options::INITIAL_WINDOW_SIZE;
use crate::{SegmentFlags, SegmentMeta, SeqN};

use crate::queue::BlobQueue;

#[derive(Debug, Clone)]
pub struct RxBuffer {
    /// ACK'd user-readable data
    buffer: BlobQueue,
    /// Last ACK'd sequnce number
    pub ackd: SeqN,
    done: bool,
    /// Window size
    window: u16,
    /// Initial sequence number
    init_seqn: SeqN,
}
impl RxBuffer {
    /// Called when in Listen state and a new SYN packet arrives,
    /// or when in SynSent state and a new SYN-ACK packet arrives
    pub fn init(&mut self, seqn: SeqN) {
        log::trace!("Init seqn={}", seqn);
        self.init_seqn = seqn;
        self.ackd = seqn.wrapping_add(1); // +1 because SYN
    }

    pub fn in_window(&self, seqn: SeqN) -> bool {
        seqn.in_range_inclusive(
            self.ackd,
            self.ackd.wrapping_add(self.window as u32).wrapping_sub(1),
        )
    }

    pub fn window_size(&self) -> u16 {
        self.window
    }

    /// Buffer empty, and will not have any more data
    pub fn is_done(&self) -> bool {
        self.done
    }

    /// Takes up to `limit` bytes and ACKs them
    pub fn available_bytes(&self) -> usize {
        self.buffer.available_bytes()
    }

    /// Takes up to `limit` bytes (if any) and ACKs them
    /// If returns None, then the FIN is is acknowledged as well
    /// TODO: max limit, derived from sequence number space size
    pub fn read_bytes(&mut self, limit: usize) -> Vec<u8> {
        assert!(!self.done);
        let (result, fin) = self.buffer.read_bytes(limit);
        if result.len() < limit {
            assert!(fin);
        }
        self.done = fin;
        result
    }

    /// Bytes from the network are written using this
    pub fn write(&mut self, seg: SegmentMeta) {
        self.ackd = self.ackd.wrapping_add(seg.seq_size() as u32);
        if seg.flags.contains(SegmentFlags::FIN) {
            debug_assert!(!self.buffer.fin(), "Duplicate FIN marking");
            log::trace!("Network FIN");
            self.buffer.mark_fin();
        }
        self.buffer.write_bytes(seg.data);
    }

    /// This can be returned with new packets as window field value
    pub fn curr_window(&self) -> u16 {
        if self.buffer.fin() {
            log::trace!("DONE! w");
            return 0;
        }
        log::trace!("curr w s={:?} b={:?}", self.window, self.available_bytes());
        // FIXME: ugly/dangerous conversions
        self.window
            .wrapping_sub(self.available_bytes() as u16)
            .wrapping_sub(self.buffer.fin() as u16)
    }
}

impl Default for RxBuffer {
    fn default() -> Self {
        Self {
            buffer: BlobQueue::new(),
            ackd: SeqN::ZERO,
            done: false,
            window: INITIAL_WINDOW_SIZE,
            init_seqn: SeqN::ZERO,
        }
    }
}
