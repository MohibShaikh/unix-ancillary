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
1.75. This is a transport primitive; it does not authenticate peers, frame
streams, spawn workers, or enforce a sandbox.

| Your integration | A reasonable starting point |
|---|---|
| Standard or Tokio Unix sockets; simple FD handoff | `unix-ancillary` |
| Already using `rustix`; need configurable ancillary buffers or syscall flags | [rustix ancillary buffers](https://docs.rs/rustix/latest/src/rustix/net/send_recv/msg.rs.html) |
| Need Unix socket addressing or additional socket types | [uds](https://github.com/tormol/uds) |
| Existing raw-FD sendfd integration | Consider the [migration guide](MIGRATION.md) if automatic ownership and error policies reduce your code |

`rustix` also supports owned received descriptors. An extra convenience dependency
may not help a project already using it. Std's ancillary API remains nightly-only
and its `ScmRights` iterator yields raw descriptors.

## Quick start

The working tree targets **0.6.0 (unreleased)**. Until publication, use a local
path dependency to evaluate these repairs; the registry's 0.5.0 has the safety
issues described in the [changelog](CHANGELOG.md).

```toml
[dependencies]
unix-ancillary = { path = "../unix-ancillary" }
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
  The allocating variant uses 4 KiB for streams and 64 KiB for datagrams.
- `recv_fds_exact` / `recv_fds_exact_into`: reject fewer or more than `N` descriptors
  in this receive, closing the entire batch on mismatch. This does not wait for
  later stream bytes or descriptors to complete a message.
- Convenience datagram receives reject payload truncation with `InvalidData`.
  All convenience receives reject ancillary truncation with `InvalidData`.
- Vectored methods are available on both blocking traits and the async stream
  trait. The async datagram trait currently exposes scalar operations.

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

## Low-level ancillary data

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
`set_passcred` / `passcred` over `AsFd`. Enable `SO_PASSCRED` before receiving;
reserve `buffer_size_for_rights(n) + buffer_size_for_credentials()` bytes for
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
  credentials. BSD uses a 253-descriptor budget. macOS derives an allocation
  budget from `RLIMIT_NOFILE`, capped at 1,048,576 descriptors. These budgets
  do not make truncation impossible; applications must handle receive errors.
- Every descriptor delivered in the control buffer is owned and cleaned up.
  Kernel handling of descriptors omitted by truncation is platform-specific;
  userspace cannot close descriptor numbers it was not given.
- CI executes tests on Linux and macOS. BSD documentation builds check
  compilation, not kernel behavior. Android uses the Linux credential path
  but is not an execution target in CI.

## Verification

The test suite covers partial sends, SIGPIPE with the default signal handler,
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
