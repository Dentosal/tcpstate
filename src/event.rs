/// Blocking operation returns an event cookie.
/// The request should be repeated whenever the cookie is [word].

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Cookie(u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WaitFor {
    /// Wait until a receive buffer has enough data
    Recv { count: u32 },
    /// Wait until a segment is sent and acknowledged
    SendAck { seqn: u32 },
}
