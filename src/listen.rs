use alloc::collections::VecDeque;

use crate::event::Events;
use crate::*;

pub struct ListenSocket<U: UserData> {
    queue: VecDeque<(U::Addr, SegmentMeta)>,
    backlog: usize,
    open: bool,
    events: Events<()>,
    pub(crate) options: SocketOptions,
    pub(crate) user_data: U,
}

impl<U: UserData> ListenSocket<U> {
    pub fn user_data(&self) -> &U {
        &self.user_data
    }

    pub fn user_data_mut(&mut self) -> &mut U {
        &mut self.user_data
    }

    pub fn options(&self) -> &SocketOptions {
        &self.options
    }

    pub fn options_mut(&mut self) -> &mut SocketOptions {
        &mut self.options
    }

    /// New socket in initial state
    pub fn call_listen(backlog: usize, user_data: U) -> Self {
        Self {
            queue: VecDeque::new(),
            backlog: backlog.max(1),
            open: true,
            events: Events::new(),
            options: SocketOptions::default(),
            user_data,
        }
    }

    /// Request closing the socket
    /// Will cancel ongoing listen if any in progress
    pub fn call_close(&mut self) {
        log::trace!("call_close");

        self.open = false;

        for (_, cookie) in self.events.suspended.drain() {
            log::trace!("Closing pending listen call {:?}", cookie);
            self.user_data.event(cookie, Err(Error::ListenClosed));
        }

        // RST all queued connections
        if !self.queue.is_empty() {
            log::trace!("Listen socket closed, sending RST to all queued connections");
        }
        for (addr, seg) in self.queue.drain(..) {
            self.user_data.send(
                addr,
                SegmentMeta {
                    seqn: seg.ackn,
                    ackn: SeqN::ZERO,
                    window: 0,
                    flags: SegmentFlags::RST,
                    data: Vec::new(),
                },
            );
        }
    }

    /// Accept a new connection from the backlog
    pub fn call_accept(
        &mut self,
        make_user_data: fn(&ListenSocket<U>) -> U,
    ) -> Result<(U::Addr, Connection<U>), Error> {
        log::trace!("call_accept");

        if !self.open {
            return Err(Error::ConnectionClosing);
        }

        if let Some((addr, seg)) = self.queue.pop_front() {
            let user_data = make_user_data(self);
            let mut socket = Connection::new(addr, user_data);
            socket.options = self.options.clone(); // Inherits options
            socket.set_state(ConnectionState::Listen);
            socket.on_segment(seg);
            Ok((addr, socket))
        } else {
            self.events.return_retry_after(())
        }
    }

    /// Called on incoming segment
    /// See RFC 793 page 64
    pub fn on_segment(&mut self, addr: U::Addr, seg: SegmentMeta) {
        log::trace!("on_segment {:?}", seg);

        if seg.flags.contains(SegmentFlags::RST) {
            return;
        }

        if seg.flags.contains(SegmentFlags::ACK) || !self.open {
            if self.open {
                log::trace!("Listen socket received ACK, responding with RST");
            } else {
                log::trace!("Listen socket iu closed, responding with RST");
            }
            self.user_data.send(
                addr,
                SegmentMeta {
                    seqn: seg.ackn,
                    ackn: SeqN::ZERO,
                    window: 0,
                    flags: SegmentFlags::RST,
                    data: Vec::new(),
                },
            );
            return;
        }

        if !seg.flags.contains(SegmentFlags::SYN) {
            log::warn!("Invalid packet received, dropping");
            return;
        }

        let queue_full = self.queue.len() >= self.backlog;

        if queue_full {
            log::debug!("Listen backlog full, dropping incoming connection");
            self.user_data.send(
                addr,
                SegmentMeta {
                    seqn: seg.ackn.wrapping_add(1),
                    ackn: SeqN::ZERO,
                    window: 0,
                    flags: SegmentFlags::ACK | SegmentFlags::RST,
                    data: Vec::new(),
                },
            );
            return;
        }

        self.queue.push_back((addr, seg));

        // Awake an arbitrarily selected single listener
        if let Some((_, cookie)) = self.events.suspended.iter().next().copied() {
            log::trace!("Awaking pending close call call {:?}", cookie);
            self.events.suspended.remove(&((), cookie));
            self.user_data.event(cookie, Ok(()));
        };
    }
}
