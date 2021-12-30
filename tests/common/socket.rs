#![deny(unused_must_use)]

use tcpstate::{mock::*, *};

use crate::{
    sim_net::{HostHandler, Packet},
    *,
};

use crossbeam_channel::{bounded, Receiver, Sender};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

pub struct Event<T> {
    tx: Sender<T>,
    rx: Receiver<T>,
}
impl<T> Event<T> {
    pub fn new() -> Self {
        let (tx, rx) = bounded(0);
        Self { tx, rx }
    }

    pub fn trigger(&self, value: T) {
        self.tx.send(value).unwrap();
    }

    #[must_use]
    pub fn wait(&self) -> T {
        self.rx.recv().unwrap()
    }
}

pub struct ListenCtx {
    pub socket: Arc<Mutex<ListenSocket<HostHandler>>>,
    event: Arc<Event<Result<(), Error>>>,
    map: Arc<Mutex<HashMap<RemoteAddr, Sender<Packet>>>>,
    host_handler: HostHandler,
    _handle: JoinHandle<()>,
}

impl ListenCtx {
    pub fn new(backlog: usize, host_handler: HostHandler, rx: Receiver<sim_net::Packet>) -> Self {
        let event = Arc::new(Event::new());
        let arc_event = event.clone();
        let socket = Arc::new(Mutex::new(ListenSocket::call_listen(
            backlog,
            Box::new(move |_, result| {
                arc_event.trigger(result);
            }),
            host_handler.clone(),
        )));

        let arc_socket = socket.clone();
        let map: Arc<Mutex<HashMap<RemoteAddr, Sender<Packet>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let arc_map = map.clone();

        let _handle = thread::spawn(move || {
            while let Ok(pkt) = rx.recv() {
                let m = arc_map.lock().unwrap();
                if let Some(c_tx) = m.get(&pkt.src) {
                    c_tx.send(pkt).unwrap();
                } else {
                    let mut s = arc_socket.lock().unwrap();
                    s.on_segment(pkt.src, pkt.seg);
                }
            }
        });

        Self {
            socket,
            event,
            map,
            host_handler,
            _handle,
        }
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
                Err(Error::RetryAfter(_)) => self.event.wait()?,
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
                Err(Error::RetryAfter(_)) => self.event.wait()?,
                Err(Error::ContinueAfter(_)) => {
                    return self.event.wait();
                }
                other => return other,
            }
        }
    }

    pub fn accept(&self) -> Result<(RemoteAddr, SocketCtx), Error> {
        let event = Arc::new(Event::new());
        let arc_event = event.clone();
        let (addr, s) = self.call_ret(move |s| {
            let arc_event = arc_event.clone();
            s.call_accept(Box::new(move |_, r| arc_event.trigger(r)), || {
                self.host_handler.clone()
            })
        })?;

        let (tx, rx) = bounded(10);
        let mut m = self.map.lock().unwrap();
        m.insert(addr, tx);
        Ok((addr, SocketCtx::from_socket(rx, event, s)))
    }
}
pub struct SocketCtx {
    socket: Arc<Mutex<Socket<HostHandler>>>,
    event: Arc<Event<Result<(), Error>>>,
    _handle: JoinHandle<()>,
}

impl SocketCtx {
    pub fn new(host_handler: HostHandler, rx: Receiver<Packet>) -> Self {
        let event = Arc::new(Event::new());
        let arc_event = event.clone();
        Self::from_socket(
            rx,
            event,
            Socket::new(
                Box::new(move |_, result| {
                    arc_event.trigger(result);
                }),
                host_handler,
            ),
        )
    }

    pub fn from_socket(
        rx: Receiver<Packet>,
        event: Arc<Event<Result<(), Error>>>,
        socket: Socket<HostHandler>,
    ) -> Self {
        let socket = Arc::new(Mutex::new(socket));
        let arc_socket = socket.clone();
        let handle = thread::spawn(move || loop {
            let pkt = rx.recv().unwrap();
            let mut s = arc_socket.lock().unwrap();
            s.on_segment(pkt.seg);
        });

        Self {
            socket,
            event,
            _handle: handle,
        }
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
                Err(Error::RetryAfter(_)) => self.event.wait()?,
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
                Err(Error::RetryAfter(_)) => self.event.wait()?,
                Err(Error::ContinueAfter(_)) => {
                    return self.event.wait();
                }
                other => return other,
            }
        }
    }
}
