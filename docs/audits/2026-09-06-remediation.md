# 0.6.0 safety remediation

The user authorized repair of the September 5 audit. The working version is
0.6.0, not published. Public high-level FD operation signatures are preserved;
low-level `messages(&mut self)` drains ownership and `add_fds` retains sender
lifetimes. See [MIGRATION.md](../../MIGRATION.md).

All six original failures are addressed:

- Send buffers do not yield owners; they retain the lifetime of each borrowed FD.
- Received descriptors enter owned storage before fallible CLOEXEC processing.
- Draining messages cannot produce duplicate owners. Buffer clear/reuse/drop and
  dropped iterators close their unread descriptors.
- Headers use bounded unaligned reads and field writes, including offset slices.
- Linux/Android receive capacity includes the automatically generated credentials.
- Fuzz input exercises the byte parser without constructing owning descriptors.

The separate fuzz workspace now compiles in CI. The README corrects the vectored
inventory, comparisons and platform guarantees. The capability example actually
reopens its file read-only and checks that writing fails. A new `multiprocess`
example exchanges a descriptor with a separately spawned child and checks access
mode. CI executes it and the existing downstream API compatibility example.

The tests started red on the old implementation: 10 ownership/alignment tests,
two maximum credentials tests, and a lifetime compile-fail doctest that wrongly
compiled. The read-only assertion also failed on the old example. All pass now.

Verification ran against read-only repository mounts copied inside disposable
Docker containers. Rust 1.97.1 used the local Cargo cache and read-only installed
rustup components to avoid repeated network downloads. Full acceptance was
completed across the recorded runs:

| Command | Result |
|---|---|
| `cargo build --offline --all-targets --all-features` | Passed |
| `cargo test --offline` | Passed |
| `cargo test --offline --all-features` | Passed |
| `cargo test --offline --test ancillary_ownership` | 10 passed |
| `cargo test --offline --doc` | 5 passed, including lifetime and migration examples |
| `cargo doc --offline --all-features --no-deps` | Passed with warnings denied |
| `cargo clippy --offline --all-targets --all-features -- -D warnings` | Passed after removing two unnecessary test Vec allocations |
| `cargo fmt --all -- --check` | Passed |
| `cargo run --offline --quiet --example pass_fd` | Passed |
| `cargo run --offline --quiet --example privsep` | Passed |
| `cargo run --offline --quiet --example async_pass_fd --features tokio` | Passed |
| `cargo run --offline --quiet --example agent_sandbox --features tokio` | Passed, writes denied |
| `cargo run --offline --quiet --example multiprocess` | Passed, distinct parent/child processes, writes denied |
| `cargo run --offline --quiet --example dep_api_check` | Passed |
| `cargo check --offline --manifest-path fuzz/Cargo.toml --locked` | Passed |
| `cargo check --offline --target x86_64-apple-darwin --all-targets --all-features` | Passed with warnings denied |
| Rust 1.75: `cargo build --offline --all-targets` | Passed |
| Rust 1.75: `cargo test --offline` | Passed |
| Rust 1.75: `cargo check --offline --all-features` | Passed |
| `cargo publish --dry-run --allow-dirty` | Packaged and verified 28 files; upload explicitly aborted as dry run |

Logs are in [2026-09-06-evidence](2026-09-06-evidence/). `acceptance.log` ends
at the test-only Clippy failures; `final-checks.log` records their correction
and the remaining checks, ending at an offline registry request from publish.
`publish-dry-run.log` records the successful online dry run. MSRV initially
needed its older Cargo cache layout populated; `msrv-cached.log` records the
successful compiler/test run.

There was no macOS/BSD kernel execution or sustained instrumented fuzz campaign
in this session. CI has not run on the new commit until it is pushed. No release,
external PR, or downstream message was sent. This is not a claim of formal proof
or that every possible kernel control-message type is supported.

## Adoption additions considered

The user also asked whether to add urgently needed features. Public evidence
checked September 6 shows requests for features this crate already supplies:

- [uds #27](https://github.com/tormol/uds/issues/27): owned received descriptors.
- [sendfd #16](https://github.com/standard-ai/sendfd/issues/16): default CLOEXEC.
- [sendfd #14](https://github.com/standard-ai/sendfd/issues/14): safe handling of surplus FDs.

Promising candidates, not implemented or validated as adoption commitments:

1. A high-level receive returning payload, FDs and credentials together. The
   existing low-level API supports this, while convenience methods discard
   credentials. A broker integration should establish the missing-credential,
   platform and rejection semantics before adding the API.
2. Runtime FD-count policies alongside const-generic convenience methods. A
   protocol can learn its FD count from a decoded message, but a single receive
   still does not constitute an entire stream frame. The
   [jsonrpc-fdpass protocol](https://github.com/bootc-dev/jsonrpc-fdpass) provides
   evidence of runtime counts and FD queues, not a request to adopt this crate.
3. Async datagram vectored parity if a caller uses separate header/body buffers.
   This is a concrete API gap but not yet evidence of an urgent downstream need.

These remain proposals. The verified 0.6.0 changes were not expanded while
answering the user's feature question. Framing, seqpacket, reusable-buffer
frameworks and new credential platforms remain outside this repair.
