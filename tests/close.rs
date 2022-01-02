use tcpstate::{options::*, *};

#[macro_use]
mod common;
use common::sim_net::{Incoming, RemoteAddr};
use common::socket_manual::{ListenCtx, ManualHandler, ManualInstant, SocketCtx};
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

    client.consume_event().expect("Error: event");
    server.consume_event().expect("Error: event");

    assert_eq!(ConnectionState::TimeWait, client.state(), "client");
    assert_eq!(ConnectionState::Closed, server.state(), "server");

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

    server.consume_event().expect("Error: event");
    client.consume_event().expect("Error: event");

    assert_eq!(ConnectionState::TimeWait, client.state(), "client");
    assert_eq!(ConnectionState::TimeWait, server.state(), "server");

    let time_after = ManualInstant::now().add(MAX_SEGMENT_LIFETIME * 3);
    server.on_time_tick(time_after);
    client.on_time_tick(time_after);

    assert_eq!(ConnectionState::Closed, client.state(), "client");
    assert_eq!(ConnectionState::Closed, server.state(), "server");
}

/// Call close before connection has been established
#[test]
fn tcp_connect_close() {
    init();

    let client_handler = ManualHandler::new();
    let (client, _) = SocketCtx::new(client_handler);

    {
        let mut s = client.socket.lock().unwrap();
        expect_continue!(s.call_connect(RemoteAddr::new()));
    }

    client.call_close().expect("Error: close");
    assert_eq!(client.consume_event(), Err(Error::ConnectionClosing));
    assert_eq!(ConnectionState::Closed, client.state(), "client");
}

/// Call close before connection has been established
#[test]
fn tcp_syn_received_close() {
    init();

    let server_handler = ManualHandler::new();
    let client_handler = ManualHandler::new();

    let (listen, mut s_on_segment) = ListenCtx::new(5, server_handler.clone());
    let (client, _c_on_segment) = SocketCtx::new(client_handler.clone());

    {
        let mut s = client.socket.lock().unwrap();
        expect_continue!(s.call_connect(RemoteAddr::new()));
    }

    let syn_packet = match client_handler.try_take() {
        Some(Incoming::Packet(pkt)) => pkt,
        other => panic!("Expected a packet, get {:?}", other),
    };
    assert_eq!(syn_packet.seg.flags, SegmentFlags::SYN);
    s_on_segment(syn_packet);

    let (_, server) = listen.accept().expect("Accept");

    expect_continue!(server.call_close());
}

/// Call close before connection has been established, with sends queued
#[test]
fn tcp_syn_received_close_queued_sends() {
    init();

    let server_handler = ManualHandler::new();
    let client_handler = ManualHandler::new();

    let (listen, mut s_on_segment) = ListenCtx::new(5, server_handler.clone());
    let (client, _c_on_segment) = SocketCtx::new(client_handler.clone());

    {
        let mut s = client.socket.lock().unwrap();
        expect_continue!(s.call_connect(RemoteAddr::new()));
    }

    let syn_packet = match client_handler.try_take() {
        Some(Incoming::Packet(pkt)) => pkt,
        other => panic!("Expected a packet, get {:?}", other),
    };
    assert_eq!(syn_packet.seg.flags, SegmentFlags::SYN);
    s_on_segment(syn_packet);

    let (_, server) = listen.accept().expect("Accept");

    expect_continue!(server.call_send(vec![1, 2, 3, 4]));
    expect_retry!(server.call_close());
}
