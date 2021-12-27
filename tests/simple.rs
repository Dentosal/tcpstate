use tcpstate::{mock::*, options::*, *};

#[macro_use]
mod common;
use common::*;

#[test]
fn tcp_simple_happy_path() {
    init();

    let (mut server, mut client) = scenario::open_pair();

    // Send and recv exact amount of data

    client.call_send(b"C=>S!".to_vec()).expect("Error");
    process(&mut server, &mut client);

    let mut buffer = [0u8; 5];
    let n = server.call_recv(&mut buffer).expect("Error");
    assert_eq!(&buffer[..n], b"C=>S!");
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

    // Close client socket
    client.call_close().expect("Error");
    process(&mut server, &mut client);
    assert_eq!(server.call_recv(&mut [0u8; 1]), Ok(0)); // Read EOF
    process(&mut server, &mut client);

    // Close server socket
    server.call_close().expect("Error");
    process(&mut server, &mut client);
    assert_eq!(client.call_recv(&mut [0u8; 1]), Ok(0)); // Read EOF
    process(&mut server, &mut client);

    // Wait until sockets are closed

    let time_after = Instant::now().add(MAX_SEGMENT_LIFETIME * 3);
    server.on_time_tick(time_after);
    client.on_time_tick(time_after);

    assert_eq!(ConnectionState::Closed, client.state(), "client");
    assert_eq!(ConnectionState::Closed, server.state(), "server");
}
