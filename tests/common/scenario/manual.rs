use tcpstate::*;

use crate::socket_manual::{ListenCtx, ManualHandler, SocketCtx};

pub fn open_pair() -> (SocketCtx, SocketCtx, impl FnMut()) {
    let server_handler = ManualHandler::new();
    let server_addr = server_handler.local;

    let client_handler = ManualHandler::new();

    let server_h = server_handler.clone();
    let client_h = client_handler.clone();

    let (listen, mut s_on_segment) = ListenCtx::new(5, server_handler);
    let (client, mut c_on_segment) = SocketCtx::new(client_handler);

    let mut communicate = move || {
        const LIMIT: usize = 20;
        let mut count = 0;
        let mut any_events = true;
        while any_events {
            any_events = false;
            while let Some(p) = server_h.try_take() {
                c_on_segment(p);
                any_events = true;
                count += 1;
                if count > LIMIT {
                    panic!("Limit reached");
                }
            }
            while let Some(p) = client_h.try_take() {
                s_on_segment(p);
                any_events = true;
                count += 1;
                if count > LIMIT {
                    panic!("Limit reached");
                }
            }
        }
    };

    {
        let mut s = client.socket.lock().unwrap();
        s.options.nagle_delay = core::time::Duration::ZERO;
        expect_continue!(s.call_connect(server_addr));
    }

    expect_retry!(listen.accept());

    communicate();

    listen.consume_event();

    let (_, server) = listen.accept().expect("Accept");
    {
        let mut s = server.socket.lock().unwrap();
        s.options.nagle_delay = core::time::Duration::ZERO;
    }

    communicate();

    client.consume_event();

    assert_eq!(ConnectionState::FULLY_OPEN, client.state(), "client");
    assert_eq!(ConnectionState::FULLY_OPEN, server.state(), "server");

    (server, client, communicate)
}
