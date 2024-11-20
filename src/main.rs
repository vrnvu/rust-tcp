use std::net::SocketAddr;

use anyhow::Result;
use log::{debug, error};

mod connection;
use connection::Connection;
use mio::{
    net::{TcpListener, TcpStream},
    Events, Poll, Token,
};
use slab::Slab;

const MAX_CLIENTS: usize = 1024;
const LISTENER: Token = Token(MAX_CLIENTS + 1);

struct Server {
    listener: TcpListener,
    poll: Poll,
    events: Events,
    connections: Slab<Connection<TcpStream>>,
}

impl Server {
    fn bind(addr: SocketAddr) -> Result<Self> {
        let mut listener = TcpListener::bind(addr)?;
        debug!("server listening on {:?}", listener.local_addr()?);

        let poll = Poll::new()?;
        let events = Events::with_capacity(MAX_CLIENTS + 1);
        let connections = Slab::with_capacity(MAX_CLIENTS);

        poll.registry()
            .register(&mut listener, LISTENER, mio::Interest::READABLE)?;

        Ok(Self {
            listener,
            poll,
            events,
            connections,
        })
    }

    fn run(&mut self) -> Result<()> {
        loop {
            self.poll.poll(&mut self.events, None)?;
            for event in self.events.iter() {
                match event.token() {
                    LISTENER => {
                        if let Ok((socket, addr)) = self.listener.accept() {
                            debug!("server: Connection from {:?}", addr);

                            let connection = Connection::with_capacity(socket, 1024);
                            let key = self.connections.insert(connection);
                            while let Err(e) = self.poll.registry().register(
                                &mut self.connections[key].socket,
                                Token(key),
                                mio::Interest::READABLE,
                            ) {
                                error!("server: Failed to register client: {:?}", e);
                            }
                        } else {
                            debug!("server: Failed to accept connection");
                        }
                    }
                    token => {
                        if let Some(connection) = self.connections.get_mut(token.0) {
                            if event.is_readable() {
                                if let Ok(n) = connection.read() {
                                    if n == 0 {
                                        debug!("Client disconnected");
                                        while let Err(e) =
                                            self.poll.registry().deregister(&mut connection.socket)
                                        {
                                            error!("server: Failed to deregister client: {:?}", e);
                                        }
                                        self.connections.remove(token.0);
                                        continue;
                                    } else {
                                        while let Err(e) = self.poll.registry().reregister(
                                            &mut connection.socket,
                                            token,
                                            mio::Interest::READABLE.add(mio::Interest::WRITABLE),
                                        ) {
                                            error!("server: Failed to reregister client: {:?}", e);
                                        }
                                    }
                                }
                            } else if event.is_writable() {
                                if let Err(e) = connection.write() {
                                    error!("server: Failed to write to client: {:?}", e);
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
    env_logger::init();

    let mut server = Server::bind("127.0.0.1:8080".parse()?)?;
    server.run()?;

    Ok(())
}

#[cfg(test)]
mod tests {

    use super::*;
    use std::{
        io::{Read, Write},
        net::{SocketAddr, TcpStream},
        thread::{self},
    };

    fn setup_server() -> Result<(Server, SocketAddr)> {
        let server_addr = "127.0.0.1:0".parse::<SocketAddr>()?;
        let server = Server::bind(server_addr)?;
        let local_addr = server.listener.local_addr()?;
        Ok((server, local_addr))
    }

    #[test]
    fn test_server_single_message() -> Result<()> {
        let (mut server, local_addr) = setup_server()?;
        thread::spawn(move || {
            server.run().unwrap();
        });

        let mut stream = TcpStream::connect(local_addr)?;
        stream.set_nodelay(true)?;

        let test_msg = b"Hello, Server!";
        stream.write_all(test_msg)?;
        stream.flush()?;

        let mut buf = vec![0; test_msg.len()];
        stream.read_exact(&mut buf)?;
        assert_eq!(&buf, test_msg);

        Ok(())
    }

    #[test]
    fn test_server_multiple_writes() -> Result<()> {
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
    fn test_server_large_message() -> Result<()> {
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
    fn test_server_message_larger_than_buffer() -> Result<()> {
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

    #[test]
    fn test_server_concurrent_clients() -> Result<()> {
        let (mut server, local_addr) = setup_server()?;
        thread::spawn(move || server.run().unwrap());

        let mut handles = vec![];
        let num_clients = 5;

        for i in 0..num_clients {
            let addr = local_addr;
            handles.push(thread::spawn(move || {
                let mut stream = TcpStream::connect(addr).unwrap();
                stream.set_nodelay(true).unwrap();

                let msg = format!("Client{}", i);
                stream.write_all(msg.as_bytes()).unwrap();
                stream.flush().unwrap();

                let mut buf = vec![0; msg.len()];
                stream.read_exact(&mut buf).unwrap();
                assert_eq!(buf, msg.as_bytes());
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        Ok(())
    }

    #[test]
    fn test_server_client_disconnect_reconnect() -> Result<()> {
        let (mut server, local_addr) = setup_server()?;
        thread::spawn(move || server.run().unwrap());

        // First connection
        let mut stream = TcpStream::connect(local_addr)?;
        stream.write_all(b"hello")?;
        stream.flush()?;
        drop(stream); // Force disconnect

        // Small delay to ensure disconnect is processed
        thread::sleep(std::time::Duration::from_millis(10));

        // Reconnect
        let mut stream2 = TcpStream::connect(local_addr)?;
        stream2.write_all(b"world")?;
        stream2.flush()?;

        let mut buf = vec![0; 5];
        stream2.read_exact(&mut buf)?;
        assert_eq!(&buf, b"world");

        Ok(())
    }

    #[test]
    fn test_server_zero_length_message() -> Result<()> {
        let (mut server, local_addr) = setup_server()?;
        thread::spawn(move || server.run().unwrap());

        let mut stream = TcpStream::connect(local_addr)?;
        stream.set_nodelay(true)?;

        stream.write_all(b"")?;
        stream.flush()?;

        // Write a real message after empty one
        let msg = b"test";
        stream.write_all(msg)?;
        stream.flush()?;

        let mut buf = vec![0; msg.len()];
        stream.read_exact(&mut buf)?;
        assert_eq!(&buf, msg);

        Ok(())
    }

    #[test]
    fn test_server_rapid_connect_disconnect() -> Result<()> {
        let (mut server, local_addr) = setup_server()?;
        thread::spawn(move || server.run().unwrap());

        for _ in 0..10 {
            let stream = TcpStream::connect(local_addr)?;
            drop(stream);
        }

        // Final connection to verify server still works
        let mut stream = TcpStream::connect(local_addr)?;
        stream.write_all(b"test")?;
        stream.flush()?;

        let mut buf = vec![0; 4];
        stream.read_exact(&mut buf)?;
        assert_eq!(&buf, b"test");

        Ok(())
    }
}
