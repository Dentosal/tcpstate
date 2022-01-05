use core::time::Duration;

pub(crate) const MAX_RTO: Duration = Duration::from_secs(120);
pub(crate) const NAGLE_DELAY_DEFAULT: Duration = Duration::from_millis(200);

// TODO: move these constants to a config parameter?

pub const MAX_SEGMENT_LIFETIME: Duration = Duration::from_secs(120);
pub(crate) const MAX_SEGMENT_SIZE: usize = 512;
pub(crate) const SLOW_START_TRESHOLD: u16 = 512;

pub(crate) const INITIAL_WINDOW_SIZE: u16 = 0x1000;

#[derive(Debug, Clone)]
pub struct SocketOptions {
    /// Set to Duration::ZERO for TCP_NODELAY
    pub nagle_delay: core::time::Duration,
}
impl Default for SocketOptions {
    fn default() -> Self {
        Self {
            // TODO: Nagle's algorithm is not supported yet
            // nagle_delay: NAGLE_DELAY_DEFAULT,
            nagle_delay: Duration::ZERO,
        }
    }
}
