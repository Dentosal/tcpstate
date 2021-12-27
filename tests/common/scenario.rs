use tcpstate::{mock::*, *};

use super::*;

pub fn open_pair() -> (Socket, Socket) {
    let mut listen = ListenSocket::call_listen(1);
    let mut client = Socket::new();

    // Establish connection

    let cc = expect_continue!(client.call_connect(RemoteAddr));
    let seg = client.take_outbound().expect("No packet");
    listen.on_segment(RemoteAddr, seg);
    let (_addr, mut server) = listen.call_accept().expect("Error");
    process(&mut server, &mut client);
    client.try_wait_event(cc).expect("Connect");

    server.options.nagle_delay = core::time::Duration::ZERO;
    client.options.nagle_delay = core::time::Duration::ZERO;

    assert_eq!(ConnectionState::FULLY_OPEN, client.state(), "client");
    assert_eq!(ConnectionState::FULLY_OPEN, server.state(), "server");

    (server, client)
}
