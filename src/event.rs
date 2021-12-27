use hashbrown::{HashMap, HashSet};

use crate::result::Error;

//// MOCK /////////////////
use core::sync::atomic::{AtomicU64, Ordering};
static SEQ: AtomicU64 = AtomicU64::new(5);

//////////////////////////

/// Blocking operation returns an event cookie.
/// The request should be repeated whenever the cookie is [word].
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

#[derive(Debug, Clone)]
pub struct Events<Event: Copy + Eq + core::hash::Hash + core::fmt::Debug> {
    /// Suspended user queries waiting for condition
    pub(crate) suspended: HashSet<(Event, Cookie)>,
    /// Suspended user queries ready for continuing
    pub(crate) ready: HashMap<Cookie, Result<(), Error>>,
    next_cookie: Cookie,
}

impl<Event: Copy + Eq + core::hash::Hash + core::fmt::Debug> Events<Event> {
    pub fn new() -> Self {
        Self {
            suspended: HashSet::new(),
            ready: HashMap::new(),
            next_cookie: Cookie::ZERO,
        }
    }

    pub(crate) fn new_cookie(&mut self) -> Cookie {
        // let result = self.next_cookie;
        // self.next_cookie = self.next_cookie.next();
        // result
        self.next_cookie = self.next_cookie.next();
        self.next_cookie
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

    pub(crate) fn trigger<F>(&mut self, f: F, r: Result<(), Error>)
    where
        F: Fn(Event) -> bool,
    {
        for (_, cookie) in self.suspended.drain_filter(|(wait, _)| f(*wait)) {
            log::trace!("Triggered cookie {:?} result {:?}", cookie, r);
            self.ready.insert(cookie, r);
        }
    }

    /// Clear event if it is active.
    /// Returns `Ok(())` if the event was active, an `Err(())` otherwise.
    #[must_use]
    pub fn try_wait(&mut self, cookie: Cookie) -> Result<(), Error> {
        if let Some(result) = self.ready.remove(&cookie) {
            log::trace!("Clearing cookie {:?} (result {:?})", cookie, result);
            result
        } else {
            debug_assert!(
                self.suspended.iter().any(|(_, c)| *c == cookie),
                "Requested cookie is not present in either list"
            );
            log::trace!("Cookie {:?} was not ready", cookie);
            Err(Error::EventNotActive)
        }
    }
}
