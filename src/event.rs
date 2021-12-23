/// Blocking operation returns an event cookie.
/// The request should be repeated whenever the cookie is [word].

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Cookie(u64);

pub enum Event {
    DataAvailable,
}
