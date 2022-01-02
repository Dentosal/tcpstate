use tcpstate::*;

#[macro_use]
mod common;
use common::sim_net::RemoteAddr;
use common::socket_manual::{ManualHandler, SocketCtx};
use common::*;

#[test]
fn tcp_abort_connection() {
    init();

    let (server, client, mut communicate) = scenario::manual::open_pair();

    // Setup pending read
    expect_retry!(client.call_recv(&mut [0u8; 16]));

    // Abort client connection
    client.call_abort().expect("Error");
    assert_eq!(client.consume_event(), Err(Error::ConnectionReset));
    communicate();

    assert_eq!(
        server.call_recv(&mut [0u8; 16]),
        Err(Error::ConnectionReset)
    );

    assert_eq!(ConnectionState::Closed, client.state(), "client");
    assert_eq!(ConnectionState::Reset, server.state(), "server");
}

#[test]
fn tcp_simulteneous_abort() {
    init();

    let (server, client, mut communicate) = scenario::manual::open_pair();

    // Abort sockets simulteneously
    server.call_abort().expect("Error");
    client.call_abort().expect("Error");
    communicate();

    assert_eq!(ConnectionState::Closed, client.state(), "client");
    assert_eq!(ConnectionState::Closed, server.state(), "server");
}

/// Call abort before connection has been established
#[test]
fn tcp_connect_abort() {
    init();

    let client_handler = ManualHandler::new();
    let (client, _) = SocketCtx::new(client_handler);

    {
        let mut s = client.socket.lock().unwrap();
        expect_continue!(s.call_connect(RemoteAddr::new()));
    }

    client.call_abort().expect("Error: close");
    assert_eq!(client.consume_event(), Err(Error::ConnectionReset));
    assert_eq!(ConnectionState::Closed, client.state(), "client");
}
