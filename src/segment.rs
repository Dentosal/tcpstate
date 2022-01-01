use alloc::vec::Vec;
use core::fmt;

#[derive(Clone, PartialEq, Eq)]
pub struct SegmentMeta {
    pub seqn: SeqN,
    pub ackn: SeqN,
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

    /// End of the seqn range used by this packet
    pub fn last_seqn(&self) -> SeqN {
        self.seqn_after().wrapping_sub(1)
    }

    /// SeqN just after this segment
    ///
    /// ## Example
    ///
    /// Packet at seqn=10 with data "123" and a FIN bit looks like this:
    ///
    /// |10 |11 |12 |13 |
    /// |---|---|---|---|
    /// | 1 | 2 | 3 |FIN|
    /// |---|---|---|---|
    ///
    /// So this function would return 14
    pub fn seqn_after(&self) -> SeqN {
        self.seqn.wrapping_add(self.seq_size())
    }
}

impl fmt::Debug for SegmentMeta {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut fs = f.debug_struct("SegmentMeta");
        fs.field("seqn", &self.seqn);
        fs.field("ackn", &self.ackn);
        fs.field("window", &self.window);
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

/// A TCP sequence number, handles wrap-around
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SeqN(u32);

impl SeqN {
    pub const ZERO: Self = Self(0);

    pub fn new(value: u32) -> Self {
        Self(value)
    }

    pub fn raw(self) -> u32 {
        self.0
    }

    pub fn wrapping_add(self, v: u32) -> Self {
        Self(self.0.wrapping_add(v))
    }

    pub fn wrapping_sub(self, v: u32) -> Self {
        Self(self.0.wrapping_sub(v))
    }

    pub fn in_range_inclusive(self, start: Self, end: Self) -> bool {
        if start == end {
            self.0 == start.0
        } else if start.0 < end.0 {
            start.0 <= self.0 && self.0 <= end.0
        } else {
            start.0 <= self.0 || self.0 <= end.0
        }
    }

    pub fn lt(self, other: Self) -> bool {
        self.in_range_inclusive(other.wrapping_sub(u32::MAX / 2), other)
    }

    pub fn lte(self, other: Self) -> bool {
        self == other || self.lt(other)
    }
}

impl fmt::Display for SeqN {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}
