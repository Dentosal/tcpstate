#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidStateTransition,
    NotConnected,
    ConnectionReset,
    ConnectionClosing,
}
