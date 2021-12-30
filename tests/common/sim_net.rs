use crossbeam_channel::{bounded, Receiver, Sender};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::thread::{self, JoinHandle};

use tcpstate::{mock::RemoteAddr, SegmentMeta};

pub struct Packet {
    pub src: RemoteAddr,
    pub dst: RemoteAddr,
    pub seg: SegmentMeta,
}

#[derive(Clone)]
pub struct HostHandler {
    local: RemoteAddr,
    hosts: Arc<RwLock<HashMap<RemoteAddr, Sender<Packet>>>>,
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
            thread::spawn(move || f(HostHandler { local: addr, hosts }, rx)),
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
