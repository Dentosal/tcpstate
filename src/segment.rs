use alloc::vec::Vec;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentMeta {
    pub seqn: u32,
    pub ackn: u32,
    pub window: u16,
    pub flags: SegmentFlags,
    pub data: Vec<u8>,
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
