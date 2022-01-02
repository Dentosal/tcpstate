use core::fmt::Debug;
use core::hash::Hash;
use core::time::Duration;

use crate::{Cookie, Error, SegmentMeta};

/// Implemented by the user of the library
pub trait UserData {
    type Time: UserTime;
    type Addr: Copy + Hash + Debug;

    fn new_seqn(&mut self) -> u32;
    fn send(&mut self, dst: Self::Addr, seg: SegmentMeta);
    fn event(&mut self, cookie: Cookie, result: Result<(), Error>);
    fn add_timeout(&mut self, instant: Self::Time);
}

pub trait UserTime: Copy + Ord {
    fn now() -> Self;

    #[must_use]
    fn add(&self, duration: Duration) -> Self;
}

#[cfg(feature = "std")]
impl UserTime for std::time::Instant {
    fn now() -> Self {
        std::time::Instant::now()
    }

    fn add(&self, duration: Duration) -> Self {
        std::time::Instant::checked_add(&self, duration).unwrap()
    }
}
