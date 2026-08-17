# Core Correctness Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make empty payloads, stream boundaries, SIGPIPE, EINTR, partial sends, datagram truncation, and exact descriptor counts explicit and safe in blocking and Tokio public APIs.

**Architecture:** Keep syscalls and flags in `cmsg.rs` and `platform.rs`, with `ext.rs` and `asyncio.rs` providing concrete socket ergonomics. Additive public APIs delegate to shared internal operations so blocking and Tokio behavior cannot drift.

**Tech Stack:** Rust 2021, libc, optional Tokio, integration tests, Docker-based stable and MSRV verification.

**Spec:** `docs/superpowers/specs/2026-08-17-production-capability-channel-design.md`

## Global Constraints

- Branch is `feat/production-capability-channel`.
- Default dependencies remain `libc` only.
- Preserve Rust 1.75 MSRV.
- Existing `send_fds`, `recv_fds`, and `recv_fds_into` signatures remain available.
- Safe public APIs expose no `RawFd`.
- Every error path closes all received descriptors.
- Update `AGENTS.md` after every failing test, passing test, and commit.

---

### Task 1: Document and enforce stream payload semantics

**Files:**
- Modify: `src/ext.rs`
- Modify: `src/asyncio.rs`
- Modify: `src/lib.rs`
- Modify: `README.md`
- Test: `tests/fd_passing.rs`
- Test: `tests/async_fd.rs`

**Interfaces:**
- Consumes: existing `UnixStreamExt::send_fds` and `AsyncUnixStreamExt::send_fds`.
- Produces: both stream APIs return `io::ErrorKind::InvalidInput` when `fds` is non-empty and `data` is empty. Datagram APIs retain their existing behavior.

- [ ] **Step 1: Add failing blocking tests**

Add to `tests/fd_passing.rs`:

```rust
#[test]
fn stream_rejects_fds_without_payload_byte() {
    let (tx, _rx) = UnixStream::pair().unwrap();
    let file = tempfile::tempfile().unwrap();

    let err = tx.send_fds(b"", &[&file]).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
}

#[test]
fn stream_allows_empty_payload_without_fds() {
    let (tx, _rx) = UnixStream::pair().unwrap();
    let empty: &[std::fs::File] = &[];
    assert_eq!(tx.send_fds(b"", empty).unwrap(), 0);
}
```

- [ ] **Step 2: Add the failing Tokio test**

Add to `tests/async_fd.rs`:

```rust
#[tokio::test]
async fn async_stream_rejects_fds_without_payload_byte() {
    let (tx, _rx) = UnixStream::pair().unwrap();
    let file = tempfile::tempfile().unwrap();

    let err = tx.send_fds(b"", &[&file]).await.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
}
```

- [ ] **Step 3: Run the targeted tests and verify failure**

Run in Docker:

```bash
cargo test --test fd_passing stream_rejects_fds_without_payload_byte
cargo test --all-features --test async_fd async_stream_rejects_fds_without_payload_byte
```

Expected: both tests fail because the current stream APIs call `sendmsg` without validating the payload.

- [ ] **Step 4: Add shared validation**

Add to `src/ext.rs`:

```rust
pub(crate) fn validate_stream_send(data: &[u8], fd_count: usize) -> io::Result<()> {
    if data.is_empty() && fd_count != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "SCM_RIGHTS over a Unix stream requires at least one payload byte",
        ));
    }
    Ok(())
}
```

Call it before constructing borrowed descriptors in both blocking and Tokio stream `send_fds` implementations. Do not call it from datagram implementations.

- [ ] **Step 5: Document stream behavior**

Document on both stream traits:

```rust
/// Unix streams do not preserve send-call boundaries. A receive call may
/// return bytes or descriptors from multiple sends, or only part of one send.
/// Use a framed protocol when descriptor-to-message association matters.
///
/// Sending one or more descriptors requires at least one payload byte.
```

Add the same warning near the README quick start and crate-level quick start.

- [ ] **Step 6: Run focused and full tests**

Run:

```bash
cargo test --test fd_passing
cargo test --all-features --test async_fd
```

Expected: all tests pass.

- [ ] **Step 7: Update `AGENTS.md` and commit**

```bash
git add AGENTS.md README.md src/ext.rs src/asyncio.rs src/lib.rs tests/fd_passing.rs tests/async_fd.rs
git commit -m "fix: enforce Unix stream fd payload contract"
```

---

### Task 2: Add SIGPIPE-safe and EINTR-resilient syscalls

**Files:**
- Modify: `src/platform.rs`
- Modify: `src/cmsg.rs`
- Test: `tests/fd_passing.rs`

**Interfaces:**
- Consumes: `sendmsg_vectored` and `recvmsg_vectored`.
- Produces: sends suppress `SIGPIPE` where supported, Apple sockets enable `SO_NOSIGPIPE`, and interrupted syscalls retry internally.

- [ ] **Step 1: Add a disconnected-peer subprocess regression test**

Add a helper test executable branch inside `tests/fd_passing.rs` driven by an environment variable.

The `SIG_DFL` reset is load-bearing. Rust's runtime sets `SIGPIPE` to `SIG_IGN` during startup, test binaries included, so an unprotected `sendmsg` already returns `EPIPE` instead of killing the process. Without restoring the default disposition this test passes identically before and after the fix and proves nothing.

`libc` is a normal dependency and is already usable from integration tests (see `tests/fd_passing.rs:57`).

```rust
#[test]
fn send_to_closed_peer_returns_error_without_sigpipe() {
    if std::env::var_os("UNIX_ANCILLARY_SIGPIPE_CHILD").is_some() {
        // Undo the runtime's SIG_IGN so an unprotected send really dies.
        // SAFETY: SIG_DFL is a valid disposition for SIGPIPE.
        unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };

        let (tx, rx) = UnixStream::pair().unwrap();
        drop(rx);
        let file = tempfile::tempfile().unwrap();
        let err = tx.send_fds(b"x", &[&file]).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::BrokenPipe);
        return;
    }

    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("send_to_closed_peer_returns_error_without_sigpipe")
        .arg("--nocapture")
        .env("UNIX_ANCILLARY_SIGPIPE_CHILD", "1")
        .status()
        .unwrap();

    assert!(status.success(), "child terminated with {status}");
}
```

- [ ] **Step 2: Run the regression test**

Run:

```bash
cargo test --test fd_passing send_to_closed_peer_returns_error_without_sigpipe -- --nocapture
```

Expected: the child dies on signal 13 and the parent's `status.success()` assertion fails. That is the red state, and it is only reachable because of the `SIG_DFL` reset in Step 1.

- [ ] **Step 3: Add platform send preparation**

Both items go inside the `mod inner` blocks at `src/platform.rs:162` and `src/platform.rs:194`, not at file scope, so the existing `pub(crate) use inner::*;` keeps exporting them:

```rust
pub(crate) const SEND_FLAGS: libc::c_int = libc::MSG_NOSIGNAL;

pub(crate) fn prepare_send(_fd: RawFd) -> io::Result<()> {
    Ok(())
}
```

Do not reuse the existing `cfg` list verbatim. It encodes `MSG_CMSG_CLOEXEC` support, which is a different platform fact that merely happens to cover the same six targets today. Write a separate list for `MSG_NOSIGNAL` and confirm the constant exists for each target in the `libc` version pinned in `Cargo.lock` before adding it.

Add an explicit Apple branch: `SEND_FLAGS = 0` plus `prepare_send` calling `setsockopt(SOL_SOCKET, SO_NOSIGPIPE, 1)`. This costs one extra syscall per send because the extension traits hold no per-socket state. Accept that for now and record it; `FdChannel` owns its socket and can hoist it to construction later.

Unknown Unix targets fall back to `SEND_FLAGS = 0` with a documented caveat in the platform support notes. Do not `compile_error!` on them. Today every Unix target builds through the fallback path at `src/platform.rs:186`; a hard failure would newly break illumos, Solaris, and AIX for a signal caveat rather than a soundness bug.

- [ ] **Step 4: Retry interrupted syscalls**

Replace the single send call in `src/cmsg.rs` with:

```rust
loop {
    let ret = unsafe { libc::sendmsg(fd.as_raw_fd(), &msg, platform::SEND_FLAGS) };
    if ret >= 0 {
        return Ok(ret as usize);
    }
    let err = io::Error::last_os_error();
    if err.kind() != io::ErrorKind::Interrupted {
        return Err(err);
    }
}
```

Call `platform::prepare_send(fd.as_raw_fd())?` before the loop. Apply the same `Interrupted` loop to `recvmsg`, preserving `WouldBlock` unchanged.

- [ ] **Step 5: Run regression and full tests**

Run:

```bash
cargo test --test fd_passing send_to_closed_peer_returns_error_without_sigpipe -- --nocapture
cargo test --all-features
```

Expected: child exits successfully after observing an I/O error, and the full suite passes.

- [ ] **Step 6: Update `AGENTS.md` and commit**

```bash
git add AGENTS.md src/platform.rs src/cmsg.rs tests/fd_passing.rs
git commit -m "fix: make ancillary sends signal-safe and EINTR-resilient"
```

---

### Task 3: Add descriptor-once complete stream sends

**Files:**
- Modify: `src/cmsg.rs`
- Modify: `src/ext.rs`
- Modify: `src/asyncio.rs`
- Test: `tests/fd_passing.rs`
- Test: `tests/async_fd.rs`

**Interfaces:**
- Consumes: `send_fds_impl`, shared send flags, and stream payload validation.
- Produces: `UnixStreamExt::send_fds_all` and `AsyncUnixStreamExt::send_fds_all`, both returning `io::Result<()>` and attaching descriptors only to the initial successful send.

- [ ] **Step 1: Add blocking round-trip test**

```rust
#[test]
fn send_fds_all_transfers_payload_and_descriptor() {
    let (tx, rx) = UnixStream::pair().unwrap();
    let file = tempfile::tempfile().unwrap();
    let payload = vec![b'x'; 128 * 1024];

    let sender = std::thread::spawn(move || tx.send_fds_all(&payload, &[&file]).unwrap());

    let first = rx.recv_fds::<1>().unwrap();
    assert_eq!(first.fds.len(), 1);
    let mut received = first.data;
    while received.len() < 128 * 1024 {
        let mut buf = [0u8; 8192];
        let n = std::io::Read::read(&mut &rx, &mut buf).unwrap();
        assert_ne!(n, 0);
        received.extend_from_slice(&buf[..n]);
    }
    sender.join().unwrap();
    assert_eq!(received, vec![b'x'; 128 * 1024]);
}
```

- [ ] **Step 2: Add Tokio round-trip test**

Add to `tests/async_fd.rs` and import `tokio::io::AsyncReadExt`:

```rust
#[tokio::test]
async fn async_send_fds_all_transfers_payload_and_descriptor() {
    let (tx, mut rx) = UnixStream::pair().unwrap();
    let file = tempfile::tempfile().unwrap();
    let expected = vec![b'x'; 128 * 1024];
    let payload = expected.clone();

    let sender = tokio::spawn(async move {
        tx.send_fds_all(&payload, &[&file]).await.unwrap();
    });

    let first = rx.recv_fds::<1>().await.unwrap();
    assert_eq!(first.fds.len(), 1);
    let mut received = first.data;
    while received.len() < expected.len() {
        let mut buf = [0u8; 8192];
        let n = rx.read(&mut buf).await.unwrap();
        assert_ne!(n, 0);
        received.extend_from_slice(&buf[..n]);
    }

    sender.await.unwrap();
    assert_eq!(received, expected);
}
```

- [ ] **Step 3: Run targeted tests and verify compile failure**

Expected: methods do not exist.

- [ ] **Step 4: Add ordinary-byte send helper**

In `src/cmsg.rs`, add a helper that sends bytes without ancillary data through `sendmsg_vectored`, using the same signal-safe flags and EINTR behavior:

```rust
pub(crate) fn send_bytes(fd: BorrowedFd<'_>, data: &[u8]) -> io::Result<usize> {
    let iov = [io::IoSlice::new(data)];
    sendmsg_vectored(fd, &iov, &[], 0)
}
```

- [ ] **Step 5: Implement blocking `send_fds_all`**

Add to `UnixStreamExt`:

```rust
fn send_fds_all(&self, data: &[u8], fds: &[impl AsFd]) -> io::Result<()>;
```

Implementation:

```rust
validate_stream_send(data, fds.len())?;
let borrowed: Vec<_> = fds.iter().map(|fd| fd.as_fd()).collect();
let mut sent = if borrowed.is_empty() {
    0
} else {
    let first = send_fds_impl(self.as_fd(), data, &borrowed)?;
    if first == 0 {
        return Err(io::Error::new(
            io::ErrorKind::WriteZero,
            "failed to send fd payload byte",
        ));
    }
    first
};
while sent < data.len() {
    let n = cmsg::send_bytes(self.as_fd(), &data[sent..])?;
    if n == 0 {
        return Err(io::Error::new(io::ErrorKind::WriteZero, "short write"));
    }
    sent += n;
}
Ok(())
```

Finish the payload with the `send_bytes` helper from Step 4, not `Write::write_all`. `write_all` calls `write(2)`, which carries no `MSG_NOSIGNAL`, so it would leave the descriptor-bearing head signal-safe and the ordinary tail unprotected on exactly the process that restored the default `SIGPIPE` handler.

Two things the obvious spelling gets wrong: `(&self).write_all(..)` does not compile because `Write` needs a mutable place, and returning early on `fds.is_empty()` skips the same retry loop the descriptor path needs.

- [ ] **Step 6: Implement Tokio `send_fds_all`**

Add the async method. Perform the initial ancillary send through `async_io`. Then loop over the remaining bytes with `self.writable().await?` and an `async_io(Interest::WRITABLE, || cmsg::send_bytes(..))` call, retrying `WouldBlock`, and return `WriteZero` for zero progress.

Do not use `try_write` for the tail. It is `write(2)` and reintroduces the unprotected-`SIGPIPE` gap that Step 5 closes on the blocking path.

Document that cancellation after the initial send may transfer descriptors and a payload prefix. `FdChannel` will later own resumable frame state.

- [ ] **Step 7: Run targeted and full tests**

```bash
cargo test --test fd_passing send_fds_all_transfers_payload_and_descriptor
cargo test --all-features --test async_fd async_send_fds_all_transfers_payload_and_descriptor
cargo test --all-features
```

Expected: all pass with exactly one descriptor delivered.

- [ ] **Step 8: Update `AGENTS.md` and commit**

```bash
git add AGENTS.md src/cmsg.rs src/ext.rs src/asyncio.rs tests/fd_passing.rs tests/async_fd.rs
git commit -m "feat: add descriptor-once complete stream sends"
```

---

### Task 4: Surface datagram payload truncation

**Files:**
- Modify: `src/cmsg.rs`
- Modify: `src/lib.rs`
- Modify: `src/ext.rs`
- Test: `tests/fd_passing.rs`
- Test: `tests/async_fd.rs`

**Interfaces:**
- Consumes: `recvmsg_vectored` and `ReceivedFds`.
- Produces: internal separate ancillary and data truncation flags, plus `ReceivedFds::data_truncated`. Convenience datagram methods return `InvalidData` on payload truncation.

- [ ] **Step 1: Add failing datagram truncation test**

`tests/fd_passing.rs:1-4` imports only `UnixStream` and `UnixStreamExt`. Add `std::os::unix::net::UnixDatagram` and `unix_ancillary::UnixDatagramExt` before pasting this and the Task 5 datagram cases.

```rust
#[test]
fn datagram_payload_truncation_is_an_error() {
    let (tx, rx) = UnixDatagram::pair().unwrap();
    tx.send(b"payload-larger-than-buffer").unwrap();
    let mut small = [0u8; 4];

    let err = rx.recv_fds_into::<0>(&mut small).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
}
```

Add the Tokio regression to `tests/async_fd.rs`:

```rust
#[tokio::test]
async fn async_datagram_payload_truncation_is_an_error() {
    let (tx, rx) = UnixDatagram::pair().unwrap();
    tx.send(b"payload-larger-than-buffer").await.unwrap();
    let mut small = [0u8; 4];

    let err = rx.recv_fds_into::<0>(&mut small).await.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
}
```

- [ ] **Step 2: Run and verify failure**

Expected: current API returns success because it checks only `MSG_CTRUNC`.

- [ ] **Step 3: Split internal receive flags**

Change `RecvMsgResult` to:

```rust
pub(crate) struct RecvMsgResult {
    pub bytes_read: usize,
    pub ancillary_len: usize,
    pub ancillary_truncated: bool,
    pub data_truncated: bool,
}
```

Set flags from `MSG_CTRUNC` and `MSG_TRUNC` separately.

- [ ] **Step 4: Propagate public metadata safely**

Keep public `RecvResult::truncated` as ancillary truncation for compatibility and add:

```rust
#[non_exhaustive]
pub struct RecvResult {
    pub bytes_read: usize,
    pub truncated: bool,
    pub data_truncated: bool,
}
```

This is a source break, not an additive change, and the plan's global constraints do not cover it. `RecvResult` at `src/lib.rs:89` is a plain public struct: adding a field breaks downstream struct literals and exhaustive destructuring, and adding `#[non_exhaustive]` breaks them again. Take it deliberately:

- Bump the crate to `0.4.0` in this phase rather than at release time.
- Record it in `CHANGELOG.md` (created in Task 6, not deferred to Phase 5).
- Note the exception in `AGENTS.md` against the "existing signatures remain available" constraint, which covers method signatures, not struct shape.

Update crate docs and tests that construct or inspect it.

Add `pub data_truncated: bool` to the already `#[non_exhaustive]` `ReceivedFds`.

- [ ] **Step 5: Make convenience datagram receives strict**

Pass a socket-kind parameter into the shared receive implementation. Stream calls ignore `data_truncated`. Datagram calls drop all received descriptors and return `InvalidData` when `data_truncated` is true.

`recv_fds_into` returns `(usize, Vec<OwnedFd>)` and has nowhere to report the flag, so making it strict leaves `cmsg_recvmsg` as the only path for a caller who intends to inspect a truncated datagram. Say that explicitly in the rustdoc for both datagram traits. It contradicts the spec line "lower-level methods may return the flag", so amend the spec in the same commit rather than leaving the two documents disagreeing.

- [ ] **Step 6: Run targeted and full tests**

```bash
cargo test --test fd_passing datagram_payload_truncation_is_an_error
cargo test --all-features --test async_fd async_datagram_payload_truncation_is_an_error
cargo test --all-features
```

Expected: all pass and descriptor leak tests remain green.

- [ ] **Step 7: Update `AGENTS.md` and commit**

```bash
git add AGENTS.md src/cmsg.rs src/lib.rs src/ext.rs tests/fd_passing.rs tests/async_fd.rs
git commit -m "fix: report Unix datagram payload truncation"
```

---

### Task 5: Add exact descriptor-count receive APIs

**Files:**
- Modify: `src/ext.rs`
- Modify: `src/asyncio.rs`
- Test: `tests/fd_passing.rs`
- Test: `tests/async_fd.rs`
- Test: `tests/fd_leak.rs`

**Interfaces:**
- Consumes: shared descriptor collection before surplus truncation.
- Produces: `recv_fds_exact::<N>` and `recv_fds_exact_into::<N>` for stream and datagram blocking/Tokio traits.

- [ ] **Step 1: Add exact-count tests**

Cover all three blocking cases in `tests/fd_passing.rs`:

```rust
#[test]
fn recv_exact_accepts_exact_count() {
    let (tx, rx) = UnixStream::pair().unwrap();
    let first = tempfile::tempfile().unwrap();
    let second = tempfile::tempfile().unwrap();
    tx.send_fds(b"two", &[&first, &second]).unwrap();

    let received = rx.recv_fds_exact::<2>().unwrap();
    assert_eq!(received.data, b"two");
    assert_eq!(received.fds.len(), 2);
}

#[test]
fn recv_exact_rejects_too_few() {
    let (tx, rx) = UnixStream::pair().unwrap();
    let file = tempfile::tempfile().unwrap();
    tx.send_fds(b"one", &[&file]).unwrap();

    let err = rx.recv_fds_exact::<2>().unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
}

#[test]
fn recv_exact_rejects_too_many() {
    let (tx, rx) = UnixStream::pair().unwrap();
    let first = tempfile::tempfile().unwrap();
    let second = tempfile::tempfile().unwrap();
    let third = tempfile::tempfile().unwrap();
    tx.send_fds(b"three", &[&first, &second, &third]).unwrap();

    let err = rx.recv_fds_exact::<2>().unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
}
```

Add these Tokio cases to `tests/async_fd.rs`:

```rust
#[tokio::test]
async fn async_recv_exact_accepts_exact_count() {
    let (tx, rx) = UnixStream::pair().unwrap();
    let first = tempfile::tempfile().unwrap();
    let second = tempfile::tempfile().unwrap();
    tx.send_fds(b"two", &[&first, &second]).await.unwrap();

    let received = rx.recv_fds_exact::<2>().await.unwrap();
    assert_eq!(received.data, b"two");
    assert_eq!(received.fds.len(), 2);
}

#[tokio::test]
async fn async_recv_exact_rejects_too_few() {
    let (tx, rx) = UnixStream::pair().unwrap();
    let file = tempfile::tempfile().unwrap();
    tx.send_fds(b"one", &[&file]).await.unwrap();

    let err = rx.recv_fds_exact::<2>().await.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
}

#[tokio::test]
async fn async_recv_exact_rejects_too_many() {
    let (tx, rx) = UnixStream::pair().unwrap();
    let first = tempfile::tempfile().unwrap();
    let second = tempfile::tempfile().unwrap();
    let third = tempfile::tempfile().unwrap();
    tx.send_fds(b"three", &[&first, &second, &third])
        .await
        .unwrap();

    let err = rx.recv_fds_exact::<2>().await.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
}
```

- [ ] **Step 2: Add strict-error leak test**

In the isolated `fd_leak` test binary, count open descriptors, send three descriptors, call `recv_fds_exact::<1>`, assert an error, drop all endpoints, and assert the descriptor count returns to baseline.

Take the baseline before creating the socket pair and the temp files, otherwise the assertion passes even if the strict path leaks. Follow the counting approach already in `tests/fd_leak.rs:11`.

Also add one datagram exact-count case. Step 5 puts `recv_fds_exact` on both traits and the stream cases alone will not catch a datagram wiring mistake.

- [ ] **Step 3: Run and verify compile failure**

Expected: exact APIs do not exist.

- [ ] **Step 4: Add internal count mode**

```rust
#[derive(Clone, Copy)]
pub(crate) enum CountMode {
    UpTo(usize),
    Exact(usize),
}
```

Collect every descriptor first. In exact mode, compare `all_fds.len()` before returning any descriptor. On mismatch, drop the vector and return `InvalidData` with expected and observed counts.

The spec describes a public `FdCount { UpTo, Exact }` enum instead. This plan keeps the count mode internal and exposes const-generic `recv_fds_exact::<N>`, which is the smaller surface. Amend the spec to match so Phase 3 and Phase 4 are not written against an enum that never ships.

- [ ] **Step 5: Add blocking trait methods**

Add for streams and datagrams:

```rust
fn recv_fds_exact<const N: usize>(&self) -> io::Result<ReceivedFds>;
fn recv_fds_exact_into<const N: usize>(
    &self,
    data_buf: &mut [u8],
) -> io::Result<(usize, Vec<OwnedFd>)>;
```

Existing methods use `CountMode::UpTo(N)` and preserve behavior.

- [ ] **Step 6: Add Tokio trait methods**

Mirror the blocking signatures as async methods and delegate through readiness to the same internal count mode.

- [ ] **Step 7: Run targeted and full tests**

```bash
cargo test --test fd_passing recv_exact
cargo test --test fd_leak
cargo test --all-features --test async_fd async_recv_exact
cargo test --all-features
```

Expected: exact, too-few, too-many, and leak cases all pass.

- [ ] **Step 8: Update `AGENTS.md` and commit**

```bash
git add AGENTS.md src/ext.rs src/asyncio.rs tests/fd_passing.rs tests/async_fd.rs tests/fd_leak.rs
git commit -m "feat: add strict descriptor-count receive APIs"
```

---

### Task 6: Finish core hardening documentation and verification

**Files:**
- Modify: `README.md`
- Modify: `src/lib.rs`
- Modify: `.github/workflows/ci.yml`
- Modify: `AGENTS.md`
- Modify: `Cargo.toml` (version to `0.4.0`)
- Create: `CHANGELOG.md`

**Interfaces:**
- Consumes: every new Phase 2 API and behavior.
- Produces: discoverable documentation, CI regression coverage, and an exact phase handoff.

- [ ] **Step 1: Update decision guidance**

Document:

- `send_fds` is one `sendmsg`, not a message transaction.
- `send_fds_all` sends descriptors once and completes ordinary bytes.
- `recv_fds` is permissive and closes surplus descriptors.
- `recv_fds_exact` validates protocol counts.
- Datagram convenience methods reject payload truncation.
- Stream callers needing boundaries should wait for or use `FdChannel`.

- [ ] **Step 2: Add explicit CI edge-test steps**

Under the existing test job, retain the full suite and add these commands after `cargo test --verbose --all-features`:

```yaml
      - name: Verify SIGPIPE-safe send behavior
        run: cargo test --test fd_passing send_to_closed_peer_returns_error_without_sigpipe -- --nocapture
      - name: Verify descriptor leak regressions
        run: cargo test --test fd_leak -- --nocapture
```

These steps make the security regressions visible without replacing the full suite.

Also add `cargo test` to the `msrv` job. It currently runs `cargo build --verbose` only (`.github/workflows/ci.yml:33-39`), so Step 4's local 1.75 test run is not enforced anywhere.

Create `CHANGELOG.md` in this task. Phase 2 lands three behavior breaks (stream empty-payload rejection, datagram truncation errors, `RecvResult` shape) and Phase 5 is too late to start recording them.

- [ ] **Step 3: Run the canonical stable acceptance workflow**

Run the exact Docker command from `AGENTS.md`, including public examples.

Expected: build, default tests, all-feature tests, docs, clippy, fmt, and four examples pass.

- [ ] **Step 4: Run MSRV verification**

Run with `rust:1.75`:

```bash
cargo build --all-targets
cargo test
```

Expected: pass with default features.

- [ ] **Step 5: Review public API diff**

Run:

```bash
git diff main...HEAD -- src/lib.rs src/ext.rs src/asyncio.rs README.md
```

Confirm existing methods remain and new behavior is documented.

- [ ] **Step 6: Update `AGENTS.md`, initiative, and commit**

Record exact command results and the next phase entry point.

```bash
git add AGENTS.md README.md CHANGELOG.md Cargo.toml src/lib.rs .github/workflows/ci.yml
git commit -m "docs: complete core hardening guidance and verification"
```

- [ ] **Step 7: Phase checkpoint**

Update the durable initiative to complete milestone `m2`, set `m3` current, and write the detailed Phase 3 plan against the verified Phase 2 interfaces.
