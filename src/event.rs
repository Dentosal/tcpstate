/// Blocking operation returns an event cookie.
/// The request should be repeated whenever the cookie is [word].
//// MOCK /////////////////
use core::sync::atomic::{AtomicU64, Ordering};
static SEQ: AtomicU64 = AtomicU64::new(5);

//////////////////////////

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Cookie(u64);

impl Cookie {
    pub(crate) const ZERO: Self = Cookie(0);

    pub(crate) fn next(self) -> Cookie {
        // Self(self.0 + 1)
        // XXX: for debugging separate cookies per process
        Self(SEQ.fetch_add(1, Ordering::SeqCst))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum WaitUntil {
    /// Wait until connection has been established
    Established,
    /// Wait until unsegmentized output buffer is empty
    /// when closing
    OutputBufferClear,
    /// Wait until a receive buffer has enough data
    Recv { count: u32 },
    /// Wait until a segment is sent and acknowledged
    Acknowledged { seqn: u32 },
}
