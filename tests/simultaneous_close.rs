use tcpstate::{mock::*, options::*, *};

#[macro_use]
mod common;
use common::*;

#[test]
fn tcp_simulteneous_close() {
    init();

    let (mut server, mut client) = scenario::open_pair();

    // Close sockets simulteneously
    client.call_close().expect("Error");
    server.call_close().expect("Error");
    process(&mut server, &mut client);

    assert_eq!(client.call_recv(&mut [0u8; 1]), Ok(0), "client recv");
    assert_eq!(server.call_recv(&mut [0u8; 1]), Ok(0), "server recv");
    process(&mut server, &mut client);

    let time_after = Instant::now().add(MAX_SEGMENT_LIFETIME * 3);
    server.on_time_tick(time_after);
    client.on_time_tick(time_after);

    assert_eq!(ConnectionState::Closed, client.state(), "client");
    assert_eq!(ConnectionState::Closed, server.state(), "server");
}