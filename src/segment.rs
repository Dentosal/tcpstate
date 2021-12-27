use core::fmt;

use alloc::vec::Vec;

#[derive(Clone, PartialEq, Eq)]
pub struct SegmentMeta {
    pub seqn: u32,
    pub ackn: u32,
    pub window: u16,
    pub flags: SegmentFlags,
    pub data: Vec<u8>,
}

impl SegmentMeta {
    /// Size in the sequence number space
    pub fn seq_size(&self) -> u32 {
        let mut result = self.data.len() as u32;
        if self.flags.contains(SegmentFlags::SYN) {
            result = result.wrapping_add(1);
        }
        if self.flags.contains(SegmentFlags::FIN) {
            result = result.wrapping_add(1);
        }
        result
    }
}

impl fmt::Debug for SegmentMeta {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut fs = f.debug_struct("SegmentMeta");
        fs.field("seqn", &self.seqn);
        fs.field("ackn", &self.ackn);
        // fs.field("window", &self.window);
        fs.field("flags", &self.flags);
        if self.data.is_empty() {
            fs.field("data", &[0u8; 0]);
        } else if let Ok(s) = core::str::from_utf8(&self.data) {
            fs.field("data", &s);
        } else {
            fs.field("data", &self.data);
        }
        fs.finish()
    }
}

bitflags::bitflags! {
    pub struct SegmentFlags: u16 {
        const FIN     = 1 << 0;
        const SYN     = 1 << 1;
        const RST     = 1 << 2;
        const PSH     = 1 << 3;
        const ACK     = 1 << 4;
        const URG     = 1 << 5;
        const ECE     = 1 << 6;
        const CWR     = 1 << 7;
        const NS      = 1 << 8;
    }
}
