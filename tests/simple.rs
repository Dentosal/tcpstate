use tcpstate::*;

mod common;
use common::*;

#[test]
fn tcp_simple_happy_path() {
    init();

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

    assert_eq!(ConnectionState::Closed, client.state(), "client");
    assert_eq!(ConnectionState::Closed, server.state(), "server");
}
