use std::io::{Read, Write};

use anyhow::Result;
use mio::{net::TcpListener, Events, Poll, Token};
use slab::Slab;

const MAX_CLIENTS: usize = 1024;
const LISTENER: Token = Token(MAX_CLIENTS + 1);

fn main() -> Result<()> {
    let addr = "127.0.0.1:8080".parse()?;
    let mut listener = TcpListener::bind(addr)?;

    let mut poll = Poll::new()?;
    let mut events = Events::with_capacity(MAX_CLIENTS);
    // TODO instead of a Slab<TcpStream> probably need a Slab<Connection> with the buf to handle partial scenarios
    let mut connections = Slab::with_capacity(MAX_CLIENTS);

    poll.registry()
        .register(&mut listener, LISTENER, mio::Interest::READABLE)?;

    loop {
        poll.poll(&mut events, None)?;

        for event in events.iter() {
            match event.token() {
                LISTENER => match listener.accept() {
                    Ok((socket, addr)) => {
                        println!("server: Connection from {:?}", addr);

                        // TODO consider MAX_CLIENTS size
                        let key = connections.insert(socket);
                        if let Err(e) = poll.registry().register(
                            &mut connections[key],
                            Token(key),
                            mio::Interest::READABLE,
                        ) {
                            println!("server: Failed to register socket: {:?}", e);
                            connections.try_remove(key);
                            return Err(e.into());
                        }
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        continue;
                    }
                    Err(e) => {
                        return Err(e.into());
                    }
                },
                token => {
                    if let Some(socket) = connections.get_mut(token.0) {
                        // TODO buf pool and store with connection
                        let mut buf = vec![0; 1024];
                        match socket.read(&mut buf) {
                            Ok(0) => {
                                connections.try_remove(token.0);
                            }
                            Ok(n) => {
                                if let Err(e) = socket.write_all(&buf[..n]) {
                                    if e.kind() == std::io::ErrorKind::WouldBlock {
                                        // TODO handle writable, need to store buf
                                        poll.registry().reregister(
                                            socket,
                                            token,
                                            mio::Interest::READABLE.add(mio::Interest::WRITABLE),
                                        )?;
                                        continue;
                                    }
                                    return Err(e.into());
                                }
                            }
                            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                continue;
                            }
                            Err(e) => return Err(e.into()),
                        }
                    }
                }
            }
        }
    }
}
