use tcpstate::{mock::*, options::*, *};

#[macro_use]
mod common;
use common::*;

/// One sided FIN, like with HTTP/1.0 requests
#[test]
fn tcp_one_sided() {
    init();

    let (server, client, mut communicate) = scenario::manual::open_pair();

    // Send request and send FIN

    const REQUEST: &[u8] = b"GET / HTTP/1.0\r\n\r\n";
    const REPLY: &[u8] = b"HTTP/1.0 200 OK\r\n\r\n";

    client.call_send(REQUEST.to_vec()).expect("Error");
    client.call_shutdown().expect("Error");

    communicate();

    // Read all data in small segments

    let mut input: Vec<u8> = Vec::new();
    let mut buffer = [0u8; 4];

    loop {
        let n = server.call_recv(&mut buffer).expect("Error");
        input.extend(&buffer[..n]);
        if n < buffer.len() {
            break;
        }
    }

    assert_eq!(&input, REQUEST);

    communicate();

    // Send response back in multiple packets and close socket

    server.call_send(REPLY.to_vec()).expect("Error");

    for _ in 0..6 {
        server
            .call_send(format!("Test!").as_bytes().to_vec())
            .expect("Error");
    }

    server.call_shutdown().expect("close failed");

    communicate();

    // Read response to a big buffer

    let mut buffer = [0u8; 1024];
    let n = client.call_recv(&mut buffer).expect("Error");
    assert!(n > REPLY.len());
    assert_eq!(&buffer[..REPLY.len()], REPLY);
    assert_eq!(&buffer[REPLY.len()..n], "Test!".repeat(6).as_bytes());

    communicate();

    // Read ack of closing the socket
    assert_eq!(server.call_recv(&mut buffer), Ok(0));

    communicate();

    // Close sockets
    server.call_close().expect("close failed");
    client.call_close().expect("close failed");
    communicate();

    let time_after = Instant::now().add(MAX_SEGMENT_LIFETIME * 3);
    server.on_time_tick(time_after);
    client.on_time_tick(time_after);

    assert_eq!(ConnectionState::Closed, client.state(), "client");
    assert_eq!(ConnectionState::Closed, server.state(), "server");
}
