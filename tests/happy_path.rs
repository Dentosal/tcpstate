use tcpstate::*;

/// Forward packets from one socket to another
/// Returns true if any packets were sent
fn fwd(src: &mut Socket, dst: &mut Socket) -> bool {
    let mut result = false;
    let mut rounds = 0;
    while let Some(seg) = src.take_outbound() {
        dst.on_segment(seg).expect("Error");
        result = true;
        rounds += 1;
        assert!(
            rounds < 10,
            "Communication round limit exceeded, possible loop inner"
        );
    }
    result
}

/// Send packets between two sockets until no more traffic is generated
fn process(a: &mut Socket, b: &mut Socket) {
    for _ in 0..10 {
        println!("SERVER => CLIENT");
        let s0 = fwd(a, b);
        println!("CLIENT => SERVER");
        let s1 = fwd(b, a);
        if !(s0 || s1) {
            return;
        }
    }
    panic!("Communication round limit exceeded, possible loop outer");
}

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

    server.call_listen().expect("Error");

    client.call_connect(RemoteAddr).expect("Error");
    process(&mut server, &mut client);

    // Send and recv exact amount of data

    client.call_send(b"Test!".to_vec()).expect("Error");
    process(&mut server, &mut client);

    let mut buffer = [0u8; 5];
    let n = server.call_recv(&mut buffer).expect("Error");
    assert_eq!(&buffer[..n], b"Test!");
    process(&mut server, &mut client);

    // Send and recv partial

    for _ in 0..4 {
        client.call_send(b"Test!".to_vec()).expect("Error");
    }
    process(&mut server, &mut client);

    let mut buffer = [0u8; 8];
    let n = server.call_recv(&mut buffer).expect("Error");
    assert_eq!(&buffer[..n], b"Test!Tes");

    let n = server.call_recv(&mut buffer).expect("Error");
    assert_eq!(&buffer[..n], b"t!Test!T");

    let avail = server.recv_available();
    let n = server.call_recv(&mut buffer[..avail]).expect("Error");
    assert_eq!(&buffer[..n], b"est!");

    // Close sockets
    client.call_close().expect("Error");
    process(&mut server, &mut client);

    server.call_close().expect("Error");
    process(&mut server, &mut client);

    let time_after = Instant::now().add(MAX_SEGMENT_LIFETIME * 3);
    server.on_time_tick(time_after);
    client.on_time_tick(time_after);

    dbg!(&server);
    dbg!(&client);

    assert_eq!(ConnectionState::Closed, client.state(), "client");
    assert_eq!(ConnectionState::Closed, server.state(), "server");
}
