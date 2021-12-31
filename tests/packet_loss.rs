#![deny(unused_must_use)]
#![feature(let_else)]

#[macro_use]
mod common;

use common::{socket::*, *};

#[test]
fn tcp_echo_server() {
    init();

    let (net, server_addr, stop_tx) = scenario::automatic::echo_server();

    net.spawn_host(Box::new(move |host_handler, rx| {
        let client = SocketCtx::new(host_handler, rx);
        client
            .call(|socket| {
                socket.options.nagle_delay = core::time::Duration::ZERO;
                socket.call_connect(server_addr)
            })
            .expect("Connect");

        for r in 0..100 {
            client
                .call(|socket| socket.call_send(format!("<{}>", r).as_bytes().to_vec()))
                .expect("Error");
        }

        client.call(|socket| socket.call_shutdown()).expect("Error");

        let mut buffer = [0; 1024];
        let n = client
            .call_ret(|socket| socket.call_recv(&mut buffer))
            .expect("Recv");

        let mut i = 0;
        for r in 0..100 {
            let b = format!("<{}>", r);
            assert!(i + b.len() <= n);
            assert_eq!(b.as_bytes(), &buffer[i..i + b.len()]);
            i += b.len();
        }

        client.call(|socket| socket.call_close()).expect("Error");

        stop_tx.send(()).unwrap();
    }));

    net.wait();
}
