use core::time::Duration;

use crate::options::MAX_RTO;

fn duration_absdiff(a: Duration, b: Duration) -> Duration {
    if a > b {
        a - b
    } else {
        b - a
    }
}

/// Timings per https://datatracker.ietf.org/doc/html/rfc6298
#[derive(Debug, Clone)]
pub struct Timings {
    /// Retransmission timeout
    pub rto: Duration,
    /// Smoothed round-trip time
    srtt: Duration,
    /// RTT variation
    rtt_var: Duration,
}
impl Timings {
    pub fn update_rtt(&mut self, rtt: Duration) {
        if self.srtt == Duration::ZERO {
            self.srtt = rtt;
            self.rtt_var = rtt / 2;
        } else {
            self.rtt_var = (3 * self.rtt_var + duration_absdiff(self.srtt, rtt)) / 4;
            self.srtt = (7 * self.srtt + rtt) / 8;
            let clock_granularity = Duration::ZERO; // TODO
            self.rto = (self.srtt + (4 * self.rtt_var).max(clock_granularity))
                .min(Duration::SECOND)
                .max(MAX_RTO);
        }
    }
}
impl Default for Timings {
    fn default() -> Self {
        Self {
            rto: Duration::SECOND,
            srtt: Duration::ZERO,
            rtt_var: Duration::ZERO,
        }
    }
}
