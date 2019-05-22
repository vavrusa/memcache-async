#![feature(async_await)]

use futures::executor::block_on;
use futures::io::AllowStdIo;
use memcache_async::ascii::Protocol;
use std::env;
use std::net::TcpStream;

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("{} <addr>", args[0]);
        return;
    }

    let addr = &args[1];

    let f = async || {
        let (key, val) = ("foo", "bar");

        let stream = TcpStream::connect(addr).expect("Failed to create stream");

        let mut cache = Protocol::new(AllowStdIo::new(stream));
        cache
            .set(&key, val.as_bytes(), 0)
            .await
            .expect("Failed to set key");

        let v = cache.get(&key).await.expect("Failed to get key");
        assert_eq!(v, val.as_bytes());
    };

    block_on(f());
}
