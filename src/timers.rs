use crate::mock::Instant;

use crate::state::ConnectionState;
use crate::Socket;

/// Timings per https://datatracker.ietf.org/doc/html/rfc6298
#[derive(Debug, Clone, Default)]
pub struct Timers {
    pub(crate) re_tx: Option<Instant>,
    pub(crate) timewait: Option<Instant>,
    pub(crate) usertime: Option<Instant>,
}
impl Timers {
    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }
}

impl Socket {
    pub(crate) fn on_timer_re_tx(&mut self) {
        log::trace!("on_timer_re_tx");
        match self.connection_state {
            ConnectionState::Listen
            | ConnectionState::SynSent
            | ConnectionState::SynReceived
            | ConnectionState::Established { .. } => {
                todo!("CALC RTO");
                todo!("REARM TIMER");
                todo!("SEND FROM RE_TX QUEUE");
            }
            ConnectionState::Reset => {}
            ConnectionState::TimeWait | ConnectionState::Closed => {
                unreachable!()
            }
        }
    }

    pub(crate) fn on_timer_timewait(&mut self) {
        log::trace!("on_timer_timewait");
        assert!(self.connection_state == ConnectionState::TimeWait);
        self.clear();
    }

    pub(crate) fn on_timer_usertime(&mut self) {
        log::trace!("on_timer_usertime");
        todo!("Signal error to all outstanding calls");
        self.clear();
    }

    /// To be called whenever a timer expires, or simply periodically
    pub fn on_time_tick(&mut self, time: Instant) {
        log::trace!("state: {:?}", self.connection_state);
        log::trace!("on_timeout");

        if self.timers.re_tx.map_or(false, |t| t <= time) {
            self.timers.re_tx = None;
            self.on_timer_re_tx();
        }

        if self.timers.timewait.map_or(false, |t| t <= time) {
            self.timers.timewait = None;
            self.on_timer_timewait();
        }

        if self.timers.usertime.map_or(false, |t| t <= time) {
            self.timers.usertime = None;
            self.on_timer_usertime();
        }
    }
}
