#![deny(unused_must_use)]
#![feature(let_else)]

#[macro_use]
mod common;

use common::{socket::*, *};

use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn tcp_echo_server() {
    init();

    let (net, server_addr, stop_tx) = scenario::automatic::echo_server();

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
