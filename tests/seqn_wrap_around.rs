#![deny(unused_must_use)]
#![feature(let_else)]

use std::sync::atomic::Ordering;

#[macro_use]
mod common;

use common::{socket::*, *};

/// Verify that sequence number wraparound works correctly
#[test]
fn tcp_seqn_wrap_around() {
    init();

    let (net, server_addr, stop_tx) = scenario::automatic::echo_server();

    sim_net::INIT_SEQN.store(u32::MAX - 5, Ordering::SeqCst);

    net.spawn_host(Box::new(move |host_handler, rx| {
        let client = SocketCtx::new(host_handler, rx);
        client
            .call(|socket| {
                socket.options_mut().nagle_delay = core::time::Duration::ZERO;
                socket.call_connect(server_addr)
            })
            .expect("Connect");

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

        client.call(|socket| socket.call_close()).expect("Error");

        stop_tx.send(()).unwrap();
    }));

    net.wait();
}
