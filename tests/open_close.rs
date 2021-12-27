use tcpstate::{mock::*, options::*, *};

#[macro_use]
mod common;
use common::*;

#[test]
fn tcp_open_close() {
    init();

    let mut server = Socket::new();
    let mut client = Socket::new();

    server.options.nagle_delay = core::time::Duration::ZERO;
    client.options.nagle_delay = core::time::Duration::ZERO;

    // Establish connection

    server.call_listen().expect("Error");

    client.call_connect(RemoteAddr).expect("Error");
    process(&mut server, &mut client);

    assert_eq!(ConnectionState::FULLY_OPEN, client.state(), "client");
    assert_eq!(ConnectionState::FULLY_OPEN, server.state(), "server");

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
