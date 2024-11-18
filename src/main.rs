use std::{
    env,
    io::{self, Read, Write},
    net::SocketAddr,
};

use anyhow::{Context, Result};
use log::{debug, error, info};
use mio::{net::TcpListener, Events, Poll, Token};
use slab::Slab;

const MAX_CLIENTS: usize = 1024;
const LISTENER: Token = Token(MAX_CLIENTS + 1);

#[derive(Debug)]
struct Server {
    listener: TcpListener,
    poll: Poll,
    events: Events,
    connections: Slab<Connection>,
}

impl Server {
    fn bind(
        addr: SocketAddr,
        poll: Poll,
        events: Events,
        connections: Slab<Connection>,
    ) -> Result<Self> {
        let mut listener = TcpListener::bind(addr)?;

        poll.registry()
            .register(&mut listener, LISTENER, mio::Interest::READABLE)
            .with_context(|| "Failed to register listener")?;

        info!("server listening on {:?}", addr);
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
                                        self.poll
                                            .registry()
                                            .deregister(&mut connection.socket)
                                            .with_context(|| "Failed to deregister socket")?;
                                        self.connections.try_remove(token.0);
                                    }
                                    Ok(_) => {
                                        if let Err(e) = connection.write() {
                                            if e.kind() == std::io::ErrorKind::WouldBlock {
                                                self.poll
                                                    .registry()
                                                    .reregister(
                                                        &mut connection.socket,
                                                        token,
                                                        mio::Interest::READABLE
                                                            .add(mio::Interest::WRITABLE),
                                                    )
                                                    .with_context(|| {
                                                        "readable branch: Failed to reregister socket as READABLE and WRITABLE"
                                                    })?;
                                                continue;
                                            }
                                            error!("readable branch: Failed to write: {:?}", e);
                                            return Err(e.into());
                                        }
                                    }
                                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                        continue;
                                    }
                                    Err(e) => {
                                        error!("readable branch: Failed to read: {:?}", e);
                                        return Err(e.into());
                                    }
                                }
                            } else if event.is_writable() {
                                if let Err(e) = connection.write() {
                                    if e.kind() == std::io::ErrorKind::WouldBlock {
                                        self.poll.registry().reregister(
                                            &mut connection.socket,
                                            token,
                                            mio::Interest::READABLE.add(mio::Interest::WRITABLE),
                                        )
                                        .with_context(|| {
                                            "writeable branch: Failed to reregister socket as READABLE and WRITABLE"
                                        })?;
                                        continue;
                                    } else {
                                        error!("writeable branch: Failed to write: {:?}", e);
                                        return Err(e.into());
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

#[derive(Debug)]
struct Connection {
    socket: mio::net::TcpStream,
    buf: Vec<u8>,
    last_read_n: usize,
}

impl Connection {
    fn with_capacity(socket: mio::net::TcpStream, capacity: usize) -> Self {
        Self {
            socket,
            buf: vec![0; capacity],
            last_read_n: 0,
        }
    }

    fn read(&mut self) -> io::Result<usize> {
        if self.last_read_n >= self.buf.len() {
            return Ok(0);
        }

        let n = self.socket.read(&mut self.buf[self.last_read_n..])?;
        self.last_read_n += n;
        Ok(n)
    }

    fn write(&mut self) -> io::Result<()> {
        let n = self.socket.write(&self.buf[..self.last_read_n])?;
        if n > 0 {
            if n < self.last_read_n {
                self.buf.copy_within(n..self.last_read_n, 0);
            }
            self.last_read_n -= n;
        }

        Ok(())
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
    use super::*;
    use std::{net::TcpStream, thread, time::Duration};

    #[test]
    fn test_server_echo() -> Result<()> {
        env_logger::init();

        // Server setup
        let server_addr = "127.0.0.1:0".parse::<SocketAddr>()?; // Use port 0 for automatic port assignment
        let poll = Poll::new()?;
        let events = Events::with_capacity(MAX_CLIENTS);
        let connections = Slab::with_capacity(MAX_CLIENTS);
        let mut server = Server::bind(server_addr, poll, events, connections)?;
        let actual_addr = server.listener.local_addr()?;

        thread::spawn(move || {
            server.run().unwrap();
        });

        thread::sleep(Duration::from_millis(100));

        // Test 1: Single message
        let mut stream = TcpStream::connect(actual_addr)?;
        stream.set_nodelay(true)?;

        let test_msg = b"Hello, Server!";
        stream.write_all(test_msg)?;
        stream.flush()?;

        let mut buf = vec![0; test_msg.len()];
        stream.read_exact(&mut buf)?;
        assert_eq!(&buf, test_msg);

        // Test 2: Multiple writes before read
        let mut stream = TcpStream::connect(actual_addr)?;
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

        // Test 3: Large message
        let mut stream = TcpStream::connect(actual_addr)?;
        stream.set_nodelay(true)?;

        let test_msg = vec![b'X'; 1024];
        stream.write_all(&test_msg)?;
        stream.flush()?;

        let mut buf = vec![0; test_msg.len()];
        stream.read_exact(&mut buf)?;
        assert_eq!(buf, test_msg);

        // Test 4: Message larger than buffer
        let mut stream = TcpStream::connect(actual_addr)?;
        stream.set_nodelay(true)?;

        let buffer_size = 1024; // Same as in Connection::with_capacity
        let test_msg = vec![b'X'; buffer_size + 500]; // Exceeds buffer size
        stream.write_all(&test_msg)?;
        stream.flush()?;

        let mut buf = vec![0; buffer_size]; // Should only receive buffer_size bytes
        stream.read_exact(&mut buf)?;
        assert_eq!(&buf[..], &test_msg[..buffer_size]);

        Ok(())
    }
}
