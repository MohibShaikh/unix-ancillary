# Changelog

## [0.6.0] - Unreleased

### Safety fixes and migration

- Low-level send buffers retain descriptor borrows. Dropping the original file
  before the buffer is no longer accepted; stale fd-number reuse cannot change
  the resource sent. Send buffers never yield received descriptor owners.
- `SocketAncillary::messages` now takes `&mut self` and drains received messages
  once. Received descriptors are owned before receive returns. Unread
  descriptors close on buffer clear/reuse/drop and iterator drop, including
  early error paths. A failed receive clears the previous message state.
- Receive buffers reject send operations until cleared. Forward descriptors
  by draining them and borrowing them into a separate send buffer.
- Control headers are parsed and written without aligned references, so caller
  byte slices at arbitrary offsets are supported. The fuzz parser no longer
  constructs owning descriptors from arbitrary integers.
- Linux/Android convenience buffers include space for automatic credentials
  alongside 253 FDs. Ancillary truncation now returns `InvalidData`, closing
  the delivered batch. Other additional cmsgs can still cause truncation.

These are low-level source and behavior changes. High-level blocking/Tokio FD
operation signatures and Rust 1.75 MSRV are retained. See [MIGRATION.md](MIGRATION.md).

### Verification and adoption

- Regression coverage for descriptor ownership, early cleanup, offset buffers,
  lifetime rejection, and maximum-sized credential/FD receives.
- Restored the separate fuzz workspace and added its compile check to CI.
- Added a real parent/child read-only handoff example; fixed the capability
  example to send a read-only descriptor and verify that writes fail.
- CI runs the multiprocess and downstream call-shape examples and checks
  Tokio compilation on the MSRV.
- Corrected comparisons, platform/truncation claims, and the vectored API
  inventory: async datagram vectored methods are not implemented.

## [0.5.0] - 2026-09-03

### Breaking

- `AncillaryData` is now `#[non_exhaustive]`. A match on it needs a wildcard
  arm. This is the only break in the release, taken so that every future
  control-message kind is additive rather than another major. The credentials
  variant below is the first thing it paid for.

### Added

- `AncillaryData::Credentials`, carrying `SCM_CREDENTIALS` on Linux and
  Android. Descriptors and credentials come out of the same `recvmsg`, off the
  same cmsg buffer, which is why they live in one crate.
- `ScmCredentials` with `pid`, `uid`, `gid`, and `for_this_process()` for the
  values any unprivileged sender may claim. Claiming anything else fails the
  send with `EPERM`, documented on both constructors.
- `set_passcred` and `passcred`, free functions over `AsFd`. Credentials only
  arrive on a socket with `SO_PASSCRED` set, and it must be set before the
  message is received. Free functions rather than extension-trait methods
  because std has an unstable inherent `UnixStream::set_passcred`; a trait
  method of that name collides with it, and would silently lose to it if std
  stabilizes. Taking `AsFd` also covers tokio sockets and bare `OwnedFd`.
- `SocketAncillary::add_credentials` and `buffer_size_for_credentials`, so one
  message can carry descriptors and credentials together.
- `send_fds_vectored` and `recv_fds_vectored` on the blocking stream/datagram
  traits and the Tokio stream trait. (Corrected inventory: the original entry
  incorrectly claimed all four traits.) Descriptors ride the same `sendmsg` as the whole iovec,
  so a framed protocol sends header and body without copying them into one
  buffer first. The stream empty-payload rule is checked against the total
  length across `iov`, not per buffer.

  There is no `send_fds_vectored_all`. The vectored send is one `sendmsg`, so
  on a stream it can accept part of the payload while still delivering every
  descriptor. The docs say so at each call site.
- `examples/dep_api_check.rs`, which compiles the call shapes the one published
  downstream uses, so a future release cannot break it silently.

Not covered: BSD `SCM_CREDS`, which is sent implicitly rather than attached, so
modelling it as this variant would describe semantics that platform does not
have. `SO_PEERCRED` is also out of scope; it is a `getsockopt` answering "who
connected" once per connection, not a control message.

### Internal

- The scalar `send_fds` and `recv_fds_into` paths delegate to the vectored
  ones, leaving a single code path. Verified with `strace` against 0.4.0: the
  `msghdr` for a scalar send is byte-identical, `msg_iovlen=1`.
- `add_fds` and `add_credentials` share one `add_cmsg` writer, so both get the
  same buffer walk and bounds checks.

### Docs

- `[package.metadata.docs.rs] targets` now builds linux, both apple targets,
  freebsd and netbsd, so the macOS `RLIMIT_NOFILE` and `fcntl` CLOEXEC claims
  are backed by a published build.
- README and AGENTS.md rewritten to drop em dashes and title-case headings.

## [0.4.0] - 2026-08-17

### Behavior changes (breaking)

- Unix stream `send_fds` now rejects an empty payload when descriptors are
  present with `ErrorKind::InvalidInput`. Linux `SCM_RIGHTS` over a stream
  requires at least one normal byte.
- Datagram `recv_fds`, `recv_fds_into`, and their async equivalents now
  return `ErrorKind::InvalidData` when the payload is truncated by the
  receive buffer, instead of silently returning the truncated bytes.
- `RecvResult` is now `#[non_exhaustive]` and gains a `data_truncated`
  field. Struct literals and exhaustive destructuring no longer compile
  against it; construct it through `cmsg_recvmsg` instead.

### Added

- `UnixStreamExt::send_fds_all` and `AsyncUnixStreamExt::send_fds_all`:
  send descriptors exactly once on the initial send, then complete the
  payload with signal-safe ordinary sends.
- `UnixStreamExt::recv_fds_exact`, `recv_fds_exact_into`,
  `AsyncUnixStreamExt::recv_fds_exact`, `recv_fds_exact_into`, and their
  datagram analogues: strict descriptor-count receives that error on too
  few or too many descriptors.

### Hardening

- `sendmsg` uses `MSG_NOSIGNAL` on supported platforms (Linux, Android,
  FreeBSD, DragonFly, NetBSD, OpenBSD) and `SO_NOSIGPIPE` on Apple targets
  (`target_vendor = "apple"`), so a
  write to a closed peer returns `BrokenPipe` instead of terminating the
  process with `SIGPIPE`.
- `sendmsg` and `recvmsg` retry internally on `EINTR`, preserving
  `WouldBlock` for readiness adapters.

## [0.3.0]

- Async (tokio) fd passing on `UnixStream` and `UnixDatagram` behind the
  optional `tokio` feature.
- `recv_fds_into` for caller-supplied data buffers.
- Leak-count regression test and `fcntl`-path tests.

## [0.2.2]

- Parser hardening surfaced by fuzz testing: bounded `cmsg_len` walks and
  defensive fd validation.

## [0.2.1]

- Bug fixes.

## [0.2.0]

- Initial feature-complete release.
