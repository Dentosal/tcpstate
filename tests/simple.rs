use tcpstate::*;

#[macro_use]
mod common;
use common::*;
#[test]
fn tcp_simple_happy_path() {
    init();

    let (server, client, mut communicate) = scenario::manual::open_pair();

    // Send and recv exact amount of data

    client.call_send(b"C=>S!".to_vec()).expect("Error");
    communicate();

    let mut buffer = [0u8; 5];
    let n = server.call_recv(&mut buffer).expect("Error");
    assert_eq!(&buffer[..n], b"C=>S!");
    communicate();

    // Send and recv partial

    for _ in 0..4 {
        client.call_send(b"Test!".to_vec()).expect("Error");
    }
    communicate();

    let mut buffer = [0u8; 8];
    let n = server.call_recv(&mut buffer).expect("Error");
    assert_eq!(&buffer[..n], b"Test!Tes");

    let n = server.call_recv(&mut buffer).expect("Error");
    assert_eq!(&buffer[..n], b"t!Test!T");

    let avail = server.recv_available();
    let n = server.call_recv(&mut buffer[..avail]).expect("Error");
    assert_eq!(&buffer[..n], b"est!");

    // Close sockets
    expect_continue!(client.call_close());
    communicate();
    assert_eq!(server.call_recv(&mut [0u8; 1]), Ok(0)); // Read EOF
    expect_continue!(server.call_close());
    communicate();
    assert_eq!(client.call_recv(&mut [0u8; 1]), Ok(0)); // Read EOF
    communicate();

    // Wait until sockets are closed
    server.consume_event().expect("Error: event");
    client.consume_event().expect("Error: event");

    assert_eq!(ConnectionState::TimeWait, client.state(), "client");
    assert_eq!(ConnectionState::Closed, server.state(), "server");
}
