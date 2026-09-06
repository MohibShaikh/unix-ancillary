Adversarial correctness and adoption audit — 2026-09-05

Follow-up: the findings below describe published 0.5.0. Repairs and verification for the unreleased 0.6.0 are recorded in [the remediation report](2026-09-06-remediation.md). The archived diagnostic fixture targets the old API at `bed5bd7`; current regression tests are in `tests/ancillary_ownership.rs`.

Audited `main` at `bed5bd7`. All seven `src/*.rs` files match the downloaded crates.io 0.5.0 archive byte for byte. This is an audit of published behavior. No runtime code was changed, no release was published, and no downstream messages or PRs were sent. The original checkout was clean. Diagnostic tests ran in disposable Docker copies, with the checkout mounted read-only.

The download increase is real, but the safe low-level API has ownership and alignment defects that should be resolved before soliciting new adopters. The strongest evidence about adoption is an existing consumer deliberately removing the crate because it already used rustix. Additional features alone do not address that reason.

1. **P1 — Safe low-level calls violate descriptor ownership and lifetime.** `src/ancillary.rs:334` copies borrowed descriptor numbers without retaining their lifetimes; `messages(&self)` at line 349 can repeatedly manufacture owning descriptors via `ScmRights::next` at line 80. Three safe-caller reproductions failed:

   - `add_fds(&[file.as_fd()])`, then consuming the buffer through `messages()` and dropping the returned descriptor, closes the original file. `file.metadata()` returned `EBADF`.
   - Parsing a received buffer twice produced two `OwnedFd` values for fd 10. The diagnostic deliberately forgot one owner to avoid a second close.
   - A buffer prepared with `/dev/null` remained usable after dropping that file. Opening `/dev/zero` reused the descriptor number, and sending the old buffer delivered `/dev/zero`. The receiver read one byte where the intended `/dev/null` would return EOF.

   The documentation's instruction to iterate exactly once does not make this sound as a safe API. Separate send and receive states; retain send descriptor borrows or duplicate them; make ownership extraction destructive and impossible to repeat. Merely changing `&self` to `&mut self` does not prevent sequential repeated extraction. Existing high-level call sites can likely retain their shape while the low-level API is repaired; the precise compatibility cost needs an implementation design. Rust's [FromRawFd contract](https://doc.rust-lang.org/std/os/fd/trait.FromRawFd.html) requires an open, owned descriptor.

2. **P1 — A safe unaligned byte slice causes undefined behavior.** `SocketAncillary::new` accepts arbitrary `&mut [u8]`, but `add_cmsg` dereferences it as a naturally aligned `cmsghdr` at `src/ancillary.rs:320`. Passing a deliberately offset slice caused a non-unwinding misaligned-pointer panic and SIGABRT in the child process. `Messages::next` and the platform fallback also contain typed header dereferences. A `Vec<u8>` or byte array does not provide an API-level alignment guarantee, even if a particular allocator happens to over-align it. Enforce alignment internally, use appropriate storage, or consistently parse/write without aligned references; account for libc helper requirements too. Test offsets across the full header alignment, not only heap-aligned input.

3. **P1 — Unconsumed low-level receives leak FDs.** `SocketAncillary`, `Messages`, and `ScmRights` have no cleanup of unconsumed descriptors. Receiving and dropping four buffers without iteration grew `/proc/self/fd` count from 7 to 11. Early return after inspecting credentials, partial iteration, `clear()`, and buffer reuse need an ownership design that accounts for every remaining descriptor. The reproduction directly tested dropping uniterated buffers; the other paths follow from the same absence of cleanup and should get dedicated regressions during repair. Adding `Drop` indiscriminately to the current shared send/receive type would wrongly close borrowed send descriptors. Fix this together with finding 1.

4. **P2 — Enabling credentials breaks a valid maximum-sized FD receive.** `src/ext.rs:124` reserves `CMSG_SPACE(cap * sizeof(fd))` only. With `set_passcred(&rx, true)` and 253 legal descriptors, `recv_fds_exact::<253>()` returned `Other` with the message `ancillary truncated despite oversized buffer; possible fd leak — abort the connection`. The identical high-level test without passcred passed. Kernel-supplied credentials consume additional control-buffer space. Reserve space for supported additional messages and document the actual limits. This reproduction demonstrates rejection of valid traffic, not a Linux descriptor leak. The README's claim that truncation is impossible is disproved by this supported configuration. Linux documents both automatic credentials and truncated-rights handling in [unix(7)](https://man7.org/linux/man-pages/man7/unix.7.html).

5. **P2 — The advertised fuzz target does not compile.** `cargo check --manifest-path fuzz/Cargo.toml` failed with E0004 at `fuzz/fuzz_targets/parse_cmsg.rs:27`. Its match lacks a wildcard after `AncillaryData` became non-exhaustive. Main-package `--all-targets` does not build the separate fuzz workspace, so the green CI misses it. Restore the harness, add a compile check to CI, and cover safe API state transitions and alignment. Random parser bytes alone cannot establish descriptor ownership correctness. Historical fuzz execution counts are not evidence that this release's harness works.

6. **P2 — Documentation makes adoption harder to evaluate accurately.** The changelog promises vectored methods on all four extension traits, but `AsyncUnixDatagramExt` has neither vectored method. README installation examples still select 0.4. Its comparison omits rustix, despite a real consumer choosing rustix, and incorrectly calls std's nightly ancillary API OwnedFd-based: current [std ScmRights](https://doc.rust-lang.org/std/os/unix/net/struct.ScmRights.html) yields `RawFd`. The sandbox example logs a read-only capability while sending a writable tempfile; it runs in one process and enforces no sandbox. The example's single-process limitation is documented, but its capability wording still overstates what it grants. Narrow platform and leak guarantees to verified behavior. Cross-target documentation builds establish compilation, not kernel behavior.

The registry snapshot is in [registry-snapshot.json](2026-09-05-evidence/registry-snapshot.json). Values were fetched directly from crates.io, including `/reverse_dependencies?page=1&per_page=100`:

| Crate | Lifetime downloads | Recent downloads | Published reverse dependencies |
|---|---:|---:|---:|
| [unix-ancillary](https://crates.io/api/v1/crates/unix-ancillary) | 1,670 | 1,601 | 1 |
| [sendfd](https://crates.io/api/v1/crates/sendfd) | 15,378,189 | 2,596,197 | 14 |
| [uds](https://crates.io/api/v1/crates/uds) | 9,771,253 | 1,461,189 | 10 |
| [tokio-seqpacket](https://crates.io/api/v1/crates/tokio-seqpacket) | 4,413,988 | 1,254,693 | 4 |
| [ipc-channel](https://crates.io/api/v1/crates/ipc-channel) | 6,069,152 | 937,801 | 54 |
| [fd-queue](https://crates.io/api/v1/crates/fd-queue) | 14,150 | 148 | 1 |

“Recent downloads” is crates.io's 90-day measure, as described by its [ranking RFC](https://rust-lang.github.io/rfcs/1824-crates.io-default-ranking.html). It was 246 in the August 17 ledger and is now 1,601, roughly 6.5 times higher. From the [daily endpoint](https://crates.io/api/v1/crates/unix-ancillary/downloads), including `meta.extra_downloads`, August 29–September 4 totaled 1,114 downloads versus 45 for August 22–28. September 5 was excluded from those windows because it is incomplete. Daily and summary endpoints did not reconcile exactly; preserve their separately observed values rather than forcing agreement.

0.4.0 accounts for 1,209 lifetime downloads; 0.5.0, published September 3, has 19. The new release is only two days old, so that is too early to call a rejection. Downloads do not identify unique people, uncached CI jobs, or production installations. The rise could include downstream builds, evaluation, or automation; the public counters cannot attribute it. The competitor comparison demonstrates that a few dependents can accompany substantial download volume, not that each additional dependent causes a predictable number of downloads. `fd-queue` is a useful counterexample: it also has one dependent and little recent volume.

The [one published dependent](https://crates.io/api/v1/crates/unix-ancillary/reverse_dependencies?page=1&per_page=100) is `runner-systemd-nspawn` 0.1.4, requiring `^0.4.0`, with the tokio feature enabled. Inspection of its published source found blocking `UnixStreamExt` calls in `src/systemd_dbus.rs`: a child enters a namespace, opens the system bus socket, and passes it to the parent with `send_fds`; the parent calls `recv_fds::<1>()`. Its observed code does not use the defective public low-level buffer API. Do not claim that every existing downstream triggers the audit bugs. Also distinguish a published binary dependent from a library that propagates this crate into other published dependency graphs.

Gnosis is a former consumer, not a fresh version-bump opportunity. [PR #771](https://github.com/gnosis/gnosis_vpn-client/pull/771), merged September 1, explicitly replaced unix-ancillary with rustix. The implementation commit explains that rustix was already present for diagnostics and supplied safe SCM_RIGHTS operations. This is direct evidence of dependency consolidation. It does not say the removal was caused by the safety findings in this audit. The user has already raised the subject in the PR; no repeat outreach was made.

Gnosis app's [current lockfile](https://github.com/gnosis/gnosis_vpn-app/blob/6579e6ee05f5661fb573ccf517bfdbfa0e4f171e/src-tauri/Cargo.lock) still contains 0.3.0 because it pins client commit `53cb1175`, predating removal. That [historical integration](https://github.com/gnosis/gnosis_vpn-client/blob/53cb11757a1871dd182efaafb65fe555e6639866/gnosis_vpn-lib/src/socket/fd_passing.rs) uses `UnixStream` with a one-byte marker, not datagrams. The old handoff's proposed datagram-truncation pitch was inapplicable. Its use of `recv_fds_into::<2>` to reject anything other than one descriptor does reveal demand for a strict-count convenience operation, but Gnosis's decision shows that convenience was not sufficient to retain this dependency.

My recommended order for adoption work:

1. Repair the safety findings, restore the fuzz gate, and validate the corrected release. Make the low-level migration explicit if signatures must change. Preserve simple high-level call shapes where possible. A safety wrapper needs credible ownership guarantees before its convenience is persuasive.
2. Position it as a small stable-Rust convenience API for std/Tokio Unix sockets, with owned received FDs, explicit count/truncation policies, and complete-send handling. Compare honestly with rustix and uds. Projects already using rustix need a concrete reduction in their own tested code to justify an extra dependency; many should simply keep rustix.
3. Retain runner-systemd-nspawn by testing its actual integration against the repaired version. A version-only bump is less valuable than preserving its behavior and documenting any applicable benefit. The local `dep_api_check` is a useful call-shape guard, but not execution of the downstream program.
4. Supply a real parent/child example and a short sendfd/raw-libc migration guide. Include descriptor cleanup on early error, exact-count rejection, CLOEXEC behavior, and stream/cancellation limitations. Correct installation snippets and capability wording. These reduce work a maintainer must do before accepting the crate.
5. Seek one additional independently maintained published library with an existing FD-passing need. Qualify candidates by actual imports and transport requirements before preparing patches. sendfd's reverse-dependency list contains relevant SSH, proxy, and process-isolation domains, but this audit did not establish that any named project is willing to migrate. Track accepted integrations, released dependency versions, and retention after a month; use downloads as supporting evidence. Do not add FdChannel, seqpacket, or other roadmap scope without a named requirement.

Verification completed:

- Docker `rust:latest`, Rust 1.97.1: `cargo build --all-targets --all-features`, `cargo test`, `cargo test --all-features`, and `cargo doc --all-features --no-deps` passed. The initial canonical run stopped at clippy because the image lacked the component.
- After `rustup component add clippy rustfmt` in another disposable container: `cargo clippy --all-targets --all-features -- -D warnings`, `cargo fmt --all -- --check`, and all four canonical example commands passed.
- `cargo check --manifest-path fuzz/Cargo.toml`: failed E0004 as described above.
- Docker `rust:1.75`: `cargo build --all-targets`, `cargo test`, and `cargo check --all-features` passed with the repository lockfile.
- A separate fresh consumer, with no lockfile and `unix-ancillary = { path = "/work", features = ["tokio"] }`, passed Rust 1.75 `cargo check`, resolving libc 0.2.189, mio 1.2.3, and tokio 1.53.1. No current MSRV adoption failure was reproduced.
- First diagnostic run: `cargo test --test audit_regressions -- --test-threads=1 --nocapture` produced five failed safety assertions and one passing high-level control. After adding the lifetime diagnostic, `cargo test --test audit_regressions send_buffer_must_retain_descriptor_lifetime -- --exact --nocapture` produced one additional failure. The final seven-case fixture was not redundantly rerun as a whole.
- No macOS/BSD kernel execution, Miri run, sustained fuzz campaign, performance benchmark, or full downstream build was performed in this audit. Linux success does not validate every platform claim.

Full logs and the safe-caller diagnostic fixture are in [2026-09-05-evidence](2026-09-05-evidence/). Reproduce the final fixture without modifying the checkout:

```bash
docker run --rm --network=bridge \
  -v /home/noman/personal-projects/unix-ancillary:/src:ro \
  rust:latest /bin/bash -c '
    set -eu
    ulimit -c 0
    cp -a /src /work
    cd /work
    cp docs/audits/2026-09-05-evidence/audit_regressions.rs tests/audit_regressions.rs
    cargo test --test audit_regressions -- --test-threads=1 --nocapture
  '
```

The assertions describe expected safety; failure is the finding. Run this diagnostic only in a disposable process/container: it intentionally exposes ownership defects and leaks a small number of FDs that close on process exit. No fixes are claimed by this report.
