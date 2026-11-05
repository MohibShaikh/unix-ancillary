# unix-ancillary

Safe file-descriptor passing on standard and Tokio Unix sockets, on stable Rust.

[![Crates.io](https://img.shields.io/crates/v/unix-ancillary.svg)](https://crates.io/crates/unix-ancillary)
[![Documentation](https://docs.rs/unix-ancillary/badge.svg)](https://docs.rs/unix-ancillary)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

## When to use it

Use this crate for small FD-passing integrations: hand a connected socket to a
worker, pass an already-open file to another process, or transfer a listener
for graceful restart. Extension traits supply owned receives, exact descriptor
counts, complete stream sends, and explicit truncation errors.

The default dependency is `libc`. Tokio support is optional. The MSRV is Rust
1.75. New tokio releases can need a newer compiler (tokio 1.54.1 needs 1.85), so
on Rust 1.75 the `tokio` feature needs a pin:
`cargo update -p tokio --precise 1.50.0`. This is a transport primitive; it does not authenticate peers, frame
streams, spawn workers, or enforce a sandbox.

| Your integration | A reasonable starting point |
|---|---|
| Standard or Tokio Unix sockets; simple FD handoff | `unix-ancillary` |
| Already using `rustix`; need configurable ancillary buffers or syscall flags | [rustix ancillary buffers](https://docs.rs/rustix/latest/src/rustix/net/send_recv/msg.rs.html) |
| Need Unix socket addressing | [uds](https://github.com/tormol/uds), which is [no longer maintained](https://github.com/tormol/uds/issues/29) |
| Existing raw-FD sendfd integration | Consider the [migration guide](MIGRATION.md) if automatic ownership and error policies reduce your code |

`rustix` also supports owned received descriptors. An extra convenience dependency
may not help a project already using it. Std's ancillary API remains nightly-only
and its `ScmRights` iterator yields raw descriptors.

## Quick start

Versions before 0.6.0 have low-level safety issues described in the
[changelog](CHANGELOG.md). See [MIGRATION.md](MIGRATION.md) when upgrading.

```toml
[dependencies]
unix-ancillary = "0.6"
# Add features = ["tokio"] for the async extension traits.
```

```rust
use std::os::unix::net::UnixStream;
use unix_ancillary::UnixStreamExt;

let (tx, rx) = UnixStream::pair().unwrap();
let file = std::fs::File::open("/dev/null").unwrap();
tx.send_fds_all(b"F", &[&file]).unwrap();

// Dedicated channel: one marker byte and exactly one descriptor.
let mut marker = [0u8; 1];
let (n, fds) = rx.recv_fds_exact_into::<1>(&mut marker).unwrap();
assert_eq!(n, 1);
assert_eq!(&marker, b"F");
assert_eq!(fds.len(), 1);
// Each received OwnedFd closes on drop. The original file remains owned here.
```

Run a real parent/child exchange:

```bash
cargo run --example multiprocess
```

The child receives a read-only file descriptor over a private Unix socket and
checks that writes fail. Only the socket path is passed as an argument. Both
processes retain their normal OS permissions; this example does not sandbox them.

## Choosing an operation

- `send_fds` / `send_fds_vectored`: one successful `sendmsg`, returning accepted
  payload bytes. On a stream, descriptors may be delivered with only a prefix
  of the payload. Do not resend them when completing the remaining bytes.
- `send_fds_all`: attach descriptors once and finish the payload. An error or
  async cancellation can leave a prefix delivered; discard or otherwise recover
  the connection through your protocol. Concurrent writers must serialize whole
  protocol operations if interleaving would be invalid.
- `recv_fds` / `recv_fds_into`: receive up to `N` descriptors and close surplus.
  `recv_fds` reads up to 4 KiB on streams and 64 KiB on datagrams, and reports how
  many it closed in `surplus_closed`. `recv_fds_into` does not report that count; use
  `recv_fds_slots` if you need it with your own buffer.
- `recv_fds_exact` / `recv_fds_exact_into`: reject fewer or more than `N` descriptors
  in this receive, closing the entire batch and discarding its bytes on mismatch.
  This does not wait for later stream bytes or descriptors to complete a message.
  With `N > 0` and room in the data buffer, a closed peer is `UnexpectedEof`.
- Convenience datagram receives reject payload truncation with `InvalidData`.
  All convenience receives reject ancillary truncation with `InvalidData`.
- Vectored methods are available on both blocking traits, the async stream trait
  and `seqpacket`. For a vectored receive on a tokio datagram socket, call
  `seqpacket::recv_fds_vectored` inside `async_io`.

Unix streams do not preserve send-call boundaries. A receive may deliver a
prefix of a send or data from multiple sends. Use a protocol with explicit
framing and descriptor association, or a datagram transport when appropriate.
Stream sends with descriptors require at least one ordinary payload byte.

## Tokio

Enable `features = ["tokio"]`, then import `AsyncUnixStreamExt` or
`AsyncUnixDatagramExt`. Their scalar operations mirror the blocking API:

```rust,ignore
use tokio::net::UnixStream;
use unix_ancillary::AsyncUnixStreamExt;

let (tx, rx) = UnixStream::pair()?;
let file = std::fs::File::open("/dev/null")?;
tx.send_fds_all(b"F", &[&file]).await?;
let mut marker = [0u8; 1];
let (n, fds) = rx.recv_fds_exact_into::<1>(&mut marker).await?;
assert_eq!(n, 1);
assert_eq!(fds.len(), 1);
```

## Seqpacket

`unix_ancillary::seqpacket` has the same operations as free functions over any
connected `SOCK_SEQPACKET` socket that is `AsFd`. `seqpacket::tokio` has async forms
over `AsyncFd`, and `seqpacket::pair` makes a connected pair. Messages keep their
boundaries, so an empty payload can carry descriptors, and a payload too long for
the buffer is `InvalidData`. A closed peer looks like an empty message, so
`recv_fds_exact` reports it as `InvalidData`, not `UnexpectedEof`. Tests run on Linux.
macOS has no `AF_UNIX` `SOCK_SEQPACKET`.

## Sockets that are not connected

`send_fds_to` takes a destination, and `recv_fds_from` also returns who sent the
message, or `None` for a sender that never called `bind`:

```rust,ignore
use std::os::unix::net::{SocketAddr, UnixDatagram};
use unix_ancillary::{recv_fds_from, send_fds_to};

let sock = UnixDatagram::unbound()?;
let service = SocketAddr::from_pathname("/run/service.sock")?;
send_fds_to(&sock, &service, b"hello", &[&file])?;

let (mut data, mut slots) = ([0u8; 64], [None]);
let (got, sender) = recv_fds_from(&server, &mut data, &mut slots)?;
```

Paths work everywhere, and abstract names work on Linux and Android. Both functions
take anything that is `AsFd`, so they run on a tokio socket inside `async_io`.

## Low-level ancillary data

`cmsg_sendmsg` does not apply the stream payload rule. On a stream, an empty
payload returns `Ok(0)` and the kernel delivers no descriptors.

`cmsg_sendmsg_with_flags` takes `MSG_DONTWAIT` and `MSG_EOR`, and
`cmsg_recvmsg_with_flags` takes `MSG_DONTWAIT` and `MSG_WAITALL`. Anything else is
`InvalidInput`, and the crate's own safety flags stay on.

`cmsg_recvmsg_peek` (Linux/Android) reads a message without consuming it. Those
kernels install fresh duplicates on each peek, and each copy is yours to close.

`SocketAncillary` accepts caller-provided byte storage at any alignment.
`add_fds` retains the lifetime of the borrowed descriptors. `cmsg_recvmsg`
owns received descriptors before returning, including when it reports
truncation through `RecvResult`.

Call `messages(&mut self)` to drain the received messages. Dropping a buffer,
clearing it, or starting another receive closes unread descriptors. Dropping a
message or rights iterator closes its unconsumed descriptors. Send buffers
produce no received messages. After a receive, call `clear` before building a
send; use a separate send buffer to forward received descriptors.

Linux/Android additionally expose `ScmCredentials`, `add_credentials`, and
`set_passcred` / `passcred` over `AsFd`. Enable `SO_PASSCRED` before the peer
sends. On a message already queued, Linux reports pid 0 and the overflow uid and
gid instead of the sender. Reserve `buffer_size_for_rights(n) + buffer_size_for_credentials()` bytes for
combined low-level messages. Credentials and descriptors arrive from the same
receive. High-level FD-only methods discard credentials; use the low-level API
when your protocol validates them. See [migration examples](MIGRATION.md).

## Safety and platform limits

- Received descriptors have `FD_CLOEXEC` set. Linux, Android and the supported
  BSD targets use `MSG_CMSG_CLOEXEC` atomically. macOS uses `fcntl` afterward.
- **macOS fork race:** a concurrent `fork` + `exec` between receive and `fcntl`
  can inherit a descriptor. Applications requiring that protection must
  coordinate both process creation and receiving with the same lock.
- Sends suppress SIGPIPE on Linux/Android/BSD with `MSG_NOSIGNAL` and on Apple
  with `SO_NOSIGPIPE`. Other Unix targets use a fallback without this guarantee.
- Linux/Android convenience receives reserve space for 253 descriptors plus
  credentials. macOS reserves 254, the most XNU accepts in one message. Elsewhere
  the budget is what you asked for, from 253 to 1000. These budgets do not make
  truncation impossible; applications must handle receive errors.
- Every descriptor delivered in the control buffer is owned and cleaned up.
  Kernel handling of descriptors omitted by truncation is platform-specific;
  userspace cannot close descriptor numbers it was not given.
- A send to a closed peer returns `BrokenPipe`. On Apple this comes from mapping
  the `EINVAL` that `SO_NOSIGPIPE` returns on such a socket.
- CI executes tests on Linux and macOS, and compiles FreeBSD and Android without
  running them. Nothing in CI checks kernel behavior on a BSD or on Android.
  CI also compiles NetBSD and illumos. OpenBSD and DragonFly are not compiled in CI.

## Cost

A send of up to about 1,000 descriptors allocates nothing, and more than that
uses the heap for its control message, though no kernel this crate tests accepts that
many. A receive allocates its returned `Vec` when descriptors
arrive and nothing when none do. `recv_fds` also allocates a `Vec` the size of the
message, and keeps one cleared receive buffer per thread: 4 KiB for streams, 64 KiB
for datagrams and seqpacket. `recv_fds_slots` never allocates: it fills a slice you
own, and it works on tokio sockets inside `async_io`. Receive control storage stays on the stack. macOS also pays one `setsockopt` per send and one `fcntl` per received
descriptor.
`tests/allocations.rs` checks these counts on Linux.

## Verification

The test suite covers partial sends, ancillary truncation, SIGPIPE with the default signal handler,
strict counts, payload truncation, ownership cleanup, repeated drains, arbitrary
buffer alignment, and combined credentials at Linux's FD limit. A compile-fail
doctest checks that a send buffer cannot outlive its descriptors. CI also runs
the parent/child example and a downstream API compatibility example.

The separate fuzz workspace shares the receive byte parser. Arbitrary input
never manufactures `OwnedFd`. CI compiles it; run an instrumented campaign with:

```bash
cargo install cargo-fuzz
cargo +nightly fuzz run parse_cmsg
```

See [CHANGELOG.md](CHANGELOG.md) and [MIGRATION.md](MIGRATION.md) before upgrading.

## License

MIT
