use tcpstate::{options::*, *};

#[macro_use]
mod common;
use common::socket_manual::ManualInstant;
use common::*;

#[test]
fn tcp_open_close() {
    init();

    let (server, client, mut communicate) = scenario::manual::open_pair();

    // Close sockets
    expect_continue!(client.call_close());
    communicate();
    assert_eq!(server.call_recv(&mut [0u8; 1]), Ok(0)); // Read EOF
    communicate();

    expect_continue!(server.call_close());
    communicate();
    assert_eq!(client.call_recv(&mut [0u8; 1]), Ok(0)); // Read EOF
    communicate();

    client.consume_event();
    server.consume_event();

    // assert_eq!(ConnectionState::TimeWait, client.state(), "client");
    // assert_eq!(ConnectionState::Closed, server.state(), "server");

    let time_after = ManualInstant::now().add(MAX_SEGMENT_LIFETIME * 3);
    server.on_time_tick(time_after);
    client.on_time_tick(time_after);

    assert_eq!(ConnectionState::Closed, client.state(), "client");
    assert_eq!(ConnectionState::Closed, server.state(), "server");
}

#[test]
fn tcp_simulteneous_close() {
    init();

    let (server, client, mut communicate) = scenario::manual::open_pair();

    // Close sockets simulteneously
    expect_continue!(server.call_close());
    expect_continue!(client.call_close());
    communicate();

    assert_eq!(client.call_recv(&mut [0u8; 1]), Ok(0), "client recv");
    assert_eq!(server.call_recv(&mut [0u8; 1]), Ok(0), "server recv");
    communicate();

    server.consume_event();
    client.consume_event();

    assert_eq!(ConnectionState::TimeWait, client.state(), "client");
    assert_eq!(ConnectionState::TimeWait, server.state(), "server");

    let time_after = ManualInstant::now().add(MAX_SEGMENT_LIFETIME * 3);
    server.on_time_tick(time_after);
    client.on_time_tick(time_after);

    assert_eq!(ConnectionState::Closed, client.state(), "client");
    assert_eq!(ConnectionState::Closed, server.state(), "server");
}
