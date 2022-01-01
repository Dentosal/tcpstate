use core::time::Duration;

use crate::mock::RemoteAddr;
use crate::{Cookie, Error, SegmentMeta};

pub trait UserData {
    type Time: UserTime;

    fn send(&mut self, dst: RemoteAddr, seg: SegmentMeta);
    fn event(&mut self, cookie: Cookie, result: Result<(), Error>);
    fn add_timeout(&mut self, instant: Self::Time);
}

pub trait UserTime: Copy + Ord {
    fn now() -> Self;
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
