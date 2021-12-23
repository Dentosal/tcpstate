use crate::event::Cookie;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidStateTransition,
    NotConnected,
    ConnectionReset,
    ConnectionClosing,
    /// Not an actual error
    /// Retry the tried call again after the cookie-event occurs.
    /// Other calls user to this socket MUST NOT be done until retry.
    RetryAfter(Cookie),
}
