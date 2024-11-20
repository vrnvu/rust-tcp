use std::io::{self, Read, Write};

#[derive(Debug)]
pub struct Connection<T: Read + Write> {
    pub(crate) socket: T,
    buf: Vec<u8>,
    last_read_n: usize,
}

impl<T: Read + Write> Connection<T> {
    pub fn with_capacity(socket: T, capacity: usize) -> Self {
        Self {
            socket,
            buf: vec![0; capacity],
            last_read_n: 0,
        }
    }

    pub fn read(&mut self) -> io::Result<usize> {
        if self.last_read_n >= self.buf.len() {
            return Ok(0);
        }

        let n = self.socket.read(&mut self.buf[self.last_read_n..])?;
        self.last_read_n += n;
        Ok(n)
    }

    pub fn write(&mut self) -> io::Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;

    // Simple socket that reads/writes all data at once
    struct StreamSocket {
        read_data: Vec<u8>,
        written_data: Vec<u8>,
        read_pos: usize,
    }

    impl StreamSocket {
        fn new(read_data: Vec<u8>) -> Self {
            Self {
                read_data,
                written_data: Vec::new(),
                read_pos: 0,
            }
        }
    }

    impl Read for StreamSocket {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.read_pos >= self.read_data.len() {
                return Ok(0);
            }
            let available = &self.read_data[self.read_pos..];
            let n = std::cmp::min(available.len(), buf.len());
            buf[..n].copy_from_slice(&available[..n]);
            self.read_pos += n;
            Ok(n)
        }
    }

    impl Write for StreamSocket {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.written_data.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    // Socket that simulates partial reads/writes
    struct ChunkedSocket {
        read_data: Vec<u8>,
        written_data: Vec<u8>,
        read_pos: usize,
        chunk_size: usize,
    }

    impl ChunkedSocket {
        fn new(read_data: Vec<u8>, chunk_size: usize) -> Self {
            Self {
                read_data,
                written_data: Vec::new(),
                read_pos: 0,
                chunk_size,
            }
        }
    }

    impl Read for ChunkedSocket {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.read_pos >= self.read_data.len() {
                return Ok(0);
            }
            let available = &self.read_data[self.read_pos..];
            let n = std::cmp::min(std::cmp::min(available.len(), buf.len()), self.chunk_size);
            buf[..n].copy_from_slice(&available[..n]);
            self.read_pos += n;
            Ok(n)
        }
    }

    impl Write for ChunkedSocket {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let n = std::cmp::min(buf.len(), self.chunk_size);
            self.written_data.extend_from_slice(&buf[..n]);
            Ok(n)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn test_connection_read() -> io::Result<()> {
        let test_data = b"hello world";
        let socket = StreamSocket::new(test_data.to_vec());
        let mut connection = Connection::with_capacity(socket, 16);

        let n = connection.read()?;
        assert_eq!(n, test_data.len());
        assert_eq!(&connection.buf[..n], test_data);
        Ok(())
    }

    #[test]
    fn test_connection_write() -> io::Result<()> {
        let socket = StreamSocket::new(Vec::new());
        let mut connection = Connection::with_capacity(socket, 16);

        let test_data = b"hello world";
        connection.buf[..test_data.len()].copy_from_slice(test_data);
        connection.last_read_n = test_data.len();

        connection.write()?;
        assert_eq!(&connection.socket.written_data, test_data);
        Ok(())
    }

    #[test]
    fn test_connection_partial_write() -> io::Result<()> {
        let socket = ChunkedSocket::new(Vec::new(), 5);
        let mut connection = Connection::with_capacity(socket, 16);

        let test_data = b"hello world";
        connection.buf[..test_data.len()].copy_from_slice(test_data);
        connection.last_read_n = test_data.len();

        connection.write()?;
        assert_eq!(&connection.buf[..connection.last_read_n], b" world");
        assert_eq!(&connection.socket.written_data, b"hello");
        Ok(())
    }

    #[test]
    fn test_connection_chunked_read() -> io::Result<()> {
        let test_data = b"hello world";
        let socket = ChunkedSocket::new(test_data.to_vec(), 5);
        let mut connection = Connection::with_capacity(socket, 16);

        let n = connection.read()?;
        assert_eq!(n, 5);
        assert_eq!(&connection.buf[..n], b"hello");

        let n = connection.read()?;
        assert_eq!(n, 5);
        assert_eq!(&connection.buf[5..5 + n], b" worl");

        let n = connection.read()?;
        assert_eq!(n, 1);
        assert_eq!(&connection.buf[10..10 + n], b"d");
        Ok(())
    }
}
