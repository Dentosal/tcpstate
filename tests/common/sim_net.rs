use crossbeam_channel::{bounded, Receiver, RecvTimeoutError, Sender};
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::sync::atomic::AtomicU32;
use std::sync::{
    atomic::{AtomicU64, AtomicUsize, Ordering},
    Arc, RwLock,
};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use tcpstate::{Cookie, Error, SegmentMeta};

#[derive(Debug)]
pub enum Incoming {
    Packet(Packet),
    Timeout(Instant),
}

#[derive(Debug)]
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

static ADDR: AtomicU64 = AtomicU64::new(10_000);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RemoteAddr(u64);
impl RemoteAddr {
    pub fn new() -> Self {
        Self(ADDR.fetch_add(1, Ordering::SeqCst))
    }
}

pub static INIT_SEQN: AtomicU32 = AtomicU32::new(0);

#[derive(Clone)]
pub struct HostHandler {
    local: RemoteAddr,
    pub(crate) event: Event<(Cookie, Result<(), Error>)>,
    network: Arc<RwLock<NetworkInner>>,
}

impl tcpstate::UserData for HostHandler {
    type Time = Instant;
    type Addr = RemoteAddr;

    fn new_seqn(&mut self) -> u32 {
        INIT_SEQN.load(Ordering::SeqCst)
    }

    fn send(&mut self, dst: RemoteAddr, seg: SegmentMeta) {
        let network = self.network.read().unwrap();
        if let Some(host) = network.hosts.get(&dst) {
            match generate_errror(network.error_profile) {
                NetError::Ok => {
                    host.send(Incoming::Packet(Packet {
                        src: self.local,
                        dst,
                        seg,
                    }))
                    .unwrap();
                }
                NetError::Drop => {
                    println!("Network error: drop {:?}", seg);
                }
            }
        } else {
            panic!("Host {:?} doesn't exist", dst)
        }
    }

    fn event(&mut self, cookie: Cookie, result: Result<(), Error>) {
        self.event.trigger((cookie, result));
    }

    fn add_timeout(&mut self, deadline: Self::Time) {
        let network = self.network.read().unwrap();
        network.tx_schedule.send((deadline, self.local)).unwrap();
    }
}

#[derive(Debug, Clone, Copy)]
pub enum ErrorProfile {
    NoErrors,
    HighPacketLoss,
}
impl Default for ErrorProfile {
    fn default() -> Self {
        Self::NoErrors
    }
}

#[derive(Debug, Clone, Copy)]
pub enum NetError {
    Ok,
    Drop,
}

static PACKET_COUNT: AtomicUsize = AtomicUsize::new(0);

fn generate_errror(p: ErrorProfile) -> NetError {
    let c = PACKET_COUNT.fetch_add(1, Ordering::Relaxed);
    match p {
        ErrorProfile::NoErrors => NetError::Ok,
        ErrorProfile::HighPacketLoss => {
            if c % 5 == 0 {
                NetError::Drop
            } else {
                NetError::Ok
            }
        }
    }
}
pub struct NetworkInner {
    hosts: HashMap<RemoteAddr, Sender<Incoming>>,
    error_profile: ErrorProfile,
    tx_schedule: Sender<(Instant, RemoteAddr)>,
}
impl NetworkInner {
    pub fn new(tx_schedule: Sender<(Instant, RemoteAddr)>) -> Self {
        Self {
            hosts: Default::default(),
            error_profile: Default::default(),
            tx_schedule,
        }
    }
}

pub struct Network {
    handles: Arc<RwLock<HashMap<RemoteAddr, JoinHandle<()>>>>,
    inner: Arc<RwLock<NetworkInner>>,
    _timer_handle: JoinHandle<()>,
}
impl Network {
    pub fn new() -> Self {
        let (tx_schedule, rx_schedule) = bounded(0);
        let inner = Arc::new(RwLock::new(NetworkInner::new(tx_schedule)));
        let arc_inner = inner.clone();
        let _timer_handle = thread::spawn(move || {
            let mut items: BinaryHeap<(Reverse<Instant>, RemoteAddr)> = BinaryHeap::new();
            loop {
                let (sched, addr) = if let Some((Reverse(deadline), addr)) = items.pop() {
                    match rx_schedule.recv_deadline(deadline) {
                        Ok(sched) => sched,
                        Err(RecvTimeoutError::Timeout) => {
                            let network = arc_inner.read().unwrap();
                            network
                                .hosts
                                .get(&addr)
                                .expect("Host not found")
                                .send(Incoming::Timeout(deadline))
                                .unwrap();
                            continue;
                        }
                        Err(RecvTimeoutError::Disconnected) => panic!("disconnected"),
                    }
                } else {
                    rx_schedule.recv().unwrap()
                };
                log::trace!("Adding timer {:?}", sched);
                items.push((Reverse(sched), addr));
            }
        });

        Self {
            handles: Default::default(),
            inner,
            _timer_handle,
        }
    }

    pub fn set_error_profile(&self, error_profile: ErrorProfile) {
        self.inner.write().unwrap().error_profile = error_profile;
    }

    pub fn spawn_host(
        &self,
        f: Box<dyn FnOnce(HostHandler, Receiver<Incoming>) + Send>,
    ) -> RemoteAddr {
        let addr = RemoteAddr::new();
        let (tx, rx) = bounded(10);

        let inner = self.inner.clone();
        inner.write().unwrap().hosts.insert(addr, tx);
        self.handles.write().unwrap().insert(
            addr,
            thread::spawn(move || {
                f(
                    HostHandler {
                        local: addr,
                        event: Event::new(),
                        network: inner,
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
