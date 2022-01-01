use core::time::Duration;

use crate::state::ConnectionState;
use crate::{Socket, UserData, UserTime};

/// Timings per https://datatracker.ietf.org/doc/html/rfc6298
#[derive(Debug, Clone)]
pub struct Timers<T: UserTime> {
    pub(crate) re_tx: Option<T>,
    pub(crate) timewait: Option<T>,
    pub(crate) usertime: Option<T>,
}

impl<T: UserTime> Default for Timers<T> {
    fn default() -> Self {
        Self {
            re_tx: None,
            timewait: None,
            usertime: None,
        }
    }
}

impl<T: UserTime> Timers<T> {
    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }
}

impl<U: UserData> Socket<U> {
    pub(crate) fn set_timer_re_tx(&mut self, duration: Duration) {
        let deadline = U::Time::now().add(duration);
        self.timers.re_tx = Some(deadline);
        self.user_data.add_timeout(deadline);
    }

    pub(crate) fn set_timer_timewait(&mut self, duration: Duration) {
        let deadline = U::Time::now().add(duration);
        self.timers.timewait = Some(deadline);
        self.user_data.add_timeout(deadline);
    }

    pub(crate) fn set_timer_usertime(&mut self, duration: Duration) {
        let deadline = U::Time::now().add(duration);
        self.timers.usertime = Some(deadline);
        self.user_data.add_timeout(deadline);
    }

    pub(crate) fn on_timer_re_tx(&mut self) {
        log::trace!("on_timer_re_tx");
        match self.connection_state {
            ConnectionState::Listen
            | ConnectionState::SynSent
            | ConnectionState::SynReceived
            | ConnectionState::Established { .. } => {
                // TODO: recalc RTO?
                // TODO: backoff
                if let Some(seg) = self.tx.re_tx.front() {
                    log::trace!("Resending {:?}", seg);
                    self.user_data
                        .send(self.remote.expect("No remote set"), seg.clone());
                    self.set_timer_re_tx(self.timings.rto);
                }
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
    pub fn on_time_tick(&mut self, time: U::Time) {
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
