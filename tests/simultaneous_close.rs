use tcpstate::*;

mod common;
use common::*;

#[test]
fn tcp_simulteneous_close() {
    init();

    let mut server = Socket::new();
    let mut client = Socket::new();

    client.options.nagle_delay = core::time::Duration::ZERO;

    // Establish connection

    server.call_listen().expect("Error");
    client.call_connect(RemoteAddr).expect("Error");
    process(&mut server, &mut client);

    assert_eq!(ConnectionState::Established, client.state(), "client");
    assert_eq!(ConnectionState::Established, server.state(), "server");

    // Close sockets simulteneously
    client.call_close().expect("Error");
    server.call_close().expect("Error");
    process(&mut server, &mut client);

    let time_after = Instant::now().add(MAX_SEGMENT_LIFETIME * 3);
    server.on_time_tick(time_after);
    client.on_time_tick(time_after);

    assert_eq!(ConnectionState::Closed, client.state(), "client");
    assert_eq!(ConnectionState::Closed, server.state(), "server");
}
