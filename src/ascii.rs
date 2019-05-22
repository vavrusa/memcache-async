/// This is a simplified implementation of [rust-memcache](https://github.com/aisk/rust-memcache)
/// ported for AsyncRead + AsyncWrite.
use core::fmt::Display;
use futures::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use std::io::{Error, ErrorKind};
use std::marker::Unpin;

pub struct Protocol<S: AsyncRead + AsyncWrite> {
    io: S,
}

impl<S> Protocol<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    /// Creates the ASCII protocol on a stream.
    pub fn new(io: S) -> Self {
        Self { io }
    }

    /// Returns the value for given key as bytes. If the value doesn't exist, `std::io::ErrorKind::NotFound` is returned.
    pub async fn get<'a, K: Display>(&'a mut self, key: &'a K) -> Result<Vec<u8>, Error> {
        // Send command
        let header = format!("get {}\r\n", key);
        self.io.write_all(header.as_bytes()).await?;
        self.io.flush().await?;

        // Read response header
        let mut reader = BufReader::new(&mut self.io);
        let header = {
            let mut buf = vec![];
            drop(reader.read_until(b'\n', &mut buf).await?);
            String::from_utf8(buf).map_err(|_| Error::from(ErrorKind::InvalidInput))?
        };

        // Check response header and parse value length
        if header.contains("ERROR") {
            return Err(Error::new(ErrorKind::Other, header));
        } else if header.starts_with("END") {
            return Err(ErrorKind::NotFound.into());
        }

        let length_str = header.trim_end().rsplitn(2, ' ').next();
        let length: usize = match length_str {
            Some(x) => x
                .parse()
                .map_err(|_| Error::from(ErrorKind::InvalidInput))?,
            None => return Err(ErrorKind::InvalidInput.into()),
        };

        // Read value
        let mut buffer: Vec<u8> = vec![0; length];
        drop(reader.read_exact(&mut buffer).await?);

        // Read the trailing header
        let mut buf = vec![];
        drop(reader.read_until(b'\n', &mut buf).await?);
        drop(reader.read_until(b'\n', &mut buf).await?);

        Ok(buffer)
    }

    /// Set key to given value and don't wait for response.
    pub async fn set<'a, K: Display>(
        &'a mut self,
        key: &'a K,
        val: &'a [u8],
        expiration: u32,
    ) -> Result<(), Error> {
        let header = format!("set {} 0 {} {} noreply\r\n", key, expiration, val.len());
        self.io.write_all(header.as_bytes()).await?;
        self.io.write_all(val).await?;
        self.io.write_all(b"\r\n").await?;
        self.io.flush().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use futures::executor::block_on;
    use futures::io::{AsyncRead, AsyncWrite};
    use std::io::{Cursor, Error, ErrorKind, Read, Write};
    use std::pin::Pin;
    use std::task::{Context, Poll};

    struct Cache {
        r: Cursor<Vec<u8>>,
        w: Cursor<Vec<u8>>,
    }

    impl Cache {
        fn new() -> Self {
            Cache {
                r: Cursor::new(Vec::new()),
                w: Cursor::new(Vec::new()),
            }
        }
    }

    impl AsyncRead for Cache {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut Context,
            buf: &mut [u8],
        ) -> Poll<Result<usize, Error>> {
            Poll::Ready(self.get_mut().r.read(buf))
        }
    }

    impl AsyncWrite for Cache {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context,
            buf: &[u8],
        ) -> Poll<Result<usize, Error>> {
            Poll::Ready(self.get_mut().w.write(buf))
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context) -> Poll<Result<(), Error>> {
            Poll::Ready(self.get_mut().w.flush())
        }

        fn poll_close(self: Pin<&mut Self>, _cx: &mut Context) -> Poll<Result<(), Error>> {
            Poll::Ready(Ok(()))
        }
    }

    #[test]
    fn test_ascii_get() {
        let mut cache = Cache::new();
        cache
            .r
            .get_mut()
            .extend_from_slice(b"VALUE foo 0 3\r\nbar\r\nEND\r\n");
        let mut ascii = super::Protocol::new(&mut cache);
        assert_eq!(block_on(ascii.get(&"foo")).unwrap(), b"bar");
        assert_eq!(cache.w.get_ref(), b"get foo\r\n");
    }

    #[test]
    fn test_ascii_get_empty() {
        let mut cache = Cache::new();
        cache.r.get_mut().extend_from_slice(b"END\r\n");
        let mut ascii = super::Protocol::new(&mut cache);
        assert_eq!(
            block_on(ascii.get(&"foo")).unwrap_err().kind(),
            ErrorKind::NotFound
        );
        assert_eq!(cache.w.get_ref(), b"get foo\r\n");
    }

    #[test]
    fn test_ascii_set() {
        let (key, val, ttl) = ("foo", "bar", 5);
        let mut cache = Cache::new();
        let mut ascii = super::Protocol::new(&mut cache);
        block_on(ascii.set(&key, val.as_bytes(), ttl)).unwrap();
        assert_eq!(
            cache.w.get_ref(),
            &format!("set {} 0 {} {} noreply\r\n{}\r\n", key, ttl, val.len(), val)
                .as_bytes()
                .to_vec()
        );
    }
}
