# memcache-async

[![Build Status](https://travis-ci.org/vavrusa/memcache-async.svg?branch=master)](https://travis-ci.org/vavrusa/memcache-async)
[![Codecov Status](https://codecov.io/gh/vavrusa/memcache-async/branch/master/graph/badge.svg)](https://codecov.io/gh/vavrusa/memcache-async)
[![Crates.io](https://img.shields.io/crates/v/memcache-async.svg)](https://crates.io/crates/memcache-async)
[![MIT licensed](https://img.shields.io/badge/license-MIT-blue.svg)](./LICENSE)
[![Docs](https://docs.rs/memcache-async/badge.svg)](https://docs.rs/memcache-async/)

memcache-async is an async [memcached](https://memcached.org/) client implementation.

## Install

The crate is called `memcache-async` and you can depend on it via cargo:

```ini
[dependencies]
memcache-async = "0.2.0"
```

## Features

The crate implements the protocol on any stream implementing `AsyncRead + AsyncWrite`.

- [ ] Binary protocol
- [x] ASCII protocol
- [x] TCP connection
- [x] UDP connection
- [x] UNIX Domain socket connection
- [ ] Automatically compress
- [ ] Automatically serialize to JSON / msgpack etc.
- [ ] Typed interface
- [ ] Mutiple server support with custom key hash algorithm
- [ ] SASL authority (plain)

## Basic usage

The crate works with byte slices for values, the caller should implement deserialization if desired. See [examples](examples/) for usage. E.g. after start a memcached instance locally, the following example code could be run:

```bash
cargo run --example tcp-simple 127.0.0.1:11211
```

## License

MIT
