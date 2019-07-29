#![feature(async_await)]
use futures::compat::{Compat01As03, Future01CompatExt};
use futures::future::{FutureExt, TryFutureExt};
use memcache_async::ascii::Protocol;
use std::env;
use std::net::SocketAddr;
use tokio::net::TcpStream;

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("{} <addr>", args[0]);
        return;
    }

    let addr = args[1].parse::<SocketAddr>().unwrap();

    let f = async move {
        let (key, val) = ("foo", "bar");
        let stream = TcpStream::connect(&addr).compat().await.unwrap();

        let mut cache = Protocol::new(Compat01As03::new(stream));
        cache.set(&key, val.as_bytes(), 0).await.unwrap();

        let v = cache.get(&key).await.unwrap();
        assert_eq!(v, val.as_bytes());
        Ok(())
    };

    tokio::run(f.boxed().compat());
}
