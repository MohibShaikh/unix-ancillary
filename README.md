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
- **Fuzz-hardened parser** — bounds-checked `cmsg_len` walk and defensive fd validation; tens of millions of fuzz executions clean
- **Ergonomic extension traits** — `send_fds()` / `recv_fds()` on `UnixStream` and `UnixDatagram`
- **Async support** — the same API on tokio sockets behind an optional `tokio` feature (off by default)

## When to reach for this

Use `unix-ancillary` when you're passing an open file descriptor between
processes over a Unix socket and you don't want to babysit its lifetime or
worry about it leaking. Common cases:

- **Privilege separation** — a privileged process binds a socket or opens a
  protected file and hands the descriptor to an unprivileged worker, so the
  worker never needs the privilege itself. (See `examples/privsep.rs`.)
- **Sandboxing / capability passing** — a supervisor opens exactly what a
  sandboxed task may touch and passes those fds in; the sandbox gets no
  `open()` rights of its own. A natural fit for confining agent/tool
  execution. (See `examples/agent_sandbox.rs`.)
- **Socket activation & graceful restart** — hand a live listening socket to a
  freshly-exec'd process without dropping connections.
- **Connection handoff** — a front-end accepts a connection and passes the
  accepted socket to a backend worker pool.

Received descriptors come back as `OwnedFd` (closed on drop) with
`FD_CLOEXEC` already set, so they can't outlive their scope or leak into a
child across `exec`. Works on blocking sockets out of the box, and on tokio
sockets with the `tokio` feature.

## Why this crate

Passing fds over Unix sockets is well-trodden ground. The distinction here is
that received descriptors are **owned and close-on-exec by default** — you
can't forget to close one, and they don't silently leak into child processes.

| | `unix-ancillary` | `sendfd` | `nix` / raw `libc` |
|---|:---:|:---:|:---:|
| Received fd type | **`OwnedFd`** | `RawFd` | `RawFd` |
| Auto-close on drop | **yes** | no (caller owns) | no |
| `FD_CLOEXEC` on received fds | **yes** (kernel or `fcntl`) | **no** | manual |
| Surplus fds on over-send | **wrapped + closed** | silently dropped | manual |
| Fuzz-hardened cmsg parser | **yes** | — | — |
| Blocking API | yes | yes | yes |
| Async (tokio) | **yes** (`tokio` feature) | yes | manual |
| Stable Rust | yes | yes | yes |

`sendfd` hands you `RawFd` integers with no `MSG_CMSG_CLOEXEC` — you own the
lifetimes and the received fds are inheritable across `exec` unless you set the
flag yourself. std's `SocketAncillary` is `OwnedFd`-based but nightly-only.
This crate is the stable, safe-by-default middle.

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

> **Stream semantics:** Unix streams do not preserve send-call boundaries. A
> receive call may return bytes or descriptors from multiple sends, or only
> part of one send. Use a framed protocol when descriptor-to-message
> association matters. Sending one or more descriptors over a stream requires
> at least one payload byte.

## Bring-your-own buffer

```rust
use std::os::unix::net::UnixStream;
use unix_ancillary::UnixStreamExt;

let (_tx, rx) = UnixStream::pair().unwrap();
let mut buf = [0u8; 256];
let (n, fds) = rx.recv_fds_into::<4>(&mut buf).unwrap();
```

## Async (tokio)

Enable the `tokio` feature — blocking users pull in no extra dependencies:

```toml
[dependencies]
unix-ancillary = { version = "0.2", features = ["tokio"] }
```

The async API mirrors the blocking one on `tokio::net::UnixStream` /
`UnixDatagram`, with identical leak-proof and CLOEXEC semantics:

```rust,ignore
use tokio::net::UnixStream;
use unix_ancillary::AsyncUnixStreamExt;

let (tx, rx) = UnixStream::pair()?;

let file = std::fs::File::open("/dev/null")?;
tx.send_fds(b"hello", &[&file]).await?;

let recv = rx.recv_fds::<1>().await?;
assert_eq!(recv.fds.len(), 1);
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

The high-level `recv_fds` sizes the receive cmsg buffer to a platform-specific
upper bound the kernel cannot exceed for a single `SCM_RIGHTS` message:

- **Linux / *BSD**: fixed `SCM_MAX_FD = 253`. The peer's kernel rejects
  oversized sends with `EINVAL` before they hit the wire.
- **macOS**: the receiver's current `RLIMIT_NOFILE`, queried per recv call.
  The kernel must allocate an fd table entry per delivered fd and physically
  cannot exceed that limit.

Result: truncation is kernel-impossible on every supported platform.

- Every fd the receiving kernel deposits is wrapped in `OwnedFd` immediately.
- Caller gets the first `N`; the rest drop and close on the spot. Zero leak.
- If `MSG_CTRUNC` somehow fires anyway, every extracted fd is closed and an
  error is returned — caller never sees partial state.

Low-level callers using `SocketAncillary` directly manage their own buffer
and must size it correctly; the `is_truncated()` flag is exposed for that
path.

## Hardening

The cmsg parser is fuzzed with `cargo-fuzz` against arbitrary byte input. Two
soundness fixes shipped in 0.2.2 from that effort:

- `Messages::next` validates `cmsg_len` fits in the remaining buffer before
  calling libc's `CMSG_NXTHDR` (which performs unchecked pointer arithmetic
  on that field). Malformed cmsgs terminate the walk cleanly.
- `ScmRights::next` skips negative fd values silently — kernels never deliver
  them via `SCM_RIGHTS`, but the parser is now defensive against any byte
  source.

Neither path is reachable from a real `recvmsg`; the hardening protects
against replay scenarios, shared-memory cmsg blobs, and similar non-kernel
input. To run the harness:

```bash
cargo install cargo-fuzz
cargo +nightly fuzz run parse_cmsg
```

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
