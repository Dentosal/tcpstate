use tcpstate::*;

#[test]
fn tcp_happy_path() {
    stderrlog::new()
        .module(module_path!())
        .verbosity(4)
        .init()
        .unwrap();

    let mut server = Socket::new();
    let mut client = Socket::new();

    client.options.nagle_delay = core::time::Duration::ZERO;

    // Establish connection

    assert_eq!(server.call_listen(), Res::empty());

    let syn = unwrap_send(client.call_connect(RemoteAddr));
    assert_eq!(syn.flags, SegmentFlags::SYN);

    let syn_ack = unwrap_send(server.on_segment(syn));
    assert_eq!(syn_ack.flags, SegmentFlags::SYN | SegmentFlags::ACK);

    let ack = unwrap_send(client.on_segment(syn_ack));
    assert_eq!(ack.flags, SegmentFlags::ACK);
    assert_eq!(ack.data.len(), 0);

    // Recv exact amount of data

    let s = client.call_send(b"Hello!".to_vec());
    let data = unwrap_send(s);
    assert_eq!(data.data.len(), 6);

    let data_ack = unwrap_send(server.on_segment(data));
    assert_eq!(data_ack.data.len(), 0);

    client.on_segment(data_ack);

    let mut buffer = [0u8; 6];
    let r = server.call_recv(&mut buffer);
    let n = r.value.unwrap();
    assert_eq!(&buffer[..n], b"Hello!");

    // Recv: concat two packets

    for i in 0..2 {
        dbg!(i);
        let s = client.call_send(b"Hello!".to_vec());
        let data = unwrap_send(s);
        assert_eq!(data.data.len(), 6);

        dbg!(&data);
        let data_ack = unwrap_send(server.on_segment(data));
        dbg!(&data_ack);
        assert_eq!(data_ack.data.len(), 0);

        client.on_segment(data_ack);
    }

    let mut buffer = [0u8; 12];
    let r = server.call_recv(&mut buffer);
    let n = r.value.unwrap();
    assert_eq!(&buffer[..n], b"Hello!Hello!");
}
