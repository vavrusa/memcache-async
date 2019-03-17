/// This is a simplified implementation of [rust-memcache](https://github.com/aisk/rust-memcache)
/// ported for AsyncRead + AsyncWrite.
use core::fmt::Display;
use std::io::{BufReader, Error, ErrorKind};
use tokio::await;
use tokio::io;
use tokio::prelude::*;

pub struct Protocol<S: AsyncRead + AsyncWrite> {
    io: S,
}

impl<S> Protocol<S>
where
    S: AsyncRead + AsyncWrite,
{
	/// Creates the ASCII protocol on a stream.
    pub fn new(io: S) -> Self {
        Self { io }
    }

    /// Returns the value for given key as bytes. If the value doesn't exist, `std::io::ErrorKind::NotFound` is returned.
    pub async fn get<'a, K: Display>(&'a mut self, key: &'a K) -> Result<Vec<u8>, Error> {
        // Send command
        let header = format!("get {}\r\n", key);
        await!(io::write_all(&mut self.io, header.as_bytes()))?;
        await!(io::flush(&mut self.io))?;

        // Read response header
        let mut reader = BufReader::new(&mut self.io);
        let header = {
            let (_, v) = await!(io::read_until(&mut reader, b'\n', Vec::default()))?;
            String::from_utf8(v).map_err(|_| Error::from(ErrorKind::InvalidInput))?
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
        drop(await!(io::read_exact(&mut reader, &mut buffer))?);

        // Read the trailing header
        drop(await!(io::read_until(&mut reader, b'\n', Vec::default()))?);
        drop(await!(io::read_until(&mut reader, b'\n', Vec::default()))?);

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
        await!(io::write_all(&mut self.io, header.as_bytes()))?;
        await!(io::write_all(&mut self.io, val))?;
        await!(io::write_all(&mut self.io, b"\r\n"))?;
        await!(io::flush(&mut self.io))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::await;
    use tokio::net::UnixStream;

    #[test]
    fn test_ascii() {
        tokio::run_async(
            async move {
                let sock = await!(UnixStream::connect("/tmp/memcached.sock")).expect("connected socket");
                let mut ascii = Protocol::new(sock);

                await!(ascii.set(&"ascii_foo", b"bar", 0)).expect("set works");
                let value = await!(ascii.get(&"ascii_foo")).expect("get works");
                assert_eq!(value, b"bar".to_vec());
            },
        );
    }

}
