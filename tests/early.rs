use tcpstate::mock::RemoteAddr;
use tcpstate::{options::*, *};

#[macro_use]
mod common;
use common::sim_net::Incoming;
use common::socket_manual::{ListenCtx, ManualHandler, SocketCtx};
use common::*;

/// Send some data with the SYN-ACK packet
#[test]
fn tcp_early_send() {
    init();

    let server_handler = ManualHandler::new();
    let client_handler = ManualHandler::new();

    let (listen, mut s_on_segment) = ListenCtx::new(5, server_handler.clone());
    let (client, mut c_on_segment) = SocketCtx::new(client_handler.clone());

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

    let synack_packet = match server_handler.try_take() {
        Some(Incoming::Packet(pkt)) => pkt,
        other => panic!("Expected a packet, get {:?}", other),
    };
    assert_eq!(
        synack_packet.seg.flags,
        SegmentFlags::SYN | SegmentFlags::ACK
    );
    c_on_segment(synack_packet);
}
