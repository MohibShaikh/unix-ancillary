# unix-ancillary

Safe, ergonomic Unix socket ancillary data (SCM_RIGHTS file descriptor passing) for Rust.

[![Crates.io](https://img.shields.io/crates/v/unix-ancillary.svg)](https://crates.io/crates/unix-ancillary)
[![Documentation](https://docs.rs/unix-ancillary/badge.svg)](https://docs.rs/unix-ancillary)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

## Features

- **Safe `OwnedFd`/`BorrowedFd` API** — no raw file descriptors in the public API
- **Automatic cleanup** — received FDs are `OwnedFd`, closed on drop
- **No fd leaks on truncation** — the high-level API sizes the receive cmsg buffer past every Unix kernel's per-message fd cap. Surplus fds beyond the caller's `N` are auto-closed; truncation cannot leak fds into the process
- **CLOEXEC errors surfaced** — if `fcntl(FD_CLOEXEC)` fails on macOS, every received fd is closed and the error is returned
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
let recv = rx.recv_fds::<1>().unwrap();
assert_eq!(&recv.data[..], b"hello");
assert_eq!(recv.fds.len(), 1);
// recv.fds[0] is an OwnedFd — automatically closed on drop
```

## Bring-your-own buffer

```rust
use std::os::unix::net::UnixStream;
use unix_ancillary::UnixStreamExt;

let (_tx, rx) = UnixStream::pair().unwrap();
let mut buf = [0u8; 256];
let (n, fds) = rx.recv_fds_into::<4>(&mut buf).unwrap();
```

## Low-Level API

```rust
use unix_ancillary::{SocketAncillary, AncillaryData};
use std::io::IoSlice;
use std::os::unix::io::AsFd;

let file = std::fs::File::open("/dev/null").unwrap();
let mut buf = vec![0u8; SocketAncillary::buffer_size_for_rights(1)];
let mut ancillary = SocketAncillary::new(&mut buf);
ancillary.add_fds(&[file.as_fd()]).unwrap();
```

## How fd-leak protection works

Linux hard-codes `SCM_MAX_FD = 253` per `SCM_RIGHTS` message; other Unix
kernels enforce comparable per-message caps. The high-level `recv_fds`
allocates a cmsg buffer sized for that cap regardless of the caller's `N`.
Result:

- A peer cannot send more fds than our buffer can hold — their kernel rejects
  the `sendmsg` first.
- Every fd the receiving kernel deposits is wrapped in `OwnedFd` immediately.
- Caller gets the first `N`; the rest drop and close on the spot. Zero leak.
- If `MSG_CTRUNC` somehow fires anyway, every extracted fd is closed and an
  error is returned — caller never sees partial state.

Low-level callers using `SocketAncillary` directly manage their own buffer
and must size it correctly; the `is_truncated()` flag is exposed for that
path.

## CLOEXEC race on macOS

macOS lacks `MSG_CMSG_CLOEXEC`. This crate sets `FD_CLOEXEC` via `fcntl`
immediately after `recvmsg` returns, but a concurrent `fork`+`exec` between
the two calls can leak the fd into the child. If your workload forks
concurrently with fd-receiving threads, hold a fork lock around the receive.

## Platform Support

- **Linux** — full support with `MSG_CMSG_CLOEXEC`
- **macOS** — supported with `fcntl` CLOEXEC fallback (see caveat above)
- **FreeBSD, OpenBSD, NetBSD, DragonFly** — supported with `MSG_CMSG_CLOEXEC`

## License

MIT
