use crossbeam_channel::{bounded, Receiver, Sender};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::thread::{self, JoinHandle};

use tcpstate::{mock::RemoteAddr, Cookie, Error, SegmentMeta};

pub struct Packet {
    pub src: RemoteAddr,
    pub dst: RemoteAddr,
    pub seg: SegmentMeta,
}

#[derive(Clone)]
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
        println!("trigger event");
        self.tx.send(value).unwrap();
    }

    #[must_use]
    pub fn wait(&self) -> T {
        println!("wait event");
        self.rx.recv().unwrap()
    }
}

#[derive(Clone)]
pub struct HostHandler {
    local: RemoteAddr,
    hosts: Arc<RwLock<HashMap<RemoteAddr, Sender<Packet>>>>,
    pub(crate) event: Event<(Cookie, Result<(), Error>)>,
}

impl tcpstate::UserData for HostHandler {
    fn send(&mut self, dst: RemoteAddr, seg: SegmentMeta) {
        if let Some(host) = self.hosts.read().unwrap().get(&dst) {
            host.send(Packet {
                src: self.local,
                dst,
                seg,
            })
            .unwrap();
        } else {
            panic!("Host {:?} doesn't exist", dst)
        }
    }

    fn event(&mut self, cookie: Cookie, result: Result<(), Error>) {
        self.event.trigger((cookie, result));
    }
}

#[derive(Default)]
pub struct Network {
    hosts: Arc<RwLock<HashMap<RemoteAddr, Sender<Packet>>>>,
    handles: Arc<RwLock<HashMap<RemoteAddr, JoinHandle<()>>>>,
}
impl Network {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn spawn_host(
        &self,
        f: Box<dyn FnOnce(HostHandler, Receiver<Packet>) + Send>,
    ) -> RemoteAddr {
        let addr = RemoteAddr::new();
        let (tx, rx) = bounded(10);

        let hosts = self.hosts.clone();
        self.hosts.write().unwrap().insert(addr, tx);
        self.handles.write().unwrap().insert(
            addr,
            thread::spawn(move || {
                f(
                    HostHandler {
                        local: addr,
                        hosts,
                        event: Event::new(),
                    },
                    rx,
                )
            }),
        );
        addr
    }

    pub fn wait(self) {
        let mut handles = self.handles.write().unwrap();
        for (_, handle) in handles.drain() {
            handle.join().unwrap();
        }
    }
}
