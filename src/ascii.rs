/// This is a simplified implementation of [rust-memcache](https://github.com/aisk/rust-memcache)
/// ported for AsyncRead + AsyncWrite.
use core::fmt::Display;
use futures::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use std::collections::HashMap;
use std::io::{Error, ErrorKind};
use std::marker::Unpin;

pub struct Protocol<S: AsyncRead + AsyncWrite> {
    io: BufReader<S>,
}

impl<S> Protocol<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    /// Creates the ASCII protocol on a stream.
    pub fn new(io: S) -> Self {
        Self { io: BufReader::new(io) }
    }

    /// Returns the value for given key as bytes. If the value doesn't exist, `std::io::ErrorKind::NotFound` is returned.
    pub async fn get<'a, K: Display>(&'a mut self, key: &'a K) -> Result<Vec<u8>, Error> {
        // Send command
        let header = format!("get {}\r\n", key);
        self.io.get_mut().write_all(header.as_bytes()).await?;
        self.io.get_mut().flush().await?;

        // Read response header
        let mut buf = vec![];
        self.io.read_until(b'\n', &mut buf).await?;
        let header = String::from_utf8(buf).map_err(|_| ErrorKind::InvalidData)?;

        // Check response header and parse value length
        if header.contains("ERROR") {
            return Err(Error::new(ErrorKind::Other, header));
        } else if header.starts_with("END") {
            return Err(ErrorKind::NotFound.into());
        }

        // VALUE <key> <flags> <bytes> [<cas unique>]\r\n
        let length: usize = header.split(' ').nth(3)
            .and_then(|len| len.trim_end().parse().ok())
            .ok_or(ErrorKind::InvalidData)?;

        // Read value
        let mut buffer: Vec<u8> = vec![0; length];
        self.io.read_exact(&mut buffer).await?;

        // Read the trailing header
        let mut buf = vec![];
        self.io.read_until(b'\n', &mut buf).await?; // \r\n
        self.io.read_until(b'\n', &mut buf).await?; // END\r\n

        Ok(buffer)
    }

    /// Returns values for multiple keys in a single call as a `HashMap` from keys to found values.
    /// If a key is not present in memcached it will be absent from returned map.
    pub async fn get_multi<'a, K: AsRef<[u8]>>(
        &'a mut self,
        keys: &'a [K],
    ) -> Result<HashMap<String, Vec<u8>>, Error> {
        // Send command
        let writer = self.io.get_mut();
        writer.write_all("get".as_bytes()).await?;
        for k in keys {
            writer.write(b" ").await?;
            writer.write_all(k.as_ref()).await?;
        }
        writer.write(b"\r\n").await?;
        writer.flush().await?;

        // Read response header
        self.read_many_values().await
    }

    async fn read_many_values(&mut self) -> Result<HashMap<String, Vec<u8>>, Error> {
        let mut reader = BufReader::new(&mut self.io);
        let mut map = HashMap::new();
        loop {
            let header = {
                let mut buf = vec![];
                reader.read_until(b'\n', &mut buf).await?;
                String::from_utf8(buf).map_err(|_| Error::from(ErrorKind::InvalidData))?
            };
            let mut parts = header.split(' ');
            match parts.next() {
                Some("VALUE") => {
                    if let (Some(key), _flags, Some(size_str)) =
                        (parts.next(), parts.next(), parts.next())
                    {
                        let size: usize = size_str
                            .trim_end()
                            .parse()
                            .map_err(|_| Error::from(ErrorKind::InvalidData))?;
                        let mut buffer: Vec<u8> = vec![0; size];
                        reader.read_exact(&mut buffer).await?;
                        let mut crlf = vec![0; 2];
                        reader.read_exact(&mut crlf).await?;

                        map.insert(key.to_owned(), buffer);
                    } else {
                        return Err(Error::new(ErrorKind::InvalidData, header));
                    }
                }
                Some("END\r\n") => return Ok(map),
                Some("ERROR") => return Err(Error::new(ErrorKind::Other, header)),
                _ => return Err(Error::new(ErrorKind::InvalidData, header)),
            }
        }
    }

    /// Get up to `limit` keys which match the given prefix. Returns a HashMap from keys to found values.
    /// This is not part of the Memcached standard, but some servers implement it nonetheless.
    pub async fn get_prefix<'a, K: Display>(
        &'a mut self,
        key_prefix: &'a K,
        limit: Option<usize>,
    ) -> Result<HashMap<String, Vec<u8>>, Error> {
        // Send command
        let header = if let Some(limit) = limit {
            format!("get_prefix {} {}\r\n", key_prefix, limit)
        } else {
            format!("get_prefix {}\r\n", key_prefix)
        };
        self.io.write_all(header.as_bytes()).await?;
        self.io.flush().await?;

        // Read response header
        self.read_many_values().await
    }

    /// Add a key. If the value exist, `std::io::ErrorKind::AllreadyExists` is returned.
    pub async fn add<'a, K: Display>(
        &'a mut self,
        key: &'a K,
        val: &'a [u8],
        expiration: u32,
    ) -> Result<(), Error> {
        // Send command
        let header = format!("add {} 0 {} {}\r\n", key, expiration, val.len());
        self.io.write_all(header.as_bytes()).await?;
        self.io.write_all(val).await?;
        self.io.write_all(b"\r\n").await?;
        self.io.flush().await?;

        // Read response header
        let mut reader = BufReader::new(&mut self.io);
        let header = {
            let mut buf = vec![];
            reader.read_until(b'\n', &mut buf).await?;
            String::from_utf8(buf).map_err(|_| Error::from(ErrorKind::InvalidData))?
        };

        // Check response header and parse value length
        if header.contains("ERROR") {
            return Err(Error::new(ErrorKind::Other, header));
        } else if header.starts_with("NOT_STORED") {
            return Err(ErrorKind::AlreadyExists.into());
        }

        Ok(())
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

    /// Delete a key and don't wait for response.
    pub async fn delete<'a, K: Display>(&'a mut self, key: &'a K) -> Result<(), Error> {
        let header = format!("delete {} noreply\r\n", key);
        self.io.write_all(header.as_bytes()).await?;
        self.io.flush().await?;
        Ok(())
    }

    pub async fn version(&mut self) -> Result<String, Error> {
        self.io.write_all(b"version\r\n").await?;
        self.io.flush().await?;

        // Read response header
        let mut reader = BufReader::new(&mut self.io);
        let response = {
            let mut buf = vec![];
            reader.read_until(b'\n', &mut buf).await?;
            String::from_utf8(buf).map_err(|_| Error::from(ErrorKind::InvalidData))?
        };

        if !response.starts_with("VERSION") {
            return Err(Error::new(ErrorKind::Other, response));
        }
        let version = response.trim_start_matches("VERSION ").trim_end();
        Ok(version.to_string())
    }

    pub async fn flush(&mut self) -> Result<(), Error> {
        self.io.write_all(b"flush_all\r\n").await?;
        self.io.flush().await?;

        // Read response header
        let mut reader = BufReader::new(&mut self.io);
        let response = {
            let mut buf = vec![];
            reader.read_until(b'\n', &mut buf).await?;
            String::from_utf8(buf).map_err(|_| Error::from(ErrorKind::InvalidData))?
        };

        if response == "OK\r\n" {
            Ok(())
        } else {
            Err(Error::new(ErrorKind::Other, response))
        }
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
    fn test_ascii_get2() {
        let mut cache = Cache::new();
        cache
            .r
            .get_mut()
            .extend_from_slice(b"VALUE foo 0 3\r\nbar\r\nEND\r\nVALUE bar 0 3\r\nbaz\r\nEND\r\n");
        let mut ascii = super::Protocol::new(&mut cache);
        assert_eq!(block_on(ascii.get(&"foo")).unwrap(), b"bar");
        assert_eq!(block_on(ascii.get(&"bar")).unwrap(), b"baz");
    }

    #[test]
    fn test_ascii_get_cas() {
        let mut cache = Cache::new();
        cache
            .r
            .get_mut()
            .extend_from_slice(b"VALUE foo 0 3 99999\r\nbar\r\nEND\r\n");
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
    fn test_ascii_get_one() {
        let mut cache = Cache::new();
        cache
            .r
            .get_mut()
            .extend_from_slice(b"VALUE foo 0 3\r\nbar\r\nEND\r\n");
        let mut ascii = super::Protocol::new(&mut cache);
        let keys = vec!["foo"];
        let map = block_on(ascii.get_multi(&keys)).unwrap();
        assert_eq!(map.len(), 1);
        assert_eq!(map.get("foo").unwrap(), b"bar");
        assert_eq!(cache.w.get_ref(), b"get foo\r\n");
    }

    #[test]
    fn test_ascii_get_many() {
        let mut cache = Cache::new();
        cache
            .r
            .get_mut()
            .extend_from_slice(b"VALUE foo 0 3\r\nbar\r\nVALUE baz 44 4\r\ncrux\r\nEND\r\n");
        let mut ascii = super::Protocol::new(&mut cache);
        let keys = vec!["foo", "baz", "blah"];
        let map = block_on(ascii.get_multi(&keys)).unwrap();
        assert_eq!(map.len(), 2);
        assert_eq!(map.get("foo").unwrap(), b"bar");
        assert_eq!(map.get("baz").unwrap(), b"crux");
        assert_eq!(cache.w.get_ref(), b"get foo baz blah\r\n");
    }

    #[test]
    fn test_ascii_get_prefix() {
        let mut cache = Cache::new();
        cache
            .r
            .get_mut()
            .extend_from_slice(b"VALUE key 0 3\r\nbar\r\nVALUE kez 44 4\r\ncrux\r\nEND\r\n");
        let mut ascii = super::Protocol::new(&mut cache);
        let key_prefix = "ke";
        let map = block_on(ascii.get_prefix(&key_prefix, None)).unwrap();
        assert_eq!(map.len(), 2);
        assert_eq!(map.get("key").unwrap(), b"bar");
        assert_eq!(map.get("kez").unwrap(), b"crux");
        assert_eq!(cache.w.get_ref(), b"get_prefix ke\r\n");
    }

    #[test]
    fn test_ascii_get_multi_empty() {
        let mut cache = Cache::new();
        cache.r.get_mut().extend_from_slice(b"END\r\n");
        let mut ascii = super::Protocol::new(&mut cache);
        let keys = vec!["foo", "baz"];
        let map = block_on(ascii.get_multi(&keys)).unwrap();
        assert!(map.is_empty());
        assert_eq!(cache.w.get_ref(), b"get foo baz\r\n");
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

    #[test]
    fn test_ascii_version() {
        let mut cache = Cache::new();
        cache.r.get_mut().extend_from_slice(b"VERSION 1.6.6\r\n");
        let mut ascii = super::Protocol::new(&mut cache);
        assert_eq!(block_on(ascii.version()).unwrap(), "1.6.6");
        assert_eq!(cache.w.get_ref(), b"version\r\n");
    }

    #[test]
    fn test_ascii_flush() {
        let mut cache = Cache::new();
        cache.r.get_mut().extend_from_slice(b"OK\r\n");
        let mut ascii = super::Protocol::new(&mut cache);
        assert!(block_on(ascii.flush()).is_ok());
        assert_eq!(cache.w.get_ref(), b"flush_all\r\n");
    }
}
