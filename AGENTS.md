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
- Branch: `feat/production-capability-channel`
- Base branch: `main`
- Baseline commit: `71b2e5e6318168ec9060e3f2cbb2be011e281dfd`
- Published crate: `unix-ancillary 0.3.0`
- Rust edition: 2021
- Declared MSRV: Rust 1.75
- Durable initiative: `build-unix-ancillary-into-a-production-capability-channel`
- Current phase: Phase 1, baseline verification and architecture planning
- Baseline Docker status: complete on Rust 1.97.1; build, tests, doctests, rustdoc, Clippy, fmt, and all four public examples pass
- Last updated: 2026-08-17 UTC

## Current objective

Implement the approved roadmap in independently testable and committed phases:

1. Core correctness hardening.
2. Power-user low-level APIs.
3. Authenticated, message-oriented `FdChannel`.
4. Multiprocess examples, security assets, benchmarks, migration guides, and release readiness.

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
- [ ] Commit the handoff, design, and plans.
- [ ] Begin Phase 2 core hardening with TDD.

## Known correctness gaps to address

1. Unix stream reads do not preserve `send_fds` boundaries, but current docs do not emphasize this sufficiently.
2. Linux stream `SCM_RIGHTS` requires at least one normal byte; empty payload behavior is not enforced.
3. A partial `sendmsg` transfers descriptors once but may leave payload bytes unsent.
4. `sendmsg` currently uses flag `0`, leaving a potential `SIGPIPE` process-termination path.
5. Datagram payload truncation is not surfaced independently from ancillary truncation.
6. Surplus descriptor closing is safe but can hide protocol count mismatches.
7. High-level calls allocate descriptor and ancillary storage per operation.
8. Peer credentials, unconnected datagram addressing, and BSD execution coverage are missing.

## Last session activity

- Completed repository, competitor, adoption, and gap assessment.
- Confirmed public metrics: 298 crates.io downloads, 246 recent downloads, 6 GitHub stars, and zero published reverse dependencies at assessment time.
- Confirmed the standard-library ancillary API remains nightly-only.
- Confirmed newer projects such as `capsudo-transport` still implement descriptor transport, framing, and peer credentials directly with `nix`, supporting the higher-level opportunity.
- Created and switched to dedicated branch `feat/production-capability-channel` before implementation.
- Created the durable initiative and milestone structure.
- Pulled official `rust:latest` Docker image.
- Corrected Docker acceptance passed all builds, default and all-feature tests, doctests, rustdoc, Clippy, fmt, and the four shipped public examples on Rust 1.97.1.
- First Docker acceptance attempt failed only because login-shell `PATH` omitted Rust binaries. No project test result was produced by that attempt.

## Exact resume instructions

1. Read this file completely.
2. Run `git status --short` and confirm the current branch is `feat/production-capability-channel` and no unrelated changes will be overwritten.
3. Check the durable initiative `build-unix-ancillary-into-a-production-capability-channel`.
4. Baseline acceptance is complete. Re-run the canonical Docker workflow after each implementation milestone.
5. Review the design and Phase 2 plan, then continue at Phase 2 Task 1.
6. Commit the handoff, design, roadmap, and Phase 2 plan before implementation.
7. Use `superpowers:test-driven-development` for every feature or bug fix.
8. Update this file after each test cycle and commit.
