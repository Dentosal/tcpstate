use tcpstate::{mock::*, options::*, *};

#[macro_use]
mod common;
use common::*;

#[test]
fn tcp_early_send_recv() {
    init();

    let mut server = Socket::new();
    let mut client = Socket::new();

    client.options.nagle_delay = core::time::Duration::ZERO;

    // Open connection and perform requests

    server.call_listen().expect("Error");
    client.call_connect(RemoteAddr).expect("Error");

    let send_cookie = match client.call_send(b"Testing!".to_vec()) {
        Err(Error::ContinueAfter(cookie)) => cookie,
        other => panic!(
            "Early send should return a continue cookie, got {:?}",
            other
        ),
    };

    let mut buffer = [0u8; 8];
    let recv_cookie = match server.call_recv(&mut buffer) {
        Err(Error::RetryAfter(cookie)) => cookie,
        other => panic!("Early send should return a retry cookie, got {:?}", other),
    };

    // Establish connection
    process(&mut server, &mut client);

    assert_eq!(ConnectionState::FULLY_OPEN, client.state(), "client");
    assert_eq!(ConnectionState::FULLY_OPEN, server.state(), "server");

    // Check the the send call is not done yet
    client
        .try_wait_event(send_cookie)
        .expect_err("send event not active");

    // Retry recv call
    server
        .try_wait_event(recv_cookie)
        .expect("send event not active");

    let n = server
        .call_recv(&mut buffer)
        .expect("recv failed even after the event");
    assert_eq!(&buffer[..n], b"Testing!");

    // Send the ACK from the retry call
    process(&mut server, &mut client);

    assert_eq!(ConnectionState::FULLY_OPEN, client.state(), "client");
    assert_eq!(ConnectionState::FULLY_OPEN, server.state(), "server");

    // Check the the send call is done
    client
        .try_wait_event(send_cookie)
        .expect("send event not active");

    // Close sockets
    client.call_close().expect("Error");
    process(&mut server, &mut client);
    assert_eq!(server.call_recv(&mut [0u8; 1]), Ok(0)); // Read EOF
    process(&mut server, &mut client);

    server.call_close().expect("Error");
    process(&mut server, &mut client);
    assert_eq!(client.call_recv(&mut [0u8; 1]), Ok(0)); // Read EOF
    process(&mut server, &mut client);

    let time_after = Instant::now().add(MAX_SEGMENT_LIFETIME * 3);
    server.on_time_tick(time_after);
    client.on_time_tick(time_after);

    assert_eq!(ConnectionState::Closed, client.state(), "client");
    assert_eq!(ConnectionState::Closed, server.state(), "server");
}
