use core::sync::atomic::{AtomicU64, Ordering};

use core::time::Duration;

static SEQ: AtomicU64 = AtomicU64::new(10_000);

pub(crate) fn random_seqnum() -> u32 {
    SEQ.fetch_add(10_000, Ordering::SeqCst) as u32
}

static ADDR: AtomicU64 = AtomicU64::new(10_000);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RemoteAddr(u64);
impl RemoteAddr {
    pub fn new() -> Self {
        Self(ADDR.fetch_add(1, Ordering::SeqCst))
    }
}

static NOW: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Instant(u64);
impl Instant {
    pub fn now() -> Self {
        Self(NOW.fetch_add(1, Ordering::SeqCst) << 32)
    }
    pub fn add(self, duration: Duration) -> Self {
        let m = duration.as_millis();
        assert!(m < u32::MAX as u128);
        Self(self.0 + m as u64)
    }
}
