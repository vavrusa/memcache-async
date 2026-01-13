//! This is a simplified implementation of [rust-memcache](https://github.com/aisk/rust-memcache)
//! ported for AsyncRead + AsyncWrite.
use core::fmt::Display;
#[cfg(feature = "with-futures")]
use futures::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};

#[cfg(feature = "with-tokio")]
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};

use std::collections::HashMap;
use std::io::{Error, ErrorKind};
use std::marker::Unpin;

/// Check if a response header indicates an error from memcached.
fn is_memcached_error(header: &str) -> bool {
    header.starts_with("ERROR")
        || header.starts_with("CLIENT_ERROR")
        || header.starts_with("SERVER_ERROR")
}

/// Memcache ASCII protocol implementation.
pub struct Protocol<S> {
    io: BufReader<S>,
    buf: Vec<u8>,
}

impl<S> Protocol<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    /// Creates the ASCII protocol on a stream.
    pub fn new(io: S) -> Self {
        Self {
            io: BufReader::new(io),
            buf: Vec::new(),
        }
    }

    /// Returns the value for given key as bytes. If the value doesn't exist, [`ErrorKind::NotFound`] is returned.
    pub async fn get<K: AsRef<[u8]>>(&mut self, key: K) -> Result<Vec<u8>, Error> {
        // Send command
        let writer = self.io.get_mut();
        writer
            .write_all(&[b"get ", key.as_ref(), b"\r\n"].concat())
            .await?;
        writer.flush().await?;

        let (val, _) = self.read_get_response().await?;
        Ok(val)
    }

    async fn read_get_response(&mut self) -> Result<(Vec<u8>, Option<u64>), Error> {
        // Read response header
        let header = self.read_line().await?;
        let header = std::str::from_utf8(header).map_err(|_| ErrorKind::InvalidData)?;

        // Check response header and parse value length
        if is_memcached_error(header) {
            return Err(Error::new(ErrorKind::Other, header));
        } else if header.starts_with("END") {
            return Err(ErrorKind::NotFound.into());
        }

        // VALUE <key> <flags> <bytes> [<cas unique>]\r\n
        let mut parts = header.split(' ');
        let length: usize = parts
            .nth(3)
            .and_then(|len| len.trim_end().parse().ok())
            .ok_or(ErrorKind::InvalidData)?;

        // cas is present only if gets is called
        let cas: Option<u64> = parts.next().and_then(|len| len.trim_end().parse().ok());

        // Read value
        let mut buffer: Vec<u8> = vec![0; length];
        self.io.read_exact(&mut buffer).await?;

        // Read the trailing header
        self.read_line().await?; // \r\n
        self.read_line().await?; // END\r\n

        Ok((buffer, cas))
    }

    /// Returns values for multiple keys in a single call as a [`HashMap`] from keys to found values.
    /// If a key is not present in memcached it will be absent from returned map.
    pub async fn get_multi<K: AsRef<[u8]>>(
        &mut self,
        keys: &[K],
    ) -> Result<HashMap<String, Vec<u8>>, Error> {
        if keys.is_empty() {
            return Ok(HashMap::new());
        }

        // Send command
        let writer = self.io.get_mut();
        writer.write_all("get".as_bytes()).await?;
        for k in keys {
            writer.write_all(b" ").await?;
            writer.write_all(k.as_ref()).await?;
        }
        writer.write_all(b"\r\n").await?;
        writer.flush().await?;

        // Read response header
        self.read_many_values().await
    }

    async fn read_many_values(&mut self) -> Result<HashMap<String, Vec<u8>>, Error> {
        let mut map = HashMap::new();
        loop {
            let header = {
                let buf = self.read_line().await?;
                std::str::from_utf8(buf).map_err(|_| Error::from(ErrorKind::InvalidData))?
            }
            .to_string();
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
                        self.io.read_exact(&mut buffer).await?;
                        let mut crlf = vec![0; 2];
                        self.io.read_exact(&mut crlf).await?;

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

    /// Get up to `limit` keys which match the given prefix. Returns a [HashMap] from keys to found values.
    /// This is not part of the Memcached standard, but some servers implement it nonetheless.
    pub async fn get_prefix<K: Display>(
        &mut self,
        key_prefix: K,
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

    /// Add a key. If the value exists, [`ErrorKind::AlreadyExists`] is returned.
    pub async fn add<K: Display>(
        &mut self,
        key: K,
        val: &[u8],
        expiration: u32,
    ) -> Result<(), Error> {
        // Send command
        let header = format!("add {} 0 {} {}\r\n", key, expiration, val.len());
        self.io.write_all(header.as_bytes()).await?;
        self.io.write_all(val).await?;
        self.io.write_all(b"\r\n").await?;
        self.io.flush().await?;

        // Read response header
        let header = {
            let buf = self.read_line().await?;
            std::str::from_utf8(buf).map_err(|_| Error::from(ErrorKind::InvalidData))?
        };

        // Check response header and parse value length
        if is_memcached_error(header) {
            return Err(Error::new(ErrorKind::Other, header));
        } else if header.starts_with("NOT_STORED") {
            return Err(ErrorKind::AlreadyExists.into());
        }

        Ok(())
    }

    /// Set key to given value and don't wait for response.
    pub async fn set<K: Display>(
        &mut self,
        key: K,
        val: &[u8],
        expiration: u32,
    ) -> Result<(), Error> {
        let header = format!("set {} 0 {} {} noreply\r\n", key, expiration, val.len());
        self.io.write_all(header.as_bytes()).await?;
        self.io.write_all(val).await?;
        self.io.write_all(b"\r\n").await?;
        self.io.flush().await?;
        Ok(())
    }

    /// Append bytes to the value in memcached and don't wait for response.
    pub async fn append<K: Display>(&mut self, key: K, val: &[u8]) -> Result<(), Error> {
        let header = format!("append {} 0 0 {} noreply\r\n", key, val.len());
        self.io.write_all(header.as_bytes()).await?;
        self.io.write_all(val).await?;
        self.io.write_all(b"\r\n").await?;
        self.io.flush().await?;
        Ok(())
    }

    /// Delete a key and don't wait for response.
    pub async fn delete<K: Display>(&mut self, key: K) -> Result<(), Error> {
        let header = format!("delete {} noreply\r\n", key);
        self.io.write_all(header.as_bytes()).await?;
        self.io.flush().await?;
        Ok(())
    }

    /// Return the version of the remote server.
    pub async fn version(&mut self) -> Result<String, Error> {
        self.io.write_all(b"version\r\n").await?;
        self.io.flush().await?;

        // Read response header
        let header = {
            let buf = self.read_line().await?;
            std::str::from_utf8(buf).map_err(|_| Error::from(ErrorKind::InvalidData))?
        };

        if !header.starts_with("VERSION") {
            return Err(Error::new(ErrorKind::Other, header));
        }
        let version = header.trim_start_matches("VERSION ").trim_end();
        Ok(version.to_string())
    }

    /// Delete all keys from the cache.
    pub async fn flush(&mut self) -> Result<(), Error> {
        self.io.write_all(b"flush_all\r\n").await?;
        self.io.flush().await?;

        // Read response header
        let header = {
            let buf = self.read_line().await?;
            std::str::from_utf8(buf).map_err(|_| Error::from(ErrorKind::InvalidData))?
        };

        if header == "OK\r\n" {
            Ok(())
        } else {
            Err(Error::new(ErrorKind::Other, header))
        }
    }

    /// Increment a specific integer stored with a key by a given value. If the value doesn't exist, [`ErrorKind::NotFound`] is returned.
    /// Otherwise the new value is returned.
    ///
    /// Note: If the key exists but contains a non-numeric value, memcached will return a CLIENT_ERROR.
    pub async fn increment<K: AsRef<[u8]>>(&mut self, key: K, amount: u64) -> Result<u64, Error> {
        // Send command
        let writer = self.io.get_mut();
        let buf = &[
            b"incr ",
            key.as_ref(),
            b" ",
            amount.to_string().as_bytes(),
            b"\r\n",
        ]
        .concat();
        writer.write_all(buf).await?;
        writer.flush().await?;

        // Read response header
        let header = {
            let buf = self.read_line().await?;
            std::str::from_utf8(buf).map_err(|_| Error::from(ErrorKind::InvalidData))?
        };

        if header == "NOT_FOUND\r\n" {
            Err(ErrorKind::NotFound.into())
        } else if is_memcached_error(header) {
            Err(Error::new(ErrorKind::Other, header))
        } else {
            let value = header
                .trim_end()
                .parse::<u64>()
                .map_err(|_| Error::from(ErrorKind::InvalidData))?;
            Ok(value)
        }
    }

    /// Increment a specific integer stored with a key by a given value without waiting for a response.
    pub async fn increment_noreply<K: AsRef<[u8]>>(
        &mut self,
        key: K,
        amount: u64,
    ) -> Result<(), Error> {
        let writer = self.io.get_mut();
        let buf = &[
            b"incr ",
            key.as_ref(),
            b" ",
            amount.to_string().as_bytes(),
            b" noreply\r\n",
        ]
        .concat();
        writer.write_all(buf).await?;
        writer.flush().await?;
        Ok(())
    }

    /// Decrement a specific integer stored with a key by a given value. If the value doesn't exist, [`ErrorKind::NotFound`] is returned.
    /// Otherwise the new value is returned.
    ///
    /// Note: If the key exists but contains a non-numeric value, memcached will return a CLIENT_ERROR.
    pub async fn decrement<K: AsRef<[u8]>>(&mut self, key: K, amount: u64) -> Result<u64, Error> {
        // Send command
        let writer = self.io.get_mut();
        let buf = &[
            b"decr ",
            key.as_ref(),
            b" ",
            amount.to_string().as_bytes(),
            b"\r\n",
        ]
        .concat();
        writer.write_all(buf).await?;
        writer.flush().await?;

        // Read response header
        let header = {
            let buf = self.read_line().await?;
            std::str::from_utf8(buf).map_err(|_| Error::from(ErrorKind::InvalidData))?
        };

        if header == "NOT_FOUND\r\n" {
            Err(ErrorKind::NotFound.into())
        } else if is_memcached_error(header) {
            Err(Error::new(ErrorKind::Other, header))
        } else {
            let value = header
                .trim_end()
                .parse::<u64>()
                .map_err(|_| Error::from(ErrorKind::InvalidData))?;
            Ok(value)
        }
    }

    async fn read_line(&mut self) -> Result<&[u8], Error> {
        let Self { io, buf } = self;
        buf.clear();
        io.read_until(b'\n', buf).await?;
        if buf.last().copied() != Some(b'\n') {
            return Err(ErrorKind::UnexpectedEof.into());
        }
        Ok(&buf[..])
    }

    /// Call gets to also return CAS id, which can be used to run a second CAS command
    pub async fn gets_cas<K: AsRef<[u8]>>(&mut self, key: K) -> Result<(Vec<u8>, u64), Error> {
        // Send command
        let writer = self.io.get_mut();
        writer
            .write_all(&[b"gets ", key.as_ref(), b"\r\n"].concat())
            .await?;
        writer.flush().await?;

        let (val, maybe_cas) = self.read_get_response().await?;
        let cas = maybe_cas.ok_or(ErrorKind::InvalidData)?;

        Ok((val, cas))
    }

    // CAS: compare and swap a value. the value of `cas` can be obtained by first making a gets_cas
    // call
    // returns true/false to indicate the cas operation succeeded or failed
    // returns an error for all other failures
    pub async fn cas<K: Display>(
        &mut self,
        key: K,
        val: &[u8],
        cas_id: u64,
        expiration: u32,
    ) -> Result<bool, Error> {
        let header = format!("cas {} 0 {} {} {}\r\n", key, expiration, val.len(), cas_id);
        self.io.write_all(header.as_bytes()).await?;
        self.io.write_all(val).await?;
        self.io.write_all(b"\r\n").await?;
        self.io.flush().await?;

        // Read response header
        let header = {
            let buf = self.read_line().await?;
            std::str::from_utf8(buf).map_err(|_| Error::from(ErrorKind::InvalidData))?
        };

        /* From memcached docs:
         *    After sending the command line and the data block the client awaits
         *    the reply, which may be:
         *
         *    - "STORED\r\n", to indicate success.
         *
         *    - "NOT_STORED\r\n" to indicate the data was not stored, but not
         *    because of an error. This normally means that the
         *    condition for an "add" or a "replace" command wasn't met.
         *
         *    - "EXISTS\r\n" to indicate that the item you are trying to store with
         *    a "cas" command has been modified since you last fetched it.
         *
         *    - "NOT_FOUND\r\n" to indicate that the item you are trying to store
         *    with a "cas" command did not exist.
         */

        if header.starts_with("STORED") {
            Ok(true)
        } else if header.starts_with("EXISTS") || header.starts_with("NOT_STORED") {
            Ok(false)
        } else if header.starts_with("NOT FOUND") {
            Err(ErrorKind::NotFound.into())
        } else {
            Err(Error::new(ErrorKind::Other, header))
        }
    }

    /// Append bytes to the value in memcached, and creates the key if it is missing instead of failing compared to simple append
    pub async fn append_or_vivify<K: Display>(
        &mut self,
        key: K,
        val: &[u8],
        ttl: u32,
    ) -> Result<(), Error> {
        /* From memcached docs:
         * - M(token): mode switch to change behavior to add, replace, append, prepend. Takes a single character for the mode.
         *       A: "append" command. If item exists, append the new value to its data.
         * ----
         * The "cas" command is supplanted by specifying the cas value with the 'C' flag.
         * Append and Prepend modes will also respect a supplied cas value.
         *
         * - N(token): if in append mode, autovivify on miss with supplied TTL
         *
         * If N is supplied, and append reaches a miss, it will
         * create a new item seeded with the data from the append command.
         */
        let header = format!("ms {} {} MA N{}\r\n", key, val.len(), ttl);
        self.io.write_all(header.as_bytes()).await?;
        self.io.write_all(val).await?;
        self.io.write_all(b"\r\n").await?;
        self.io.flush().await?;

        // Read response header
        let header = {
            let buf = self.read_line().await?;
            std::str::from_utf8(buf).map_err(|_| Error::from(ErrorKind::InvalidData))?
        };

        /* From memcached docs:
         *    After sending the command line and the data block the client awaits
         *    the reply, which is of the format:
         *
         *    <CD> <flags>*\r\n
         *
         *    Where CD is one of:
         *
         *    - "HD" (STORED), to indicate success.
         *
         *    - "NS" (NOT_STORED), to indicate the data was not stored, but not
         *    because of an error.
         *
         *    - "EX" (EXISTS), to indicate that the item you are trying to store with
         *    CAS semantics has been modified since you last fetched it.
         *
         *    - "NF" (NOT_FOUND), to indicate that the item you are trying to store
         *    with CAS semantics did not exist.
         */

        if header.starts_with("HD") {
            Ok(())
        } else {
            Err(Error::new(ErrorKind::Other, header))
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
    fn test_ascii_get_eof_error() {
        let mut cache = Cache::new();
        cache.r.get_mut().extend_from_slice(b"EN");
        let mut ascii = super::Protocol::new(&mut cache);
        assert_eq!(
            block_on(ascii.get(&"foo")).unwrap_err().kind(),
            ErrorKind::UnexpectedEof
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
    fn test_ascii_get_multi_zero_keys() {
        let mut cache = Cache::new();
        cache.r.get_mut().extend_from_slice(b"END\r\n");
        let mut ascii = super::Protocol::new(&mut cache);
        let map = block_on(ascii.get_multi::<&str>(&[])).unwrap();
        assert!(map.is_empty());
        assert_eq!(cache.w.get_ref(), b"");
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
    fn test_ascii_increment_noreply() {
        let (key, amount) = ("foo", 5);
        let mut cache = Cache::new();
        let mut ascii = super::Protocol::new(&mut cache);
        block_on(ascii.increment_noreply(key.as_bytes(), amount)).unwrap();
        assert_eq!(
            cache.w.get_ref(),
            &format!("incr {} {} noreply\r\n", key, amount)
                .as_bytes()
                .to_vec()
        );
    }

    #[test]
    fn test_ascii_add_new_key() {
        let (key, val, ttl) = ("foo", "bar", 5);
        let mut cache = Cache::new();
        cache.r.get_mut().extend_from_slice(b"STORED\r\n");
        let mut ascii = super::Protocol::new(&mut cache);
        block_on(ascii.add(&key, val.as_bytes(), ttl)).unwrap();
        assert_eq!(
            cache.w.get_ref(),
            &format!("add {} 0 {} {}\r\n{}\r\n", key, ttl, val.len(), val)
                .as_bytes()
                .to_vec()
        );
    }

    #[test]
    fn test_ascii_add_duplicate() {
        let (key, val, ttl) = ("foo", "bar", 5);
        let mut cache = Cache::new();
        cache.r.get_mut().extend_from_slice(b"NOT_STORED\r\n");
        let mut ascii = super::Protocol::new(&mut cache);
        assert_eq!(
            block_on(ascii.add(&key, val.as_bytes(), ttl))
                .unwrap_err()
                .kind(),
            ErrorKind::AlreadyExists
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

    #[test]
    fn test_ascii_increment() {
        let mut cache = Cache::new();
        cache.r.get_mut().extend_from_slice(b"2\r\n");
        let mut ascii = super::Protocol::new(&mut cache);
        assert_eq!(block_on(ascii.increment("foo", 1)).unwrap(), 2);
        assert_eq!(cache.w.get_ref(), b"incr foo 1\r\n");
    }

    #[test]
    fn test_ascii_decrement() {
        let mut cache = Cache::new();
        cache.r.get_mut().extend_from_slice(b"0\r\n");
        let mut ascii = super::Protocol::new(&mut cache);
        assert_eq!(block_on(ascii.decrement("foo", 1)).unwrap(), 0);
        assert_eq!(cache.w.get_ref(), b"decr foo 1\r\n");
    }

    #[test]
    fn test_ascii_increment_not_found() {
        let mut cache = Cache::new();
        cache.r.get_mut().extend_from_slice(b"NOT_FOUND\r\n");
        let mut ascii = super::Protocol::new(&mut cache);
        let err = block_on(ascii.increment("foo", 1)).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::NotFound);
    }

    #[test]
    fn test_ascii_increment_client_error() {
        let mut cache = Cache::new();
        cache
            .r
            .get_mut()
            .extend_from_slice(b"CLIENT_ERROR cannot increment or decrement non-numeric value\r\n");
        let mut ascii = super::Protocol::new(&mut cache);
        let err = block_on(ascii.increment("foo", 1)).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Other);
        assert!(err
            .to_string()
            .contains("cannot increment or decrement non-numeric value"));
    }

    #[test]
    fn test_ascii_increment_server_error() {
        let mut cache = Cache::new();
        cache
            .r
            .get_mut()
            .extend_from_slice(b"SERVER_ERROR out of memory\r\n");
        let mut ascii = super::Protocol::new(&mut cache);
        let err = block_on(ascii.increment("foo", 1)).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Other);
        assert!(err.to_string().contains("out of memory"));
    }

    #[test]
    fn test_ascii_increment_error() {
        let mut cache = Cache::new();
        cache.r.get_mut().extend_from_slice(b"ERROR\r\n");
        let mut ascii = super::Protocol::new(&mut cache);
        let err = block_on(ascii.increment("foo", 1)).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Other);
    }

    #[test]
    fn test_ascii_decrement_not_found() {
        let mut cache = Cache::new();
        cache.r.get_mut().extend_from_slice(b"NOT_FOUND\r\n");
        let mut ascii = super::Protocol::new(&mut cache);
        let err = block_on(ascii.decrement("foo", 1)).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::NotFound);
    }

    #[test]
    fn test_ascii_decrement_client_error() {
        let mut cache = Cache::new();
        cache
            .r
            .get_mut()
            .extend_from_slice(b"CLIENT_ERROR cannot increment or decrement non-numeric value\r\n");
        let mut ascii = super::Protocol::new(&mut cache);
        let err = block_on(ascii.decrement("foo", 1)).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Other);
        assert!(err
            .to_string()
            .contains("cannot increment or decrement non-numeric value"));
    }

    #[test]
    fn test_ascii_gets_cas() {
        let mut cache = Cache::new();
        cache
            .r
            .get_mut()
            .extend_from_slice(b"VALUE foo 0 3 77\r\nbar\r\nEND\r\n");
        let mut ascii = super::Protocol::new(&mut cache);
        assert_eq!(
            block_on(ascii.gets_cas(&"foo")).unwrap(),
            (Vec::from(b"bar"), 77)
        );
        assert_eq!(cache.w.get_ref(), b"gets foo\r\n");
    }

    #[test]
    fn test_ascii_cas() {
        let (key, val, cas_id, ttl) = ("foo", "bar", 33, 5);
        let mut cache = Cache::new();
        cache
            .r
            .get_mut()
            .extend_from_slice(b"STORED\r\nbar\r\nEND\r\n");
        let mut ascii = super::Protocol::new(&mut cache);
        block_on(ascii.cas(&key, val.as_bytes(), cas_id, ttl)).unwrap();
        assert_eq!(
            cache.w.get_ref(),
            &format!(
                "cas {} 0 {} {} {}\r\n{}\r\n",
                key,
                ttl,
                val.len(),
                cas_id,
                val
            )
            .as_bytes()
            .to_vec()
        );
    }

    #[test]
    fn test_ascii_append_or_vivify() {
        let (key, val, ttl) = ("foo", "bar", 5);
        let mut cache = Cache::new();
        cache.r.get_mut().extend_from_slice(b"HD\r\nbar\r\nEND\r\n");
        let mut ascii = super::Protocol::new(&mut cache);
        block_on(ascii.append_or_vivify(&key, val.as_bytes(), ttl)).unwrap();
        assert_eq!(
            cache.w.get_ref(),
            &format!("ms {} {} MA N{}\r\n{}\r\n", key, val.len(), ttl, val)
                .as_bytes()
                .to_vec()
        );
    }

    #[test]
    fn test_response_starting_with_error() {
        let mut cache = Cache::new();
        cache.r.get_mut().extend_from_slice(b"ERROR\r\n");
        let mut ascii = super::Protocol::new(&mut cache);
        let err = block_on(ascii.get(&"foo")).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Other);
    }

    #[test]
    fn test_response_containing_error_string() {
        let mut cache = Cache::new();
        cache
            .r
            .get_mut()
            .extend_from_slice(b"VALUE contains_ERROR 0 3\r\nbar\r\nEND\r\n");
        let mut ascii = super::Protocol::new(&mut cache);
        assert_eq!(block_on(ascii.get(&"foo")).unwrap(), b"bar");
    }

    #[test]
    fn test_client_error_response() {
        let mut cache = Cache::new();
        cache
            .r
            .get_mut()
            .extend_from_slice(b"CLIENT_ERROR bad command\r\n");
        let mut ascii = super::Protocol::new(&mut cache);
        let err = block_on(ascii.get(&"foo")).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Other);
    }

    #[test]
    fn test_server_error_response() {
        let mut cache = Cache::new();
        cache
            .r
            .get_mut()
            .extend_from_slice(b"SERVER_ERROR out of memory\r\n");
        let mut ascii = super::Protocol::new(&mut cache);
        let err = block_on(ascii.get(&"foo")).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Other);
    }
}
