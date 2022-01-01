//! TODO: size limit

use alloc::collections::VecDeque;
use alloc::vec::Vec;

/// Queue of blobs, with a FIN marker
#[derive(Debug, Clone, Default)]
pub struct BlobQueue {
    buffer: VecDeque<Vec<u8>>,
    /// Fin has been received from the network. No new data will be added.
    fin: bool,
}
impl BlobQueue {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn fin(&self) -> bool {
        self.fin
    }

    pub fn mark_fin(&mut self) {
        self.fin = true;
    }

    pub fn is_empty(&self) -> bool {
        self.available_bytes() == 0
    }

    /// Takes up to `limit` bytes and ACKs them
    pub fn available_bytes(&self) -> usize {
        self.buffer.iter().map(|b| b.len()).sum()
    }

    /// Takes up to `limit` bytes, and returns them and whether fin has been reached.
    pub fn read_bytes(&mut self, limit: usize) -> (Vec<u8>, bool) {
        assert!(limit != 0);

        let mut result = Vec::new();

        while let Some(value) = self.buffer.pop_front() {
            result.extend(value);
            if result.len() >= limit {
                let rest = result.split_off(limit);
                self.buffer.push_front(rest);
                break;
            }
        }

        let done = result.len() < limit && self.fin;
        (result, done)
    }

    pub fn write_bytes(&mut self, data: Vec<u8>) {
        if data.len() == 0 {
            return;
        }
        assert!(!self.fin);
        self.buffer.push_back(data);
    }
}
