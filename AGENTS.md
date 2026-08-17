# AGENTS.md

## Purpose

This file is the authoritative handoff ledger for agents, harnesses, and maintainers working on `unix-ancillary`.

The project is being evolved from a safe `SCM_RIGHTS` primitive into a production-grade capability channel for Rust processes. Keep this file current so another agent can resume without relying on chat history.

## Mandatory update protocol

Update this file:

1. Before starting a new implementation task.
2. After every meaningful test or verification result.
3. After every commit.
4. When discovering a blocker, API decision, platform constraint, or failed approach.
5. Before ending a session or when model/tool limits may be near.

Never mark work complete here without recording the exact verification command and observed result.

## Repository state

- Repository: `/home/noman/personal-projects/unix-ancillary`
- Branch: `feat/phase2-core-hardening`
- Base branch: `main`; the planning commit `1d75576` is the branch base
- Baseline commit: `71b2e5e6318168ec9060e3f2cbb2be011e281dfd`
- Published crate: `unix-ancillary 0.3.0` (working tree is `0.4.0`, unreleased)
- Rust edition: 2021
- Declared MSRV: Rust 1.75
- Durable initiative: `build-unix-ancillary-into-a-production-capability-channel`
- Current phase: Phase 2 complete (core correctness hardening); crate ends at Layer 2
- Phase 2 Task 1 completed on 2026-08-17 on branch `feat/phase2-core-hardening` (a new branch carrying the reviewed plan edits; the planning commit `1d75576` is its base). Red state observed first: both new tests failed with `unwrap_err()` on `Ok(0)`; after `validate_stream_send` in `src/ext.rs` and calls in `UnixStreamExt::send_fds` and `AsyncUnixStreamExt::send_fds`, `cargo test --test fd_passing` (9 tests) and `cargo test --all-features --test async_fd` (4 tests) pass in Docker on Rust 1.97.1.
- Phase 2 Task 2 completed on 2026-08-17. The SIGPIPE subprocess regression test went red first with `child terminated with signal: 13 (SIGPIPE)`. Added `SEND_FLAGS` (`MSG_NOSIGNAL` for the CLOEXEC six, `SO_NOSIGPIPE` via `prepare_send` on Apple, documented `0` fallback for unknown Unix) in `src/platform.rs` and EINTR retry loops in both `sendmsg_vectored` and `recvmsg_vectored` in `src/cmsg.rs`. The regression test, full default suite (10 fd_passing tests), all-feature suite, clippy `-D warnings`, and fmt all pass in Docker on Rust 1.97.1.
- Phase 2 Task 3 completed on 2026-08-17. Added `send_fds_all` to `UnixStreamExt` and `AsyncUnixStreamExt`. Tests went red first on compile (`no method named send_fds_all`). Added `cmsg::send_bytes` and used it (not `Write::write_all`) for the tail so `MSG_NOSIGNAL` covers the whole payload. tokio dev-deps gained `io-util` for `AsyncReadExt` in the round-trip test. Blocking and Tokio round-trip tests pass (payload 128 KiB + one descriptor, exactly one descriptor delivered), full default and all-feature suites pass, clippy and fmt clean in Docker on Rust 1.97.1.
- Phase 2 Task 4 completed on 2026-08-17. Both datagram truncation tests went red first (`unwrap_err()` on `Ok(..)`). Split `RecvMsgResult.truncated` into `ancillary_truncated` + `data_truncated`; `RecvResult` is now `#[non_exhaustive]` with `data_truncated` (a deliberate source break: version bumped to 0.4.0 and `CHANGELOG.md` created in this task, not deferred); `ReceivedFds` gained `data_truncated`. `recv_fds_into_impl` takes `SocketKind` and datagram calls drop all received fds then return `InvalidData` on payload truncation, leaving `cmsg_recvmsg` as the only lenient path. The spec's "lower-level methods may return the flag" line was already corrected by the reviewed plan edits. Full default (12 fd_passing tests) and all-feature suites, clippy, and fmt pass in Docker on Rust 1.97.1.
- Phase 2 Task 5 completed on 2026-08-17. Added `recv_fds_exact`/`recv_fds_exact_into` to `UnixStreamExt`, `UnixDatagramExt`, `AsyncUnixStreamExt`, and `AsyncUnixDatagramExt`. Tests went red first on compile (`no method named recv_fds_exact`). Introduced internal `CountMode::{UpTo,Exact}`; `recv_fds_into_impl` now takes `SocketKind` + `CountMode` and drops every received descriptor before returning `InvalidData` on an exact-count mismatch. The two fd-counting leak tests in `tests/fd_leak.rs` raced the process-global fd table under default test concurrency, so they serialize through a shared `Mutex` (`FD_LOCK`). Blocking (4), Tokio (4), and leak (2) tests pass, full default (16 fd_passing tests) and all-feature suites pass, clippy and fmt clean in Docker on Rust 1.97.1.
- Phase 2 Task 6 completed on 2026-08-17. README gained a "Choosing a send / receive method" guide and crate docs mirror it; CI gained the SIGPIPE and fd_leak edge-test steps plus `cargo test` in the msrv job; `CHANGELOG.md` documents the 0.4.0 source breaks. Canonical Docker acceptance on Rust 1.97.1 passes in full (build, default + all-feature tests, rustdoc, clippy `-D warnings`, fmt, all four examples). MSRV verified on `rust:1.75` (`cargo build --all-targets` + `cargo test`) after pinning `tempfile` to 3.14.0 in `Cargo.lock` — 3.26 pulls `getrandom 0.4.1`, which requires the edition2024 Cargo feature unavailable in 1.75.
- Baseline Docker status: complete on Rust 1.97.1; build, tests, doctests, rustdoc, Clippy, fmt, and all four public examples pass
- Last updated: 2026-08-17 UTC

## Current objective

Implement the approved roadmap in independently testable and committed phases:

1. Core correctness hardening.
2. Peer credentials and shared runtime-neutral internals.
3. Multiprocess examples, security assets, migration guides, and release readiness, targeting one real downstream dependent.
4. Authenticated, message-oriented `FdChannel`, only if a named downstream user asks for it.

Reordered and narrowed on 2026-08-17. Do not build reusable receive buffers, unconnected datagram addressing, spawn helpers, or seqpacket transport: `rustix`, `uds`, `command-fds`, and `tokio-seqpacket` already ship them. See the Prior art section of the design spec.

## Non-negotiable constraints

- Stable Rust only.
- Preserve Rust 1.75 MSRV for the low-level crate unless a documented design decision explicitly changes it.
- No `RawFd` in safe public high-level APIs. Use `OwnedFd` and `BorrowedFd`.
- Received descriptors must be owned immediately and closed on every error path.
- `FD_CLOEXEC` behavior must remain safe-by-default and platform caveats must be explicit.
- Keep the default dependency surface minimal. Tokio and serialization remain optional features.
- Every behavior change starts with a failing test.
- Each milestone ends with the full acceptance workflow and a focused commit.
- Do not broaden into a general replacement for `nix` or `rustix`.

## Baseline evidence

Public GitHub Actions run `29920977162` at baseline commit `71b2e5e` completed successfully on 2026-07-22:

- Ubuntu: build all targets, default tests, all-feature tests, clippy, and fmt passed.
- macOS: build all targets, default tests, all-feature tests, clippy, and fmt passed.
- Rust 1.75 MSRV build passed.
- All-feature rustdoc build passed.

Local Docker acceptance on 2026-08-17 using `rust:latest` and Rust 1.97.1 observed:

- `cargo build --all-targets --all-features`: passed.
- `cargo test`: passed, including 2 unit tests, 1 leak test, 7 blocking integration tests, and 1 doctest.
- `cargo test --all-features`: passed, including 3 Tokio integration tests plus the default test set.
- `cargo doc --all-features --no-deps`: passed.
- The first lint attempt stopped because the image lacked optional components. After `rustup component add clippy rustfmt`, `cargo clippy --all-targets --all-features -- -D warnings` and `cargo fmt --all -- --check` both passed.
- `pass_fd` passed and read the expected file contents through the received descriptor.
- `privsep` passed and the worker reconstructed the live listener without binding it.
- `async_pass_fd` passed and read the expected async file contents.
- `agent_sandbox` passed and the sandbox consumed only the granted file capability.

Local host has no `cargo`, `rustc`, or `rustup` in `PATH`. Docker 29.7.2 is available and the official `rust:latest` image has been pulled. The first Docker acceptance attempt failed because `sh -lc` reset the image `PATH`; use `/bin/bash -c` or absolute `/usr/local/cargo/bin/*` paths.

## Canonical Docker verification

The repository must be mounted read-only and copied inside the container so generated artifacts never modify the host checkout:

```bash
docker run --rm --network=bridge \
  -v /home/noman/personal-projects/unix-ancillary:/src:ro \
  rust:latest /bin/bash -c '
    set -euo pipefail
    cp -a /src /work
    cd /work
    cargo build --all-targets --all-features
    cargo test
    cargo test --all-features
    cargo doc --all-features --no-deps
    cargo clippy --all-targets --all-features -- -D warnings
    cargo fmt --all -- --check
    cargo run --quiet --example pass_fd
    cargo run --quiet --example privsep
    cargo run --quiet --example async_pass_fd --features tokio
    cargo run --quiet --example agent_sandbox --features tokio
  '
```

Also verify MSRV before release milestones:

```bash
docker run --rm --network=bridge \
  -v /home/noman/personal-projects/unix-ancillary:/src:ro \
  rust:1.75 /bin/bash -c '
    set -euo pipefail
    cp -a /src /work
    cd /work
    cargo build --all-targets
  '
```

## Architecture direction

### Layer 1: syscall-safe ancillary core

Responsible for `sendmsg`, `recvmsg`, control-message parsing, descriptor ownership, CLOEXEC, truncation flags, and platform flags. It must remain usable by blocking and async adapters.

### Layer 2: explicit high-level FD operations

Provides:

- Stream APIs with documented non-message semantics.
- Empty-payload protection.
- SIGPIPE-safe sending.
- Descriptor-once partial-send handling.
- Strict and permissive descriptor count policies.
- Datagram payload truncation reporting.
- Reusable ancillary buffers.
- Peer credentials and datagram source addressing.

### Layer 3: `FdChannel`

Provides bounded, framed messages with attached descriptors, exact protocol validation, peer authentication, optional serialization, cancellation-safe Tokio support, and child-process establishment helpers. `SOCK_SEQPACKET` may be added as a transport where portability and API quality justify it.

## Planned files

Existing files expected to change:

- `src/cmsg.rs`: syscall flags, result metadata, address support.
- `src/platform.rs`: send flags, peer credentials, platform capabilities.
- `src/ancillary.rs`: reusable control buffers and additional safe message parsing.
- `src/ext.rs`: hardened blocking public APIs.
- `src/asyncio.rs`: cancellation-safe Tokio APIs.
- `src/lib.rs`: exports and crate documentation.
- `tests/fd_passing.rs`: blocking behavior and edge cases.
- `tests/async_fd.rs`: async behavior and cancellation cases.
- `tests/fd_leak.rs`: ownership and leak regressions.
- `.github/workflows/ci.yml`: expanded verification.
- `README.md`: precise stream semantics and capability-channel positioning.

Likely new focused modules:

- `src/buffer.rs`: reusable send and receive storage.
- `src/credentials.rs`: cross-platform peer credential model.
- `src/channel.rs`: framed blocking `FdChannel`.
- `src/channel/tokio.rs`: optional Tokio channel adapter.
- `src/protocol.rs`: frame header validation and limits.

Do not create these files blindly. Follow the committed implementation plan and preserve focused responsibilities.

## Current task queue

- [x] Complete baseline Docker acceptance workflow.
- [x] Create this live `AGENTS.md` handoff ledger.
- [x] Write architecture design to `docs/superpowers/specs/2026-08-17-production-capability-channel-design.md`.
- [x] Write the execution roadmap and detailed Phase 2 plan under `docs/superpowers/plans/`.
- [x] Commit the handoff, design, and plans as `1d75576`.
- [x] Phase 2 Task 1: enforce the Unix stream fd payload contract (`fix: enforce Unix stream fd payload contract`).
- [x] Phase 2 Task 2: SIGPIPE-safe and EINTR-resilient syscalls (`fix: make ancillary sends signal-safe and EINTR-resilient`).
- [x] Phase 2 Task 3: descriptor-once complete stream sends (`feat: add descriptor-once complete stream sends`).
- [x] Phase 2 Task 4: surface datagram payload truncation (`fix: report Unix datagram payload truncation`).
- [x] Phase 2 Task 5: exact descriptor-count receive APIs (`feat: add strict descriptor-count receive APIs`).
- [x] Phase 2 Task 6: finish core hardening documentation and verification (`docs: complete core hardening guidance and verification`).
- [x] Phase 2 complete on 2026-08-17. Canonical Docker acceptance on Rust 1.97.1 passes (build, default + all-feature tests, rustdoc, clippy `-D warnings`, fmt, four examples). MSRV verified on `rust:1.75` with `cargo build --all-targets` and `cargo test` after pinning `tempfile` to 3.14.0 in `Cargo.lock` (3.26 pulls `getrandom 0.4.1`, which requires the edition2024 Cargo feature). Per the user's decision, the crate ends at Layer 2: peer credentials stay as the one absorbed duplication, Phase 4's exit remains one real dependent or a documented stop, and `FdChannel` stays gated on a named request. Do not start Phase 3+ without a new directive.

## Known correctness gaps to address

Addressed by Phase 2 on 2026-08-17:

1. ~~Unix stream reads do not preserve `send_fds` boundaries~~ — documented on both stream traits, crate docs, and README; framed protocols recommended.
2. ~~Linux stream `SCM_RIGHTS` requires at least one normal byte~~ — `send_fds`/`send_fds_all` reject empty payloads with fds via `InvalidInput`.
3. ~~Partial `sendmsg` transfers descriptors once but may leave payload bytes unsent~~ — `send_fds_all` completes the payload descriptor-once with signal-safe sends.
4. ~~`sendmsg` uses flag `0`, leaving a `SIGPIPE` path~~ — `MSG_NOSIGNAL` / `SO_NOSIGPIPE`; unknown Unix targets documented.
5. ~~Datagram payload truncation not surfaced independently~~ — `RecvResult::data_truncated`; convenience methods reject it with `InvalidData`.
6. ~~Surplus descriptor closing hides count mismatches~~ — `recv_fds_exact`/`recv_fds_exact_into` error on mismatch while still closing surplus fds.

Remaining:

7. High-level calls allocate descriptor and ancillary storage per operation. Accepted, not a gap: `rustix` owns the reusable-buffer story.
8. Peer credentials and BSD execution coverage are missing. Unconnected datagram addressing is delegated to `uds`. Peer credentials stay the one absorbed duplication (Phase 3), gated on a new directive.

## Last session activity

- Prior session (baseline): assessed repo/competitors/adoption (298 downloads, 246 recent, 6 stars, 0 reverse deps); confirmed stdlib ancillary is nightly-only; created durable initiative and `feat/production-capability-channel`; pulled `rust:latest`; baseline Docker acceptance passed; committed handoff, design, roadmap, and Phase 2 plan as `1d75576`; a gpt-5.5 dispatch hit its provider limit before editing; reviewed the Phase 2 plan against source and corrected six items (SIGPIPE `SIG_IGN`, `write(2)` tail, uncompilable snippet, `RecvResult` source break / 0.4.0 bump, `cmsg_recvmsg` as only lenient path, `compile_error!` breaking illumos/Solaris/AIX). Measured crates.io on 2026-08-17: `sendfd` 2,285,277 / 90d from 13 dependents, `uds` 1,359,468 from 10, `ipc-channel` 804,818 from 54, `fd-queue` 143 from 1, `unix-ancillary` 246 from 0; downloads track dependents, roadmap narrowed accordingly.
- Completed Phase 2 (core correctness hardening) end to end on branch `feat/phase2-core-hardening` from planning commit `1d75576`, six tasks, one commit each, each starting red and verified in Docker on Rust 1.97.1:
  - `adf2cc0` `fix: enforce Unix stream fd payload contract` — `validate_stream_send`; empty payloads with fds now reject with `InvalidInput`.
  - `92889c4` `fix: make ancillary sends signal-safe and EINTR-resilient` — `MSG_NOSIGNAL` for the CLOEXEC six, `SO_NOSIGPIPE` via `prepare_send` on Apple, `0` fallback documented; EINTR retry loops in both `sendmsg_vectored` and `recvmsg_vectored`.
  - `ba547be` `feat: add descriptor-once complete stream sends` — `send_fds_all` on blocking and async stream traits; `cmsg::send_bytes` so `MSG_NOSIGNAL` covers the whole payload.
  - `70209ec` `fix: report Unix datagram payload truncation` — split `ancillary_truncated`/`data_truncated`; `RecvResult`/`ReceivedFds` source break (0.4.0 bump); `CHANGELOG.md` created.
  - `de61c8a` `feat: add strict descriptor-count receive APIs` — `recv_fds_exact`/`recv_fds_exact_into` on all four traits; `CountMode::{UpTo,Exact}`; exact mismatch drops all received fds then returns `InvalidData`; leak tests serialize via `FD_LOCK`.
  - Task 6 (README "Choosing a send / receive method" guide, CI edge-test steps + msrv `cargo test`, changelog) verified by canonical Docker acceptance: build --all-targets --all-features, `cargo test` (16 fd_passing), `cargo test --all-features` (10 async), rustdoc, clippy `-D warnings`, fmt check, and all four examples pass on Rust 1.97.1.
- MSRV verified on `rust:1.75`: `cargo build --all-targets` and `cargo test` both pass after pinning `tempfile` to 3.14.0 in `Cargo.lock` (the previously locked 3.26 pulls `getrandom 0.4.1`, which requires the edition2024 Cargo feature that Cargo 1.75 lacks). The `--precise` downgrade also drops the wasip2/wit-parser transitive set.
- Per the user's direction the crate ends at Layer 2. Peer credentials stay the one absorbed duplication (Phase 3), Phase 4's exit stays one real dependent or a documented stop, and `FdChannel` (Phase 5) stays gated on a named downstream request. Do not start Phase 3+ without a new directive.

## Exact resume instructions

1. Read this file completely.
2. Run `git status --short` and confirm the current branch is `feat/phase2-core-hardening` and no unrelated changes will be overwritten.
3. Check the durable initiative `build-unix-ancillary-into-a-production-capability-channel`.
4. Baseline and Phase 2 acceptance are complete. Re-run the canonical Docker workflow after each future milestone.
5. Read `docs/superpowers/specs/2026-08-17-production-capability-channel-design.md`, `docs/superpowers/plans/2026-08-17-core-correctness-hardening.md`, and the roadmap.
6. Phase 2 is complete. Do not start Phase 3+ without a new directive from the user.
7. Use `superpowers:test-driven-development` for every feature or bug fix.
8. Update this file after each test cycle and commit.
