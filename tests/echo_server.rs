#![deny(unused_must_use)]
#![feature(let_else)]

#[macro_use]
mod common;

use common::{socket::*, *};

use crossbeam_channel::bounded;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

#[test]
fn tcp_echo_server() {
    init();

    let net = sim_net::Network::new();

    static COMPLETED: AtomicUsize = AtomicUsize::new(0);

    const TEST_CASES: &[fn(&SocketCtx)] = &[|client| {
        let data = b"Testing!".repeat(2);

        client
            .call(|socket| socket.call_send(data.to_vec()))
            .expect("Error");

        client.call(|socket| socket.call_shutdown()).expect("Error");

        let mut buffer = [0; 1024];
        let n = client
            .call_ret(|socket| socket.call_recv(&mut buffer))
            .expect("Recv");

        assert_eq!(data, &buffer[..n]);
    }];

    let (stop_tx, stop_rx) = bounded(0);

    let server_addr = net.spawn_host(Box::new(move |host_handler, rx| {
        let listen = ListenCtx::new(5, host_handler, rx);

        let arc_socket = listen.socket.clone();
        let mut handlers = vec![thread::spawn(move || {
            stop_rx.recv().unwrap();
            arc_socket.lock().unwrap().call_close();
        })];

        loop {
            // TODO: try close handles of all finished threads
            let Ok((addr, accepted)) = listen.accept() else {
                break;
            };
            handlers.push(thread::spawn(move || {
                println!("New connection from {:?}", addr);
                accepted
                    .call(|socket| {
                        socket.options.nagle_delay = core::time::Duration::ZERO;
                        Ok(())
                    })
                    .unwrap();

                let mut buffer = [0; 64];
                loop {
                    let n = accepted
                        .call_ret(|socket| socket.call_recv(&mut buffer))
                        .expect("Recv");

                    accepted
                        .call(move |socket| {
                            let data = buffer[..n].to_vec();
                            socket.call_send(data)
                        })
                        .expect("Send");

                    if n < buffer.len() {
                        break;
                    }
                }

                accepted.call(|socket| socket.call_close()).expect("Send");
                println!("Closed connection from {:?}", addr);
            }));
        }
    }));

    for tc in TEST_CASES {
        let stop_tx = stop_tx.clone();
        net.spawn_host(Box::new(move |host_handler, rx| {
            let client = SocketCtx::new(host_handler, rx);
            client
                .call(|socket| {
                    socket.options.nagle_delay = core::time::Duration::ZERO;
                    socket.call_connect(server_addr)
                })
                .expect("Connect");

            tc(&client);
            let v = COMPLETED.fetch_add(1, Ordering::SeqCst);
            client.call(|socket| socket.call_close()).expect("Error");

            if v + 1 == TEST_CASES.len() {
                stop_tx.send(()).unwrap();
            }
        }));
    }

    net.wait();
}
