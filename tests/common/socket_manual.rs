//! Like socket, but:
//! * Data must be moved around manually
//! * Events are not automatically dispatched
//! * All APIs are nonblocking

#![deny(unused_must_use)]

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use tcpstate::{mock::*, *};

use crate::sim_net::Packet;

#[derive(Clone)]
pub struct ManualHandler {
    pub local: RemoteAddr,
    queue: Arc<Mutex<VecDeque<Packet>>>,
    event: Option<Result<(), Error>>,
}
impl ManualHandler {
    pub fn new() -> Self {
        Self {
            local: RemoteAddr::new(),
            queue: Arc::new(Mutex::new(VecDeque::new())),
            event: None,
        }
    }

    pub fn try_take(&self) -> Option<Packet> {
        self.queue.lock().unwrap().pop_front()
    }
}
impl UserData for ManualHandler {
    fn send(&mut self, dst: RemoteAddr, seg: SegmentMeta) {
        self.queue.lock().unwrap().push_back(Packet {
            src: self.local,
            dst,
            seg,
        });
    }

    fn event(&mut self, _: Cookie, result: Result<(), Error>) {
        self.event = Some(result);
    }
}

pub struct ListenCtx {
    pub socket: Arc<Mutex<ListenSocket<ManualHandler>>>,
    map: Arc<Mutex<HashMap<RemoteAddr, Box<dyn FnMut(Packet)>>>>,
    host_handler: ManualHandler,
}

impl ListenCtx {
    pub fn new(backlog: usize, host_handler: ManualHandler) -> (Self, impl FnMut(Packet)) {
        let socket = Arc::new(Mutex::new(ListenSocket::call_listen(
            backlog,
            host_handler.clone(),
        )));

        let arc_socket = socket.clone();
        let map: Arc<Mutex<HashMap<RemoteAddr, Box<dyn FnMut(Packet)>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let arc_map = map.clone();

        let rx_callback = move |pkt: Packet| {
            let mut m = arc_map.lock().unwrap();
            if let Some(on_segment) = m.get_mut(&pkt.src) {
                on_segment(pkt);
            } else {
                let mut s = arc_socket.lock().unwrap();
                s.on_segment(pkt.src, pkt.seg);
            }
        };

        (
            Self {
                socket,
                map,
                host_handler,
            },
            rx_callback,
        )
    }

    pub fn consume_event(&self) {
        let mut s = self.socket.lock().unwrap();
        let _ = s.user_data.event.take().expect("No event active");
    }

    pub fn call<T, F: FnMut(&mut ListenSocket<ManualHandler>) -> Result<T, Error>>(
        &self,
        mut f: F,
    ) -> Result<T, Error> {
        let mut guard = self.socket.lock().unwrap();
        f(&mut *guard)
    }

    pub fn accept(&self) -> Result<(RemoteAddr, SocketCtx), Error> {
        let (addr, s) = self.call(move |s| s.call_accept(|| self.host_handler.clone()))?;

        let (socket, s_on_packet) = SocketCtx::from_socket(s);
        let mut m = self.map.lock().unwrap();
        m.insert(addr, Box::new(s_on_packet));
        Ok((addr, socket))
    }
}
pub struct SocketCtx {
    pub socket: Arc<Mutex<Socket<ManualHandler>>>,
}

impl SocketCtx {
    pub fn new(host_handler: ManualHandler) -> (Self, impl FnMut(Packet)) {
        Self::from_socket(Socket::new(host_handler))
    }

    pub fn from_socket(socket: Socket<ManualHandler>) -> (Self, impl FnMut(Packet)) {
        let socket = Arc::new(Mutex::new(socket));
        let arc_socket = socket.clone();
        let rx_callback = move |pkt: Packet| {
            let mut s = arc_socket.lock().unwrap();
            s.on_segment(pkt.seg);
        };

        (Self { socket }, rx_callback)
    }

    pub fn consume_event(&self) {
        let mut s = self.socket.lock().unwrap();
        let _ = s.user_data.event.take().expect("No event active");
    }

    pub fn call_close(&self) -> Result<(), Error> {
        let mut guard = self.socket.lock().unwrap();
        guard.call_close()
    }

    pub fn call_shutdown(&self) -> Result<(), Error> {
        let mut guard = self.socket.lock().unwrap();
        guard.call_shutdown()
    }

    pub fn call_send(&self, data: Vec<u8>) -> Result<(), Error> {
        let mut guard = self.socket.lock().unwrap();
        guard.call_send(data)
    }

    pub fn call_recv(&self, buffer: &mut [u8]) -> Result<usize, Error> {
        let mut guard = self.socket.lock().unwrap();
        guard.call_recv(buffer)
    }

    pub fn recv_available(&self) -> usize {
        let mut guard = self.socket.lock().unwrap();
        guard.recv_available()
    }

    pub fn on_time_tick(&self, time: Instant) {
        let mut guard = self.socket.lock().unwrap();
        guard.on_time_tick(time);
    }

    pub fn state(&self) -> ConnectionState {
        let guard = self.socket.lock().unwrap();
        guard.state()
    }
}
