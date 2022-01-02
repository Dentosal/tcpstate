#![deny(unused_must_use)]
#![feature(let_else)]

#[macro_use]
mod common;

use common::{sim_net::ErrorProfile, socket::*, *};

#[test]
fn tcp_echo_server() {
    init();

    let (net, server_addr, stop_tx) = scenario::automatic::echo_server();

    net.set_error_profile(ErrorProfile::HighPacketLoss);

    net.spawn_host(Box::new(move |host_handler, rx| {
        let client = SocketCtx::new(host_handler, rx);
        client
            .call(|socket| {
                socket.options_mut().nagle_delay = core::time::Duration::ZERO;
                socket.call_connect(server_addr)
            })
            .expect("Connect");

        let cc = client.clone();
        let collector = std::thread::spawn(move || {
            let mut buffer = [0; 1024];
            let n = cc
                .call_ret(|socket| socket.call_recv(&mut buffer))
                .expect("Recv");

            let mut i = 0;
            for r in 0..20 {
                let b = format!("<{}>", r);
                assert!(i + b.len() <= n);
                assert_eq!(b.as_bytes(), &buffer[i..i + b.len()]);
                i += b.len();
            }
        });

        for r in 0..20 {
            client
                .call(|socket| socket.call_send(format!("<{}>", r).as_bytes().to_vec()))
                .expect("Error");
        }

        client.call(|socket| socket.call_shutdown()).expect("Error");

        collector.join().unwrap();

        client.call(|socket| socket.call_close()).expect("Error");

        stop_tx.send(()).unwrap();
    }));

    net.wait();
}
