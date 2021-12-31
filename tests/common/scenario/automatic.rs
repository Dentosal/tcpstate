use crossbeam_channel::{bounded, Sender};
use tcpstate::mock::RemoteAddr;

use crate::sim_net;
use crate::socket::{ListenCtx, SocketCtx};

fn pair<S, C>(server_body: S, client_body: C)
where
    S: FnOnce(&SocketCtx) + Sync + Send + 'static,
    C: FnOnce(&SocketCtx) + Sync + Send + 'static,
{
    let net = sim_net::Network::new();
    let server_addr = net.spawn_host(Box::new(move |host_handler, rx| {
        let listen = ListenCtx::new(5, host_handler, rx);

        let (_, accepted) = listen.accept().expect("Accept");
        accepted
            .call(|socket| {
                socket.options.nagle_delay = core::time::Duration::ZERO;
                Ok(())
            })
            .unwrap();

        server_body(&accepted);

        accepted.call(|socket| socket.call_close()).expect("Send");
    }));
    net.spawn_host(Box::new(move |host_handler, rx| {
        let client = SocketCtx::new(host_handler, rx);
        client
            .call(|socket| {
                socket.options.nagle_delay = core::time::Duration::ZERO;
                socket.call_connect(server_addr)
            })
            .expect("Connect");

        client_body(&client);
        client.call(|socket| socket.call_close()).expect("Error");
    }));
    net.wait()
}

pub fn echo_server() -> (sim_net::Network, RemoteAddr, Sender<()>) {
    use std::thread;

    let net = sim_net::Network::new();

    let (stop_tx, stop_rx) = bounded(0);

    let server_addr = net.spawn_host(Box::new(move |host_handler, rx| {
        let listen = ListenCtx::new(5, host_handler, rx);

        let arc_socket = listen.socket.clone();
        let mut handlers = vec![thread::spawn(move || {
            stop_rx.recv().unwrap();
            arc_socket.lock().unwrap().call_close();
        })];

        loop {
            // TODO: try close handles of all finished threads
            let (addr, accepted) = if let Ok(a) = listen.accept() {
                a
            } else {
                break;
            };
            handlers.push(thread::spawn(move || {
                println!("New connection from {:?}", addr);
                accepted
                    .call(|socket| {
                        socket.options.nagle_delay = core::time::Duration::ZERO;
                        Ok(())
                    })
                    .unwrap();

                let mut buffer = [0; 64];
                loop {
                    let n = accepted
                        .call_ret(|socket| socket.call_recv(&mut buffer))
                        .expect("Recv");

                    accepted
                        .call(move |socket| {
                            let data = buffer[..n].to_vec();
                            socket.call_send(data)
                        })
                        .expect("Send");

                    if n < buffer.len() {
                        break;
                    }
                }

                accepted.call(|socket| socket.call_close()).expect("Send");
                println!("Closed connection from {:?}", addr);
            }));
        }
    }));

    (net, server_addr, stop_tx)
}
