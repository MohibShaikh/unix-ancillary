# Production Capability Channel Design

**Date:** 2026-08-17

**Status:** Approved for phased implementation

**Repository:** `unix-ancillary`

**Branch:** `feat/production-capability-channel`

## Summary

`unix-ancillary` currently provides a focused, safe-by-default stable-Rust API for passing file descriptors through Unix sockets. It correctly uses `OwnedFd` and `BorrowedFd`, protects against descriptor leaks, applies close-on-exec semantics, supports blocking and Tokio sockets, and fuzzes its control-message parser.

The project will evolve in three layers:

1. A hardened syscall-safe ancillary core.
2. Explicit high-level descriptor operations for power users.
3. An authenticated, bounded, message-oriented `FdChannel`.

The crate will not become a general Unix syscall library. `nix`, `rustix`, and `libc` remain appropriate for broad ancillary message support. This project will specialize in correct, ergonomic, security-sensitive local capability transfer.

## Goals

1. Make every stream and datagram edge case explicit in the API.
2. Prevent process termination, descriptor duplication, leaks, and protocol ambiguity during error and retry paths.
3. Add reusable storage and runtime-neutral primitives for high-throughput callers.
4. Support peer identity verification and unconnected datagram addressing.
5. Provide framed capability messages with bounded payloads and descriptor counts.
6. Preserve a minimal default dependency graph and stable Rust support.
7. Provide real multiprocess examples and production-oriented project metadata.

## Non-goals

1. Replacing `nix` or `rustix`.
2. Supporting arbitrary IP ancillary messages such as timestamps, packet info, or error queues.
3. Hiding all platform differences. Platform-specific credential fields and security caveats remain documented.
4. Treating `SOCK_STREAM` as message-oriented without framing.
5. Adding serialization to the default feature set.
6. Adding a full RPC framework, service registry, or authorization language.

## Constraints

- Default build remains Unix-only and stable Rust.
- The low-level and high-level descriptor APIs retain Rust 1.75 MSRV unless implementation evidence makes that impossible.
- `tokio` remains optional.
- New optional serialization uses a separate feature.
- Safe high-level APIs expose `OwnedFd` and `BorrowedFd`, never unowned `RawFd` values.
- All received descriptors are owned immediately after parsing.
- Every error path closes all descriptors that will not be returned.
- Unsafe code remains localized to syscall, platform, and validated parsing modules.

## Existing architecture

- `src/ancillary.rs` constructs and parses control-message buffers.
- `src/cmsg.rs` owns the raw `sendmsg` and `recvmsg` calls.
- `src/platform.rs` selects CLOEXEC behavior and receive limits.
- `src/ext.rs` provides blocking extension traits for `UnixStream` and `UnixDatagram`.
- `src/asyncio.rs` applies the blocking core through Tokio readiness.
- `src/lib.rs` exports the public API and crate documentation.

The implementation already has a useful separation between syscall behavior and socket adapters. The new design preserves that separation and adds focused modules only where responsibilities become independently testable.

## Layer 1: hardened syscall-safe core

### Send flags

`platform.rs` will expose a platform-specific send flag constant or function. Linux, Android, and platforms with `MSG_NOSIGNAL` use it for all descriptor sends. Platforms that require socket-level suppression use the best supported behavior and document unavoidable differences.

The core send call must not use a bare flag value of zero when doing so can deliver `SIGPIPE`.

### Receive result metadata

The internal receive result will separately report:

- `bytes_read`
- `ancillary_len`
- `ancillary_truncated`
- `data_truncated`
- optional source address metadata when the caller requests it

`MSG_CTRUNC` and `MSG_TRUNC` are not interchangeable. High-level datagram APIs will reject either truncation by default.

### Empty payloads

A stream send carrying one or more descriptors requires at least one normal byte. The low-level `cmsg_sendmsg` remains a direct syscall-oriented API and documents the platform contract. High-level stream APIs reject an empty payload with `InvalidInput` when descriptors are non-empty.

Datagram behavior remains distinct because datagrams preserve boundaries and platform behavior permits different payload rules.

### EINTR

Raw syscall helpers retry `sendmsg` and `recvmsg` on `Interrupted`. They return `WouldBlock` unchanged so readiness adapters can retry correctly.

### Descriptor ownership

The parser continues to construct `OwnedFd` immediately. Unknown control messages are skipped safely. Malformed messages terminate parsing without unsafe pointer advancement. Any strict high-level receive error drops every collected descriptor before returning.

## Layer 2: explicit high-level descriptor operations

### Stream contract

The stream extension documentation will state:

- Unix streams do not preserve call boundaries.
- Bytes and descriptors from one send are not guaranteed to arrive through one receive call.
- Applications needing message boundaries should use `FdChannel`, a datagram socket, or a sequenced-packet transport.

The existing `send_fds` remains a one-`sendmsg` primitive and returns the number of payload bytes accepted.

### Descriptor-once complete sending

A new complete-send API provides a state machine rather than pretending a partial send can be retried from the beginning:

```rust
pub fn send_fds_all(
    &self,
    data: &[u8],
    fds: &[impl AsFd],
) -> io::Result<()>;
```

Behavior:

1. Reject non-empty descriptor sets paired with empty stream payloads.
2. Call `sendmsg` once with the descriptors.
3. If the first call writes fewer bytes than `data.len()`, write the remainder without ancillary data.
4. Retry interrupted operations.
5. Never resend descriptors after the first successful byte.

The Tokio form uses an explicit progress object or internal state machine so cancellation after descriptor transfer cannot cause a future retry to retransmit descriptors. A cancelled future may leave remaining ordinary payload bytes unsent, which is reported in its API contract. `FdChannel` avoids exposing partially committed frames by serializing writes through owned channel state.

### Count policy

Strict and permissive receive modes are separate:

```rust
pub enum FdCount {
    UpTo(usize),
    Exact(usize),
}
```

Const-generic convenience methods remain available. Strict mode errors if the peer sends fewer or more descriptors than expected. Surplus descriptors are still closed before returning the error.

### Receive outcome

High-level receive results expose protocol-relevant flags:

```rust
pub struct ReceivedFds {
    pub data: Vec<u8>,
    pub fds: Vec<OwnedFd>,
    pub data_truncated: bool,
}
```

Ancillary truncation remains a hard error because returning incomplete descriptor state is unsafe. Datagram data truncation is also a hard error in convenience methods. Lower-level methods may return the flag for callers that intentionally inspect truncated datagrams.

### Reusable buffers

A dedicated reusable receive object owns both data and ancillary storage:

```rust
pub struct FdRecvBuffer {
    data: Vec<u8>,
    ancillary: Vec<u8>,
}
```

It supports configurable payload and descriptor capacities and can be cleared without releasing allocation. The existing allocation-based convenience APIs delegate to it.

A corresponding send buffer is added only if benchmarks show meaningful benefit beyond borrowing caller data and reusing ancillary storage. YAGNI applies.

### Peer credentials

A portable model exposes fields that can be established reliably:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerCredentials {
    pub pid: Option<u32>,
    pub uid: u32,
    pub gid: u32,
}
```

Linux and Android use `SO_PEERCRED`. BSD and macOS use their available peer credential APIs. Unsupported fields are `None`, not invented values. The API distinguishes credentials established by the connected socket from credentials carried in a message.

### Datagram addressing

Unconnected datagram support adds explicit `send_fds_to` and `recv_fds_from` operations. Address parsing uses standard-library Unix socket address types where possible and keeps raw `sockaddr_un` handling internal.

### Runtime-neutral nonblocking core

Blocking high-level operations are expressed over `BorrowedFd`, not concrete socket types, inside the crate. Public extension traits remain ergonomic wrappers. Tokio uses readiness against the same `WouldBlock`-preserving operations.

Additional runtime adapters are separate optional modules or companion crates only when actual downstream demand justifies their maintenance.

## Layer 3: `FdChannel`

### Purpose

`FdChannel` provides the abstraction repeatedly implemented by sandbox runtimes, capability brokers, plugin hosts, and privilege-separated daemons: bounded framed messages with attached owned capabilities and verified peer identity.

### Transport

The first implementation uses connected Unix streams because standard-library and Tokio support is mature and portable. Frame boundaries are implemented in user space.

`SOCK_SEQPACKET` is evaluated after the framed stream implementation is stable. It may be offered as a transport adapter rather than becoming a prerequisite.

### Wire format

Version 1 uses a fixed-size, endian-defined header:

```text
magic:       4 bytes  "UANC"
version:     u8       1
flags:       u8
fd_count:    u16 big-endian
payload_len: u32 big-endian
message_id:  u64 big-endian
```

The header is 20 bytes. Reserved flag bits must be zero. Receivers reject:

- Wrong magic
- Unsupported version
- Reserved flags
- Payloads above configured maximum
- Descriptor counts above configured maximum
- Descriptor count different from the descriptors attached to the frame
- EOF during a header or payload

The header is sent with the descriptors in the initial `sendmsg`. It is always non-empty, satisfying stream descriptor-transfer requirements. Remaining payload bytes may follow through ordinary writes.

### Frame API

```rust
pub struct ChannelConfig {
    pub max_payload: usize,
    pub max_fds: usize,
}

pub struct Frame {
    pub message_id: u64,
    pub payload: Vec<u8>,
    pub fds: Vec<OwnedFd>,
}

pub struct FdChannel<S> {
    io: S,
    config: ChannelConfig,
    peer: Option<PeerCredentials>,
}
```

Blocking and Tokio channel types may use distinct concrete wrappers if a shared generic type harms clarity.

### Write serialization and cancellation

A channel write is serialized through mutable channel access. The Tokio API requires `&mut self`, preventing concurrent frame interleaving without an explicit synchronization wrapper.

The send state tracks:

1. Header and descriptors not started.
2. Header partially written with descriptors already transferred.
3. Header completed.
4. Payload partially written.
5. Frame completed.

Cancellation may interrupt progress. The channel becomes poisoned if a frame is partially committed and cannot be resumed safely. The caller must close or explicitly recover by completing the stored send state. The initial implementation chooses poisoning over silent protocol corruption.

### Peer authentication

Channel construction may query peer credentials and validate a policy:

```rust
pub enum PeerPolicy {
    Any,
    Uid(u32),
    Gid(u32),
    Pid(u32),
}
```

More complex authorization remains application code. The crate provides verified identity facts and basic equality policies, not an authorization engine.

### Optional serialization

An optional `serde` feature provides convenience methods that serialize payloads with a documented format selected by another feature or helper module. The core channel only transports bytes and descriptors. Serialization failures happen before committing the frame.

### Child process establishment

Helpers create a Unix socket pair, preserve one endpoint across spawn using an explicit inherited descriptor, and return the parent channel. The API documents the required child-side descriptor number or environment variable. It does not perform privilege dropping or sandbox setup itself.

## Errors

Low-level and extension APIs continue to return `io::Error` where the error maps directly to a syscall or input contract.

`FdChannel` uses a typed non-exhaustive error:

```rust
#[non_exhaustive]
pub enum ChannelError {
    Io(io::Error),
    Protocol(ProtocolError),
    PeerRejected(PeerCredentials),
    Poisoned,
}
```

Protocol errors distinguish invalid magic, unsupported version, invalid flags, oversized payload, oversized descriptor count, descriptor count mismatch, and unexpected EOF.

Every error type includes actionable display text without descriptor values or sensitive payload contents.

## Feature flags and dependencies

Initial feature layout:

```toml
[features]
default = []
tokio = ["dep:tokio"]
channel = []
serde = ["channel", "dep:serde"]
```

The byte-oriented blocking channel may become part of the default API after implementation review because it requires no new dependencies. Tokio channel support remains under `tokio`. Serialization remains optional.

No dependency is added when equivalent behavior is small, well-tested, and already platform-specific inside the crate.

## Testing strategy

### Unit tests

- Frame header encoding and decoding.
- Every protocol rejection case.
- Receive limit calculations.
- Peer policy evaluation.
- Reusable buffer reset and growth behavior.
- Platform flag selection.

### Integration tests

- Empty stream payload rejection.
- At least one payload byte transfers descriptors.
- Partial initial send never duplicates descriptors.
- Closed peer returns an error without terminating the process.
- Exact descriptor count accepts exact and rejects fewer or more.
- Datagram payload truncation is reported.
- Surplus descriptors close on strict errors.
- Consecutive stream sends do not imply receive boundaries.
- Blocking channel frame round trip.
- Tokio channel frame round trip.
- Cancellation and poisoned-channel behavior.
- Peer credential retrieval on supported platforms.
- Unconnected datagram address round trip.

### Multiprocess acceptance tests

- Parent passes a file to a spawned child.
- Privileged supervisor hands a listening socket to a worker process.
- Server hands a live listener to a replacement process.
- Authenticated channel accepts the expected UID and rejects a mismatched policy.

### Platform verification

- Ubuntu stable and MSRV.
- macOS stable.
- At least one BSD execution environment before claiming tested BSD support.
- Fuzz target remains clean for arbitrary control-message bytes.
- Rustdoc, clippy with warnings denied, formatting, and semver checks.

## Documentation and adoption

The README will lead with the problem and include:

- A precise stream-semantics warning.
- A primitive-versus-channel decision guide.
- A comparison against `sendfd`, `nix`, `rustix`, standard library nightly, and `anchovy`.
- Real multiprocess quick starts.
- Platform credential and CLOEXEC caveats.

Additional project assets:

- `CHANGELOG.md`
- `SECURITY.md`
- Platform support matrix
- Migration guides from `sendfd`, `nix`, and raw `libc`
- Benchmarks for allocation reuse and framed throughput
- `cargo-semver-checks`
- Periodic fuzz workflow
- GitHub description and topics documented for the maintainer to configure

## Delivery phases

### Phase 1: baseline and plans

Establish Docker verification, `AGENTS.md`, this design, phase plans, and clean commits.

### Phase 2: core hardening

Implement empty-payload enforcement, stream documentation, SIGPIPE-safe flags, EINTR handling, descriptor-once complete sends, separate truncation metadata, and exact count policies.

### Phase 3: power-user APIs

Implement reusable buffers, peer credentials, unconnected datagram addressing, and internal `BorrowedFd` operations shared by adapters.

### Phase 4: capability channel

Implement the frame protocol, blocking channel, Tokio channel, peer policies, optional serialization, spawn helpers, and sequenced-packet evaluation.

### Phase 5: adoption and release readiness

Implement real multiprocess examples, benchmarks, security and migration documents, expanded CI, final acceptance verification, and release notes.

## Compatibility strategy

Existing 0.3 APIs remain source-compatible during hardening unless behavior is demonstrably unsafe. New strict APIs are additive. Any unavoidable breaking change is grouped into the next semver-minor pre-1.0 release and documented in the changelog and migration guide.

The crate remains pre-1.0, but changes still receive semver review because downstream confidence is a core adoption goal.

## Success criteria

The design is complete when:

1. Every stated correctness gap has an automated regression test.
2. The existing convenience API remains simple for one-off descriptor passing.
3. Power users can avoid repeated allocations and verify peer identity.
4. `FdChannel` transports bounded frames without ambiguous descriptor association.
5. Blocking and Tokio multiprocess examples pass through public APIs.
6. Linux, macOS, MSRV, docs, clippy, format, fuzz, and supported-BSD checks are recorded.
7. `AGENTS.md` gives a new harness everything needed to resume safely.
