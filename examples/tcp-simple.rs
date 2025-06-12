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

    block_on(async move {
        let (key, val) = ("foo", "bar");
        let stream = TcpStream::connect(addr).expect("Failed to create stream");

        // "futures::io::AllowStdIo" is used here to make the stream
        // work with AsyncIO. This shouldn't be used in production
        // since it will block current thread. Use something like
        // romio or tokio instead.
        let mut cache = Protocol::new(AllowStdIo::new(stream));
        cache
            .set(&key, val.as_bytes(), 0, None)
            .await
            .expect("Failed to set key");

        let v = cache.get(&key).await.expect("Failed to get key");
        assert_eq!(v, val.as_bytes());
    });
}
