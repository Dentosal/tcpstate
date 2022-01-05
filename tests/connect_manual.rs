use tcpstate::*;

#[macro_use]
mod common;
use common::sim_net::{Incoming, Packet, RemoteAddr};
use common::socket_manual::{ManualHandler, SocketCtx};
use common::*;

/// Connect to a server, test with manually crafted packets
#[test]
fn tcp_connect_manual() {
    init();

    let server_addr = RemoteAddr::new();
    let client_addr = RemoteAddr::new();

    let client_handler = ManualHandler::new();

    let (client, mut c_on_segment) = SocketCtx::new(client_handler.clone());

    {
        let mut s = client.socket.lock().unwrap();
        expect_continue!(s.call_connect(server_addr));
    }

    let syn_packet = match client_handler.try_take() {
        Some(Incoming::Packet(pkt)) => pkt,
        other => panic!("Expected a packet, get {:?}", other),
    };
    assert_eq!(syn_packet.seg.flags, SegmentFlags::SYN);

    c_on_segment(Packet {
        src: server_addr,
        dst: client_addr,
        seg: SegmentMeta {
            seqn: SeqN::new(1000),
            ackn: syn_packet.seg.seqn.wrapping_add(1),
            window: 1024,
            flags: SegmentFlags::SYN | SegmentFlags::ACK,
            data: Vec::new(),
        },
    });

    {
        let mut s = client.socket.lock().unwrap();
        s.user_data_mut()
            .try_take_event()
            .expect("No Event")
            .expect("Event error");
    }

    let first_ack_packet = match client_handler.try_take() {
        Some(Incoming::Packet(pkt)) => pkt,
        other => panic!("Expected a packet, get {:?}", other),
    };
    assert_eq!(first_ack_packet.seg.flags, SegmentFlags::ACK);
    assert_eq!(first_ack_packet.seg.ackn, SeqN::new(1001));
}
