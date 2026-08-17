# Production Capability Channel Execution Roadmap

> **For agentic workers:** Each phase has or will receive a dedicated implementation plan. Execute phases in order. Update `AGENTS.md` after every test result and commit.

**Goal:** Deliver the approved production capability channel design through independently useful, acceptance-tested releases.

**Architecture:** Preserve the syscall-safe ancillary core, harden the existing public operations, then add reusable and authenticated primitives before introducing framed channel semantics. Adoption assets and release verification are a separate final phase so they exercise completed public APIs.

**Tech Stack:** Rust 2021, stable Rust, Rust 1.75 MSRV for the low-level crate, libc, optional Tokio, GitHub Actions, Docker-based local verification.

**Spec:** `docs/superpowers/specs/2026-08-17-production-capability-channel-design.md`

## Global constraints

- Work only on `feat/production-capability-channel`.
- Keep `AGENTS.md` synchronized with observed state.
- Use TDD for every behavioral change.
- Commit each independently verified task.
- Safe public APIs use `OwnedFd` and `BorrowedFd`.
- Default dependencies remain `libc` only.
- Tokio and serialization remain optional.
- Never claim platform support without execution evidence.

## Phase plans

1. **Phase 1: baseline and durable handoff**
   - Files: `AGENTS.md`, design spec, roadmap, phase plans.
   - Exit: baseline public APIs and examples pass in Docker, documents committed.

2. **Phase 2: core correctness hardening**
   - Plan: `docs/superpowers/plans/2026-08-17-core-correctness-hardening.md`
   - Exit: empty payloads, SIGPIPE, EINTR, partial sends, truncation, and exact FD counts are explicit and tested.

3. **Phase 3: power-user low-level APIs**
   - Planned scope: reusable receive storage, peer credentials, unconnected datagrams, runtime-neutral internals.
   - Write the detailed phase plan after Phase 2 public types and signatures are verified.

4. **Phase 4: authenticated `FdChannel`**
   - Planned scope: protocol codec, blocking channel, Tokio channel, peer policy, optional serialization, spawn helpers, sequenced-packet evaluation.
   - Write the detailed phase plan after Phase 3 confirms the reusable and credential interfaces consumed by the channel.

5. **Phase 5: adoption and release readiness**
   - Planned scope: real multiprocess examples, benchmarks, security and migration documents, expanded CI, semver and fuzz checks, release notes.
   - Write the detailed phase plan against the completed public API.

## Phase transition gate

Before moving to the next phase:

1. Run the canonical Docker acceptance workflow from `AGENTS.md`.
2. Run the phase-specific edge and integration tests.
3. Run the Rust 1.75 build where the phase affects default features.
4. Update `AGENTS.md` with commands and observed results.
5. Commit all phase work with a focused message.
6. Update the durable initiative checkpoint and current milestone.
