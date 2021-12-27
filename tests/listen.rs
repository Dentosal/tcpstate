#![deny(unused_must_use)]

use tcpstate::{mock::*, *};

#[macro_use]
mod common;
use common::*;

#[test]
fn tcp_listen() {
    init();

    let mut server = ListenSocket::call_listen(5);

    // Process sequentially
    for i in 0..4 {
        let mut client = Socket::new();
        client.options.nagle_delay = core::time::Duration::ZERO;

        let cc = expect_continue!(client.call_connect(RemoteAddr));
        let seg = client.take_outbound().unwrap();

        server.on_segment(RemoteAddr, seg);

        let (_addr, mut socket) = server.call_accept().expect("Listen");

        process(&mut socket, &mut client);

        client.try_wait_event(cc).expect("Connect");

        let data = format!("Message {}!", i).as_bytes().to_vec();
        client.call_send(data.clone()).expect("Error");

        client.call_shutdown().expect("Error");
        process(&mut socket, &mut client);

        let mut buffer = [0; 1024];
        let n = socket.call_recv(&mut buffer).expect("Error");
        assert_eq!(&buffer[..n], data);
        process(&mut socket, &mut client);

        client.call_close().expect("Error");
        socket.call_close().expect("Error");
        process(&mut socket, &mut client);
    }

    // Queued clients

    let clients: Vec<_> = (0..4)
        .map(|_| {
            let mut client = Socket::new();
            client.options.nagle_delay = core::time::Duration::ZERO;

            let cc = expect_continue!(client.call_connect(RemoteAddr));
            let seg = client.take_outbound().unwrap();

            server.on_segment(RemoteAddr, seg);
            (client, cc)
        })
        .collect();

    for (i, (mut client, cc)) in clients.into_iter().enumerate() {
        let (_addr, mut socket) = server.call_accept().expect("Listen");

        process(&mut socket, &mut client);
        client.try_wait_event(cc).expect("Connect");

        let data = format!("Message {}!", i).as_bytes().to_vec();
        client.call_send(data.clone()).expect("Error");

        client.call_shutdown().expect("Error");
        process(&mut socket, &mut client);

        let mut buffer = [0; 1024];
        let n = socket.call_recv(&mut buffer).expect("Error");
        assert_eq!(&buffer[..n], data);
        process(&mut socket, &mut client);

        client.call_close().expect("Error");
        socket.call_close().expect("Error");
        process(&mut socket, &mut client);
    }
}

#[test]
fn tcp_listen_drops_over_limit() {
    init();

    let mut server = ListenSocket::call_listen(5);

    for _ in 0..5 {
        let mut client = Socket::new();
        let _ = expect_continue!(client.call_connect(RemoteAddr));
        let seg = client.take_outbound().unwrap();
        server.on_segment(RemoteAddr, seg);
    }

    let mut client = Socket::new();
    let _ = expect_continue!(client.call_connect(RemoteAddr));
    let seg = client.take_outbound().unwrap();
    server.on_segment(RemoteAddr, seg);

    let (_addr, seg) = server.take_outbound().unwrap();
    assert!(seg.flags.contains(SegmentFlags::RST));
    client.on_segment(seg);
    assert_eq!(client.state(), ConnectionState::Reset);
}
