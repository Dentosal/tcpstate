#![deny(unused_must_use)]

use tcpstate::*;

use crate::sim_net::{HostHandler, Incoming, RemoteAddr};

use crossbeam_channel::{bounded, Receiver, Sender};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use super::sim_net::Event;

pub struct ListenCtx {
    pub socket: Arc<Mutex<ListenSocket<HostHandler>>>,
    event: Event<(Cookie, Result<(), Error>)>,
    map: Arc<Mutex<HashMap<RemoteAddr, Sender<Incoming>>>>,
    _handle: JoinHandle<()>,
}

impl ListenCtx {
    pub fn new(backlog: usize, host_handler: HostHandler, rx: Receiver<Incoming>) -> Self {
        let event = host_handler.event.clone();
        let socket = Arc::new(Mutex::new(ListenSocket::call_listen(
            backlog,
            host_handler.clone(),
        )));

        let arc_socket = socket.clone();
        let map: Arc<Mutex<HashMap<RemoteAddr, Sender<Incoming>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let arc_map = map.clone();

        let _handle = thread::spawn(move || {
            while let Ok(inc) = rx.recv() {
                let m = arc_map.lock().unwrap();
                match inc {
                    Incoming::Packet(pkt) => {
                        if let Some(c_tx) = m.get(&pkt.src) {
                            c_tx.send(Incoming::Packet(pkt)).unwrap();
                        } else {
                            let mut s = arc_socket.lock().unwrap();
                            s.on_segment(pkt.src, pkt.seg);
                        }
                    }
                    Incoming::Timeout(now) => {
                        // let mut s = arc_socket.lock().unwrap();
                        // s.on_timeout(now);
                        for (_, item) in m.iter() {
                            item.send(Incoming::Timeout(now)).unwrap();
                        }
                    }
                }
            }
        });

        Self {
            socket,
            event,
            map,
            _handle,
        }
    }

    fn wait_event(&self) -> Result<(), Error> {
        self.event.wait().1
    }

    pub fn call_ret<T, F: FnMut(&mut ListenSocket<HostHandler>) -> Result<T, Error>>(
        &self,
        mut f: F,
    ) -> Result<T, Error> {
        loop {
            let r = {
                let mut guard = self.socket.lock().unwrap();
                f(&mut *guard)
            };
            match r {
                Err(Error::RetryAfter(_)) => self.wait_event()?,
                Err(Error::ContinueAfter(_)) => todo!("Error"),
                other => return other,
            }
        }
    }

    pub fn call<F: FnMut(&mut ListenSocket<HostHandler>) -> Result<(), Error>>(
        &self,
        mut f: F,
    ) -> Result<(), Error> {
        loop {
            let r = {
                let mut guard = self.socket.lock().unwrap();
                f(&mut *guard)
            };
            match r {
                Err(Error::RetryAfter(_)) => self.wait_event()?,
                Err(Error::ContinueAfter(_)) => {
                    return self.wait_event();
                }
                other => return other,
            }
        }
    }

    pub fn accept(&self) -> Result<(RemoteAddr, SocketCtx), Error> {
        let (addr, s) = self.call_ret(|s| {
            let mut host_handler = s.user_data.clone();
            host_handler.event = Event::new(); // Detach events
            s.call_accept(move || host_handler)
        })?;

        let (tx, rx) = bounded(10);
        let mut m = self.map.lock().unwrap();
        m.insert(addr, tx);
        Ok((addr, SocketCtx::from_socket(rx, s)))
    }
}

#[derive(Clone)]
pub struct SocketCtx {
    socket: Arc<Mutex<Socket<HostHandler>>>,
    event: Event<(Cookie, Result<(), Error>)>,
    _handle: Arc<JoinHandle<()>>,
}

impl SocketCtx {
    pub fn new(host_handler: HostHandler, rx: Receiver<Incoming>) -> Self {
        Self::from_socket(rx, Socket::new(host_handler))
    }

    pub fn from_socket(rx: Receiver<Incoming>, socket: Socket<HostHandler>) -> Self {
        let event = socket.user_data.event.clone();
        let socket = Arc::new(Mutex::new(socket));
        let arc_socket = socket.clone();
        let handle = thread::spawn(move || loop {
            match rx.recv().unwrap() {
                Incoming::Packet(pkt) => {
                    let mut s = arc_socket.lock().unwrap();
                    s.on_segment(pkt.seg);
                }
                Incoming::Timeout(now) => {
                    let mut s = arc_socket.lock().unwrap();
                    s.on_time_tick(now);
                }
            }
        });

        Self {
            socket,
            event,
            _handle: Arc::new(handle),
        }
    }

    fn wait_event(&self) -> Result<(), Error> {
        self.event.wait().1
    }

    pub fn call_ret<T, F: FnMut(&mut Socket<HostHandler>) -> Result<T, Error>>(
        &self,
        mut f: F,
    ) -> Result<T, Error> {
        loop {
            let r = {
                let mut guard = self.socket.lock().unwrap();
                f(&mut *guard)
            };
            match r {
                Err(Error::RetryAfter(_)) => self.wait_event()?,
                Err(Error::ContinueAfter(_)) => todo!("Error"),
                other => return other,
            }
        }
    }

    pub fn call<F: FnMut(&mut Socket<HostHandler>) -> Result<(), Error>>(
        &self,
        mut f: F,
    ) -> Result<(), Error> {
        loop {
            let r = {
                let mut guard = self.socket.lock().unwrap();
                f(&mut *guard)
            };
            match r {
                Err(Error::RetryAfter(_)) => self.wait_event()?,
                Err(Error::ContinueAfter(_)) => {
                    return self.wait_event();
                }
                other => return other,
            }
        }
    }
}
