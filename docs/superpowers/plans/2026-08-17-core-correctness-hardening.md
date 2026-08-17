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

Add a helper test executable branch inside `tests/fd_passing.rs` driven by an environment variable:

```rust
#[test]
fn send_to_closed_peer_returns_error_without_sigpipe() {
    if std::env::var_os("UNIX_ANCILLARY_SIGPIPE_CHILD").is_some() {
        let (tx, rx) = UnixStream::pair().unwrap();
        drop(rx);
        let file = tempfile::tempfile().unwrap();
        let result = tx.send_fds(b"x", &[&file]);
        assert!(result.is_err());
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

Expected: on Linux, the child may terminate with `SIGPIPE`, proving the unsafe process behavior.

- [ ] **Step 3: Add platform send preparation**

In `src/platform.rs`, expose:

```rust
pub(crate) const SEND_FLAGS: libc::c_int = libc::MSG_NOSIGNAL;

pub(crate) fn prepare_send(_fd: RawFd) -> io::Result<()> {
    Ok(())
}
```

Use the constant only under targets where `MSG_NOSIGNAL` is available. On Apple targets, define `SEND_FLAGS` as `0` and implement `prepare_send` with `setsockopt(SOL_SOCKET, SO_NOSIGPIPE, 1)`.

Keep every `cfg` branch explicit for Linux, Android, macOS, FreeBSD, OpenBSD, NetBSD, and DragonFly. Unsupported Unix targets must fail compilation with an actionable message rather than silently using unsafe flags.

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
if fds.is_empty() {
    return (&self).write_all(data);
}
let borrowed: Vec<_> = fds.iter().map(|fd| fd.as_fd()).collect();
let first = send_fds_impl(self.as_fd(), data, &borrowed)?;
if first == 0 {
    return Err(io::Error::new(io::ErrorKind::WriteZero, "failed to send fd payload byte"));
}
(&self).write_all(&data[first..])
```

Import `std::io::Write`.

- [ ] **Step 6: Implement Tokio `send_fds_all`**

Add the async method. Perform the initial ancillary send through `async_io`. Then loop over the remaining bytes with `self.writable().await?` and `self.try_write(&remaining)`, retrying `WouldBlock`, and return `WriteZero` for zero progress.

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

Update crate docs and tests that construct or inspect it.

Add `pub data_truncated: bool` to the already `#[non_exhaustive]` `ReceivedFds`.

- [ ] **Step 5: Make convenience datagram receives strict**

Pass a socket-kind parameter into the shared receive implementation. Stream calls ignore `data_truncated`. Datagram calls drop all received descriptors and return `InvalidData` when `data_truncated` is true.

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
git add AGENTS.md README.md src/lib.rs .github/workflows/ci.yml
git commit -m "docs: complete core hardening guidance and verification"
```

- [ ] **Step 7: Phase checkpoint**

Update the durable initiative to complete milestone `m2`, set `m3` current, and write the detailed Phase 3 plan against the verified Phase 2 interfaces.
