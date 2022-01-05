//! TCP as per https://www.ietf.org/rfc/rfc793.txt

#![cfg_attr(not(any(test, feature = "std")), no_std)]
#![feature(default_free_fn, duration_constants, let_else, drain_filter)]
#![allow(dead_code, unreachable_code)]
#![deny(unused_must_use, unconditional_recursion)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::vec::Vec;

pub mod options;

mod connection;
mod event;
mod listen;
mod queue;
mod result;
mod rtt;
mod rx;
mod segment;
mod state;
mod timers;
mod tx;
mod user_data;

pub use connection::Connection;
pub use event::Cookie;
pub use listen::ListenSocket;
pub use options::SocketOptions;
pub use result::Error;
pub use rtt::Timings;
pub use segment::{SegmentFlags, SegmentMeta, SeqN};
pub use state::ConnectionState;
pub use user_data::{UserData, UserTime};

/// Response to a packet when the socket is closed
/// RFC 793 page 64
pub fn response_to_closed(seg: SegmentMeta) -> Option<SegmentMeta> {
    if seg.flags.contains(SegmentFlags::RST) {
        None
    } else if seg.flags.contains(SegmentFlags::ACK) {
        Some(SegmentMeta {
            seqn: SeqN::ZERO,
            ackn: seg.seqn.wrapping_add(seg.data.len().try_into().unwrap()),
            window: 0,
            flags: SegmentFlags::RST | SegmentFlags::ACK,
            data: Vec::new(),
        })
    } else {
        Some(SegmentMeta {
            seqn: seg.ackn,
            ackn: SeqN::ZERO,
            window: 0,
            flags: SegmentFlags::RST,
            data: Vec::new(),
        })
    }
}

/// Common data for all socket types
struct SocketCommon<U: UserData> {
    user_data: U,
    options: SocketOptions,
}

enum SocketInner<U: UserData> {
    Closed(SocketCommon<U>),
    Connection(Connection<U>),
    Listen(ListenSocket<U>),
    /// Used as a placeholder when swapping items out of the non-copy field
    SwapTemp,
}
impl<U: UserData> SocketInner<U> {
    /// Deconstructs self and returns user_data.
    /// Used only as a helper for `inner`-replacing functions.
    fn extract_common(self) -> SocketCommon<U> {
        match self {
            SocketInner::SwapTemp => unreachable!(),
            SocketInner::Closed(common) => common,
            SocketInner::Listen(s) => SocketCommon {
                user_data: s.user_data,
                options: s.options,
            },
            SocketInner::Connection(s) => SocketCommon {
                user_data: s.user_data,
                options: s.options,
            },
        }
    }
}

pub struct Socket<U: UserData> {
    inner: SocketInner<U>,
}

impl<U: UserData> Socket<U> {
    pub fn new(user_data: U) -> Self {
        Self {
            inner: SocketInner::Closed(SocketCommon {
                user_data,
                options: SocketOptions::default(),
            }),
        }
    }

    pub fn state(&self) -> ConnectionState {
        match &self.inner {
            SocketInner::SwapTemp => unreachable!(),
            SocketInner::Closed(_) => ConnectionState::Closed,
            SocketInner::Listen(_) => ConnectionState::Listen,
            SocketInner::Connection(s) => s.state(),
        }
    }

    pub fn options(&self) -> &SocketOptions {
        match &self.inner {
            SocketInner::SwapTemp => unreachable!(),
            SocketInner::Closed(s) => &s.options,
            SocketInner::Listen(s) => &s.options,
            SocketInner::Connection(s) => &s.options,
        }
    }

    pub fn options_mut(&mut self) -> &mut SocketOptions {
        match &mut self.inner {
            SocketInner::SwapTemp => unreachable!(),
            SocketInner::Closed(s) => &mut s.options,
            SocketInner::Listen(s) => &mut s.options,
            SocketInner::Connection(s) => &mut s.options,
        }
    }

    pub fn user_data(&self) -> &U {
        match &self.inner {
            SocketInner::SwapTemp => unreachable!(),
            SocketInner::Closed(s) => &s.user_data,
            SocketInner::Listen(s) => &s.user_data,
            SocketInner::Connection(s) => &s.user_data,
        }
    }

    pub fn user_data_mut(&mut self) -> &mut U {
        match &mut self.inner {
            SocketInner::SwapTemp => unreachable!(),
            SocketInner::Closed(s) => &mut s.user_data,
            SocketInner::Listen(s) => &mut s.user_data,
            SocketInner::Connection(s) => &mut s.user_data,
        }
    }

    pub fn call_connect(&mut self, remote: U::Addr) -> Result<(), Error> {
        if self.state() != ConnectionState::Closed {
            return Err(Error::InvalidStateTransition);
        }

        // Do some swapping so that we don't need Copy property for UserData
        let inner = core::mem::replace(&mut self.inner, SocketInner::SwapTemp);
        let SocketCommon { user_data, options } = inner.extract_common();

        let mut c = Connection::new(remote, user_data);
        c.options = options;
        let result = c.call_connect();
        self.inner = SocketInner::Connection(c);
        result
    }

    pub fn call_listen(&mut self, backlog: usize) -> Result<(), Error> {
        if self.state() != ConnectionState::Closed {
            return Err(Error::InvalidStateTransition);
        }

        // Do some swapping so that we don't need Copy property for UserData
        let inner = core::mem::replace(&mut self.inner, SocketInner::SwapTemp);
        let SocketCommon { user_data, .. } = inner.extract_common();
        self.inner = SocketInner::Listen(ListenSocket::call_listen(backlog, user_data));
        Ok(())
    }

    pub fn call_accept(
        &mut self,
        make_user_data: fn(&ListenSocket<U>) -> U,
    ) -> Result<(U::Addr, Connection<U>), Error> {
        match &mut self.inner {
            SocketInner::SwapTemp => unreachable!(),
            SocketInner::Closed(_) => Err(Error::Closed),
            SocketInner::Connection(_) => Err(Error::InvalidStateTransition),
            SocketInner::Listen(s) => s.call_accept(make_user_data),
        }
    }

    pub fn call_close(&mut self) -> Result<(), Error> {
        match &mut self.inner {
            SocketInner::SwapTemp => unreachable!(),
            SocketInner::Closed(_) => Err(Error::Closed),
            SocketInner::Connection(s) => s.call_close(),
            SocketInner::Listen(s) => {
                s.call_close();
                Ok(())
            }
        }
    }

    pub fn call_abort(&mut self) -> Result<(), Error> {
        match &mut self.inner {
            SocketInner::SwapTemp => unreachable!(),
            SocketInner::Closed(_) => Err(Error::Closed),
            SocketInner::Connection(s) => s.call_abort(),
            SocketInner::Listen(s) => {
                s.call_close();
                Ok(())
            }
        }
    }

    pub fn call_shutdown(&mut self) -> Result<(), Error> {
        match &mut self.inner {
            SocketInner::SwapTemp => unreachable!(),
            SocketInner::Closed(_) => Err(Error::Closed),
            SocketInner::Listen(_) => Err(Error::Listening),
            SocketInner::Connection(s) => s.call_shutdown(),
        }
    }

    pub fn call_send(&mut self, data: Vec<u8>) -> Result<(), Error> {
        match &mut self.inner {
            SocketInner::SwapTemp => unreachable!(),
            SocketInner::Closed(_) => Err(Error::Closed),
            SocketInner::Listen(_) => Err(Error::Listening),
            SocketInner::Connection(s) => s.call_send(data),
        }
    }

    pub fn call_recv(&mut self, buffer: &mut [u8]) -> Result<usize, Error> {
        match &mut self.inner {
            SocketInner::SwapTemp => unreachable!(),
            SocketInner::Closed(_) => Err(Error::Closed),
            SocketInner::Listen(_) => Err(Error::Listening),
            SocketInner::Connection(s) => s.call_recv(buffer),
        }
    }

    pub fn recv_available(&mut self) -> Result<usize, Error> {
        match &mut self.inner {
            SocketInner::SwapTemp => unreachable!(),
            SocketInner::Closed(_) => Err(Error::Closed),
            SocketInner::Listen(_) => Err(Error::Listening),
            SocketInner::Connection(s) => Ok(s.recv_available()),
        }
    }

    pub fn on_segment(&mut self, src: U::Addr, seg: SegmentMeta) {
        match &mut self.inner {
            SocketInner::SwapTemp => unreachable!(),
            SocketInner::Closed(s) => {
                if let Some(resp) = response_to_closed(seg) {
                    s.user_data.send(src, resp);
                }
            }
            SocketInner::Listen(s) => s.on_segment(src, seg),
            SocketInner::Connection(s) => s.on_segment(seg),
        }
    }

    pub fn on_time_tick(&mut self, time: U::Time) {
        match &mut self.inner {
            SocketInner::SwapTemp => unreachable!(),
            SocketInner::Closed(_) => {}
            SocketInner::Listen(_) => {}
            SocketInner::Connection(s) => s.on_time_tick(time),
        }
    }
}

impl<U: UserData> core::convert::From<Connection<U>> for Socket<U> {
    fn from(value: Connection<U>) -> Self {
        Self {
            inner: SocketInner::Connection(value),
        }
    }
}

impl<U: UserData> core::convert::From<ListenSocket<U>> for Socket<U> {
    fn from(value: ListenSocket<U>) -> Self {
        Self {
            inner: SocketInner::Listen(value),
        }
    }
}
