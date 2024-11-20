use std::{env, net::SocketAddr};

use anyhow::{Context, Result};
use log::{debug, error, info};
use mio::{
    net::{TcpListener, TcpStream},
    Events, Poll, Token,
};
use slab::Slab;

mod connection;
use connection::Connection;

const MAX_CLIENTS: usize = 1024;
const LISTENER: Token = Token(MAX_CLIENTS + 1);

#[derive(Debug)]
struct Server {
    listener: TcpListener,
    poll: Poll,
    events: Events,
    connections: Slab<Connection<TcpStream>>,
}

impl Server {
    fn bind(
        addr: SocketAddr,
        poll: Poll,
        events: Events,
        connections: Slab<Connection<TcpStream>>,
    ) -> Result<Self> {
        let mut listener = TcpListener::bind(addr)?;

        poll.registry()
            .register(&mut listener, LISTENER, mio::Interest::READABLE)
            .with_context(|| "Failed to register listener")?;

        info!("server listening on {:?}", listener.local_addr()?);
        Ok(Self {
            listener,
            poll,
            events,
            connections,
        })
    }

    fn run(&mut self) -> Result<()> {
        loop {
            self.poll
                .poll(&mut self.events, None)
                .with_context(|| "poll failed")?;

            for event in self.events.iter() {
                match event.token() {
                    LISTENER => match self.listener.accept() {
                        Ok((socket, addr)) => {
                            debug!("server: Connection from {:?}", addr);

                            let connection = Connection::with_capacity(socket, 1024);
                            let key = self.connections.insert(connection);
                            if let Err(e) = self.poll.registry().register(
                                &mut self.connections[key].socket,
                                Token(key),
                                mio::Interest::READABLE,
                            ) {
                                error!("server: Failed to register socket: {:?}", e);
                                self.connections.try_remove(key);
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
                        if let Some(connection) = self.connections.get_mut(token.0) {
                            if event.is_readable() {
                                match connection.read() {
                                    Ok(0) => {
                                        debug!("Client disconnected");
                                        while let Err(e) =
                                            self.poll.registry().deregister(&mut connection.socket)
                                        {
                                            error!("server: Failed to deregister socket: {:?}", e);
                                        }
                                        self.connections.remove(token.0);
                                        continue;
                                    }
                                    Ok(_) => {
                                        self.poll
                                            .registry()
                                            .reregister(
                                                &mut connection.socket,
                                                token,
                                                mio::Interest::READABLE
                                                    .add(mio::Interest::WRITABLE),
                                            )
                                            .with_context(|| "Failed to reregister socket")?;
                                        continue;
                                    }
                                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                        continue;
                                    }
                                    Err(e) => {
                                        error!("server: Failed to read: {:?}", e);
                                        let _ =
                                            self.poll.registry().deregister(&mut connection.socket);
                                        self.connections.remove(token.0);
                                        continue;
                                    }
                                }
                            } else if event.is_writable() {
                                match connection.write() {
                                    Ok(_) => {
                                        self.poll
                                            .registry()
                                            .reregister(
                                                &mut connection.socket,
                                                token,
                                                mio::Interest::READABLE,
                                            )
                                            .with_context(|| {
                                                "Failed to reregister socket as READABLE"
                                            })?;
                                    }
                                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                        continue;
                                    }
                                    Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {
                                        debug!("Client disconnected (broken pipe)");
                                        let _ =
                                            self.poll.registry().deregister(&mut connection.socket);
                                        self.connections.remove(token.0);
                                        continue;
                                    }
                                    Err(e) => {
                                        error!("writeable branch: Failed to write: {:?}", e);
                                        let _ =
                                            self.poll.registry().deregister(&mut connection.socket);
                                        self.connections.remove(token.0);
                                        continue;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn main() -> Result<()> {
    if env::var("RUST_LOG").is_err() {
        env::set_var("RUST_LOG", "info");
    }
    env_logger::init();

    let addr = "127.0.0.1:8080".parse()?;
    let poll = Poll::new()?;
    let events = Events::with_capacity(MAX_CLIENTS);
    let connections = Slab::with_capacity(MAX_CLIENTS);

    let mut server = Server::bind(addr, poll, events, connections)?;
    server.run()?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use mio::net::TcpStream;

    use super::*;
    use std::{
        io::{Read, Write},
        thread::{self},
    };

    fn setup_server() -> Result<(Server, SocketAddr)> {
        let server_addr = "127.0.0.1:0".parse::<SocketAddr>()?;
        let poll = Poll::new()?;
        let events = Events::with_capacity(MAX_CLIENTS);
        let connections = Slab::with_capacity(MAX_CLIENTS);

        let server = Server::bind(server_addr, poll, events, connections)?;
        let local_addr = server.listener.local_addr()?;
        Ok((server, local_addr))
    }

    #[test]
    fn test_single_message() -> Result<()> {
        let (mut server, local_addr) = setup_server()?;
        thread::spawn(move || server.run().unwrap());

        let mut stream = TcpStream::connect(local_addr)
            .with_context(|| format!("Failed to connect to {:?}", local_addr))?;
        stream.set_nodelay(true)?;

        let test_msg = b"Hello, Server!";
        stream
            .write_all(test_msg)
            .with_context(|| format!("Failed to write to {:?}", local_addr))?;
        stream.flush()?;

        let mut buf = vec![0; test_msg.len()];
        stream
            .read_exact(&mut buf)
            .with_context(|| format!("Failed to read from {:?}", local_addr))?;
        assert_eq!(&buf, test_msg);

        Ok(())
    }

    #[test]
    fn test_multiple_writes() -> Result<()> {
        let (mut server, local_addr) = setup_server()?;
        thread::spawn(move || server.run().unwrap());

        let mut stream = TcpStream::connect(local_addr)?;
        stream.set_nodelay(true)?;

        let test_msg1 = b"First";
        let test_msg2 = b"Second";
        stream.write_all(test_msg1)?;
        stream.write_all(test_msg2)?;
        stream.flush()?;

        let expected = [&test_msg1[..], &test_msg2[..]].concat();
        let mut buf = vec![0; expected.len()];
        stream.read_exact(&mut buf)?;
        assert_eq!(buf, expected);

        Ok(())
    }

    #[test]
    fn test_large_message() -> Result<()> {
        let (mut server, local_addr) = setup_server()?;
        thread::spawn(move || server.run().unwrap());

        let mut stream = TcpStream::connect(local_addr)?;
        stream.set_nodelay(true)?;

        let test_msg = vec![b'X'; 1024];
        stream.write_all(&test_msg)?;
        stream.flush()?;

        let mut buf = vec![0; test_msg.len()];
        stream.read_exact(&mut buf)?;
        assert_eq!(buf, test_msg);

        Ok(())
    }

    #[test]
    fn test_message_larger_than_buffer() -> Result<()> {
        let (mut server, local_addr) = setup_server()?;
        thread::spawn(move || server.run().unwrap());

        let mut stream = TcpStream::connect(local_addr)?;
        stream.set_nodelay(true)?;

        let buffer_size = 1024;
        let test_msg = vec![b'X'; buffer_size + 500];
        stream.write_all(&test_msg)?;
        stream.flush()?;

        let mut buf = vec![0; buffer_size];
        stream.read_exact(&mut buf)?;
        assert_eq!(&buf[..], &test_msg[..buffer_size]);

        Ok(())
    }
}
