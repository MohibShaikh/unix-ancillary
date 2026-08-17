# Changelog

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
