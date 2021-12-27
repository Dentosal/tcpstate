//! TODO: buffer size limit
//! TODO: buffers larger then 2GiB??

use alloc::vec::Vec;

use crate::options::INITIAL_WINDOW_SIZE;

use crate::queue::BlobQueue;

#[derive(Debug, Clone)]
pub struct RxBuffer {
    /// All data in the buffer is always user-readable
    /// and acknowledged whenever it is read
    buffer: BlobQueue,
    /// If user has read the FIN byte
    done: bool,
    /// Last ACK'd sequnce number, e.g. first item before the buffer
    ackd: u32,
    /// Window size
    window: u16,
    /// Initial sequence number
    init_seqn: u32,
}
impl RxBuffer {
    /// Called when in Listen state and a new SYN packet arrives,
    /// or when in SynSent state and a new SYN-ACK packet arrives
    pub fn init(&mut self, seqn: u32) {
        log::trace!("Init seqn={}", seqn);
        self.init_seqn = seqn;
        self.ackd = seqn.wrapping_add(1); // +1 because SYN
    }

    pub fn in_window(&self, seqn: u32) -> bool {
        // TODO: handle wrapping
        self.ackd <= seqn && seqn < (self.ackd + (self.window as u32))
    }

    pub fn window_size(&self) -> u16 {
        self.window
    }

    pub fn mark_network_fin(&mut self) {
        debug_assert!(!self.buffer.fin(), "Duplicate FIN marking");
        log::trace!("Network FIN");
        self.buffer.mark_fin();
    }

    /// Takes up to `limit` bytes and ACKs them
    pub fn available_bytes(&self) -> usize {
        self.buffer.available_bytes()
    }

    /// Takes up to `limit` bytes (if any) and ACKs them
    /// If returns None, then the FIN is is acknowledged as well
    /// TODO: max limit, derived from sequence number space size
    pub fn read_bytes(&mut self, limit: usize) -> Vec<u8> {
        let (result, fin) = self.buffer.read_bytes(limit);
        if result.len() < limit {
            assert!(fin);
        }
        self.ackd = self.ackd.wrapping_add(result.len() as u32);
        self.done = fin;
        result
    }

    /// Bytes from the network are written using this
    pub fn write_bytes(&mut self, data: Vec<u8>) {
        self.buffer.write_bytes(data);
    }

    /// This can be returned with new packets as ACK field value
    pub fn curr_ackn(&self) -> u32 {
        self.ackd
    }

    /// Next sequence number to be accepted for new data
    pub fn next_seqn(&self) -> u32 {
        self.ackd
            .wrapping_add(self.available_bytes() as u32)
            .wrapping_add(self.buffer.fin() as u32)
    }
}

impl Default for RxBuffer {
    fn default() -> Self {
        Self {
            buffer: BlobQueue::new(),
            done: false,
            ackd: 0,
            window: INITIAL_WINDOW_SIZE,
            init_seqn: 0,
        }
    }
}
