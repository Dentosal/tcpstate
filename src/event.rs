use hashbrown::HashSet;

use crate::result::Error;
use crate::SeqN;

/// Blocking operation returns an event cookie.
/// The operation can block in either continue or retry mode,
/// and the corresponding action should be taken whn the cookie
/// is sent through `user_data.event()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Cookie(u64);

impl Cookie {
    pub(crate) const ZERO: Self = Cookie(0);

    pub(crate) fn next(self) -> Cookie {
        Self(self.0 + 1)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum WaitUntil {
    /// Wait until connection has been established
    Established,
    /// No data in tx buffer and no queued tx calls
    TxQueueEmpty,
    /// Tx side of buffer is closed (FIN ACK'd)
    TxClosed,
    /// Rx side of buffer is closed (FIN ACK'd)
    RxClosed,
    /// Tboth tx and rx sides of buffer is closed (FINs ACK'd)
    BothClosed,
    /// Wait until a receive buffer has enough data
    Recv { count: u32 },
    /// Wait until a segment is sent and acknowledged
    Acknowledged { seqn: SeqN },
}

pub struct Events<Event: Copy + Eq + core::hash::Hash + core::fmt::Debug> {
    /// Suspended user queries waiting for condition
    pub(crate) suspended: HashSet<(Event, Cookie)>,
    next_cookie: Cookie,
}

impl<Event: Copy + Eq + core::hash::Hash + core::fmt::Debug> Events<Event> {
    pub fn new() -> Self {
        Self {
            suspended: HashSet::new(),
            next_cookie: Cookie::ZERO,
        }
    }

    pub(crate) fn new_cookie(&mut self) -> Cookie {
        let result = self.next_cookie;
        self.next_cookie = self.next_cookie.next();
        result
    }

    pub(crate) fn return_retry_after<T>(&mut self, until: Event) -> Result<T, Error> {
        let cookie = self.new_cookie();
        log::trace!("Suspend/retry {:?} {:?}", until, cookie);
        self.suspended.insert((until, cookie));
        Err(Error::RetryAfter(cookie))
    }

    pub(crate) fn return_continue_after<T>(&mut self, until: Event) -> Result<T, Error> {
        let cookie = self.new_cookie();
        log::trace!("Suspend/continue {:?} {:?}", until, cookie);
        self.suspended.insert((until, cookie));
        Err(Error::ContinueAfter(cookie))
    }

    pub(crate) fn any_suspended<F>(&mut self, f: F) -> bool
    where
        F: Fn(Event) -> bool,
    {
        self.suspended.iter().any(|(e, _)| f(*e))
    }
}
