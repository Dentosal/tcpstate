use crate::event::Cookie;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Error {
    InvalidStateTransition,
    NotConnected,
    ConnectionReset,
    OutputClosed,
    ConnectionClosing,
    ListenClosed,
    TimedOut,
    /// This socket is closed, and the operation is not accepted
    Closed,
    /// This socket is listening, and the operation is not accepted
    Listening,
    EventNotActive,
    /// Not an actual error
    /// Retry the tried call again after the cookie-event occurs.
    /// Other calls user to this socket MUST NOT be done until retry.
    RetryAfter(Cookie),
    /// Not an actual error
    /// Contine execution only after the cookie-event occurs.
    /// Other calls user to this socket MUST NOT be done until the event.
    ContinueAfter(Cookie),
}
