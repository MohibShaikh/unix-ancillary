# unix-ancillary

Safe, ergonomic Unix socket ancillary data (SCM_RIGHTS file descriptor passing) for Rust.

[![Crates.io](https://img.shields.io/crates/v/unix-ancillary.svg)](https://crates.io/crates/unix-ancillary)
[![Documentation](https://docs.rs/unix-ancillary/badge.svg)](https://docs.rs/unix-ancillary)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

## Features

- **Safe `OwnedFd`/`BorrowedFd` API** — no raw file descriptors in the public API
- **Automatic cleanup** — received FDs are `OwnedFd`, closed on drop
- **macOS fd-leak protection** — detects kernel-level truncation and closes leaked FDs
- **Ergonomic extension traits** — `send_fds()` / `recv_fds()` on `UnixStream` and `UnixDatagram`

## Quick Start

```rust
use std::os::unix::net::UnixStream;
use unix_ancillary::UnixStreamExt;

let (tx, rx) = UnixStream::pair().unwrap();

// Send a file descriptor
let file = std::fs::File::open("/dev/null").unwrap();
tx.send_fds(b"hello", &[&file]).unwrap();

// Receive it
let (bytes_read, buf, fds) = rx.recv_fds::<1>().unwrap();
assert_eq!(&buf[..bytes_read], b"hello");
assert_eq!(fds.len(), 1);
// fds[0] is an OwnedFd — automatically closed on drop
```

## Low-Level API

```rust
use unix_ancillary::{SocketAncillary, AncillaryData};
use std::io::IoSlice;
use std::os::unix::io::AsFd;

let mut buf = [0u8; SocketAncillary::buffer_size_for_rights(1)];
let mut ancillary = SocketAncillary::new(&mut buf);
ancillary.add_fds(&[file.as_fd()]).unwrap();
```

## Platform Support

- **Linux** — full support with `MSG_CMSG_CLOEXEC`
- **macOS** — supported with `fcntl` CLOEXEC fallback + fd-leak protection
- **FreeBSD, OpenBSD, NetBSD, DragonFly** — supported with `MSG_CMSG_CLOEXEC`

## License

MIT
