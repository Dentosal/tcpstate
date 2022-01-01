use core::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(10_000);

pub(crate) fn random_seqnum() -> u32 {
    SEQ.fetch_add(10_000, Ordering::SeqCst) as u32
}

static ADDR: AtomicU64 = AtomicU64::new(10_000);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RemoteAddr(u64);
impl RemoteAddr {
    pub fn new() -> Self {
        Self(ADDR.fetch_add(1, Ordering::SeqCst))
    }
}
