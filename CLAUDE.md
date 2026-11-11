# Cavalry

Cavalry is a brokerless, low-latency message transport with a durable log and Raft replication, written in Rust. One daemon per host; clients map shared-memory log buffers; IPC and UDP share one API. The promise: Aeron-class latency without Aeron-class expertise.

Aeron (github.com/aeron-io/aeron) is the reference competitor. Where its behaviour is good, copy it and use its names. Where it is hard to operate, that is the product. Invariants come first because they decide conflicts. Everything here is a rule.

## Invariants, in priority order

When two conflict, the higher one wins. Never trade down.

1. **Never lie about delivery.** A stream is ordered. A message is delivered once, or its absence is reported to the subscriber. Replaying the same log produces the same state, byte for byte. A bug here blocks release; nothing else does automatically.
2. **Tail latency is a budget.** The data plane is judged at p99.99 and max, never at mean or p50. Per message on the data plane: no heap allocation, no lock, no syscall, no `await`, no panic path. A measured regression fails CI.
3. **Bounded everything.** Every queue, buffer, map and retry has a fixed bound and a documented behaviour when full. Back-pressure is explicit and returned to the caller, never absorbed by growth. The driver degrades under cgroup CPU throttling, shm pressure or packet loss; it does not fall over. Mixed versions run during an upgrade.
4. **Explainable from counters.** Every state transition increments a counter in the shared-memory counters file. `cavalry doctor` can explain any latency or loss problem from counters alone. Logs and traces live on the control plane and are zero-cost when disabled.
5. **Correct by default.** `cavalry` with no arguments works on a laptop and on a cloud VM. A new configuration key needs a written reason why the default cannot be right, plus a `doctor` check that fires when it is set wrong.
6. **Understandable by a stranger.** A competent engineer who has never seen Aeron, in another time zone, with nobody to ask, can run Cavalry in ten minutes, find why any behaviour happens, and find why any decision was made, from the repository alone. Documentation never overrides the invariants above, but code without its documentation does not merge.

Observability yields to latency. Latency yields to boundedness. Everything yields to correctness. Documentation wins no conflicts and blocks every merge.

## Gates: how each invariant is enforced

This file is context, not enforcement. The gates below are the enforcement. If a gate named here does not exist yet, building it is part of the first feature that needs it. Do not ship around a missing gate.

| Invariant | Gate |
| --- | --- |
| 1 Delivery | `conformance/` runs every client against `spec/`. `just replay-check` replays a recorded log twice and diffs state, and diffs the replays of three cluster members' recordings of one log and what each member's service read of it. Frame parsers are fuzzed. `cavalry-shm` passes `loom` and Miri. |
| 2 Latency | `just bench` writes HdrHistogram p50 / p99 / p99.99 / max to `bench/results/`. CI diffs against `bench/baseline.json` and fails on a p99.99 regression over 5% and over one clock step. An allocation-counting test asserts zero allocations per message. A workspace lint denies `async` and `std::sync::Mutex` in data-plane crates. |
| 3 Bounded | Every collection in the driver is built with explicit capacity and has a test at capacity. Fault-injection tests cover loss, reordering, CPU throttling and shm exhaustion. `conformance/upgrade/` runs N and N+1 together. |
| 4 Counters | Counters are generated from `counters.toml`. `just lint-counters` fails on a state transition with no counter. Each known pathology has a `doctor` test. |
| 5 Defaults | `just quickstart` runs the README's ten-minute path end to end in CI, on Linux for every pull request and on Linux and macOS for every commit on `master` (ADR 0073). Config keys live in `config.toml` with a non-empty `why_not_default` field, linted. |
| 6 Understandable | `#![deny(missing_docs, rustdoc::broken_intra_doc_links)]` on every public crate. `just docs` builds rustdoc and the mdBook with warnings denied, runs every code block, and checks links. Reference pages are generated from `spec/`, `counters.toml` and `config.toml`, never hand-written. `just lint-docs` fails a commit that changes a public item, counter, config key or `spec/` without touching `docs/` or a README. A change to `spec/`, the on-disk log format or the C ABI needs an accepted ADR in `docs/decisions/`. |

## Commands

The `justfile` is the single entry point. `cargo` works, but `just` sets the flags.

```
just build          # workspace build, release profile for driver and bench
just test           # unit + integration, excluding bench and conformance
just bench          # latency and throughput harness; prints histograms; writes bench/results/
just bench-compare  # diff latest results against bench/baseline.json (what CI runs)
just conformance    # wire-spec suite against every client in clients/
just replay-check   # deterministic replay test
just quickstart     # the README path, timed
just lint           # clippy with workspace lints, fmt, lint-counters, config lint
just docs           # rustdoc + mdBook, warnings denied, code blocks run, links checked
just lint-docs      # fail if code changed without its docs (what CI runs)
just doctor         # run cavalry doctor against the local driver
just tidy           # retire branches whose work is on master, locally and on origin; `just tidy <branch>` for a named one
```

Run `just bench` before and after any change under a data-plane crate and put both histograms in the commit message. A performance claim without a histogram is not a claim.

## Repository map

```
spec/                 wire protocol, counters and log format specs; read these before crates/
docs/                 mdBook: tutorials/, how-to/, reference/ (generated), explanation/, decisions/ (ADRs), glossary.md
crates/
  cavalry/            the one crate users touch: `cargo add` gives the client API and an embeddable driver; `cargo install` gives the `cavalry` binary (run, doctor, replay, console). ADR 0002
  cavalry-proto/      wire types, zerocopy frames, version negotiation          data plane
  cavalry-shm/        memory-mapped log buffers, ring buffers, counters file    data plane, the unsafe crate
  cavalry-driver/     the daemon: conductor, sender, receiver, idle strategies  data plane
  cavalry-client/     client API implementation, re-exported by `cavalry`      data plane
  cavalry-log/        durable log, recording, replay                            data plane
  cavalry-raft/       replicated state machine on top of cavalry-log            data plane
  cavalry-control/    admin API, discovery, OTel export, config                 control plane, async allowed
  cavalry-ffi/        extern "C" ABI and cbindgen header                        data plane, the other unsafe crate
clients/              thin clients generated over the C ABI: python, node, dotnet, jvm, go
bench/                harness, baseline.json, results/
conformance/          protocol conformance suite and upgrade tests
```

"Data plane" means any code a message passes through between `try_claim` on the publisher and the fragment handler on the subscriber, including the driver's sender and receiver threads. Everything else is control plane. Across the C ABI the boundary has three parts (ADR 0046): the Rust core behind the ABI, every `cavalry-ffi` function a message passes through, holds all five per-message rules; a generated client's own code holds them by its card's check and record; the runtime's side, its trampoline, its argument objects and its own step across the boundary, is outside the rules and their gate, and each client's runner reports its cost.

## Rules

### Data plane

- Dedicated threads, pinned when the OS allows; busy-spin, yield and backoff idle strategies, default backoff; `doctor` recommends busy-spin on isolated cores.
- Lock-free SPSC/MPSC queues, 64-byte alignment on shared state, `#[repr(C)]` layouts with a versioned header. A layout change is a wire-protocol change.
- The per-message checklist is `.claude/rules/data-plane.md`, loaded on demand.

### Unsafe

- `unsafe` is permitted in `cavalry-shm` and `cavalry-ffi` only; a workspace lint denies it elsewhere. Every block carries a `// SAFETY:` comment naming the invariant and where it is established.
- New `unsafe` in `cavalry-shm` needs a `loom` test in the same commit. FFI buffer lifetimes are by convention, so debug builds poison released buffers.

### Protocol

- Spec first. A wire change is a change to `spec/`, a conformance test and a version bump, in that order, before any implementation.
- Unspecified behaviour is a bug in the spec, not a freedom in the code.
- Keep Aeron's vocabulary where the concept matches: publication, subscription, image, term, position, NAK, status message, flow control (max, min, tagged), stream id, session id, channel URI. Do not invent synonyms for Aeron's names.

### Configuration and defaults

- Zero configuration is the primary supported configuration. Tune by detection (`doctor`), not by documentation. Never add a knob to make a benchmark pass.
- Every key has a `why_not_default`, a `doctor` check, and a line in the config reference, or it does not merge.

### Observability

- Counters are the primary instrument: cheaper than a log line, and they survive a crash. OTel metrics and traces are exported by `cavalry-control` from the counters file. Nothing on the data plane calls into `tracing` or `log`.
- Every error message says what the operator should do next.

### Clients and developer experience

- Clients are thin wrappers over `cavalry-ffi`, generated where possible, released together, all passing `conformance/`. Every public API has a doctest. The README's ten-minute path is a test (`just quickstart`), so changing the API means changing the README.

### Documentation

- Docs ship in the same commit as the code they describe. A commit whose message says "docs to follow" gets reverted.
- `docs/` has four kinds of page and they do not mix: tutorial (learn by doing), how-to (one task), reference (generated, exhaustive), explanation (why). A page that is two kinds gets split. Detail in `.claude/rules/docs.md`.
- Write for the reader in invariant 6. Define a term on first use or link it to `docs/glossary.md`, which also maps every Cavalry term to Aeron's.
- Every crate README answers four questions: what this does, what it must never do, a twenty-line example that is also a doctest, and what to read next.
- Decisions live in `docs/decisions/` as numbered ADRs, immutable once accepted and superseded by a new one. A decision made in chat, a call or a commit message does not exist until it is an ADR.
- Every error message and every `doctor` finding names the docs anchor that explains it, so the first search result is ours.

## Working rules for agents

- Read `spec/` before `crates/`. Read the crate's `README.md` before its code.
- Trunk-based by default: developers commit directly to `master`. Bots and agent workflows use branches and pull requests. `master` is green after every commit, so run `just lint` and `just test` before each one; `/commit` does. `gates (macos)` runs for `master`'s commit and not for a pull request (ADR 0073), so whoever lands a change watches `master`'s run and fixes a red one forward before anything else. The commit message is the review record: what changed, which invariant, and the evidence.
- After a pull request merges, run `just tidy`: it retires the branch locally and on origin and removes its worktree, and `just tidy <branch>` retires a working branch that never needed a pull request. GitHub deletes a merged pull request's branch on origin by itself; everything else lingers until this runs.
- One crate per commit where possible. Keep data-plane and control-plane changes in separate commits.
- A changelog entry is a file of its own under `changelog.d/`, never a line appended to `CHANGELOG.md` (ADR 0075). Concurrent branches still collide in the files everyone appends to: `docs/SUMMARY.md`, `docs/glossary.md`, `spec/README.md`, `counters.toml` and a crate's `lib.rs`. Collaborators tell each other which of these they are appending to before they do, append at the end of the section so both sides can be kept, and resolve a conflict in them by keeping both sides with `master`'s lines first, in the branch that merges second. A conflict anywhere else is the session's too: keep what both changes meant, run the gates, and name the files and the resolution in the commit message or the pull request body. No conflict waits on a person; never rebase, force-push or `git merge --abort` to escape one.
- Before claiming a change is faster, run `just bench-compare`. Before claiming it is correct, run `just conformance` and `just replay-check`.
- Do not add a dependency to a data-plane crate without a justification in the commit message. Data-plane crates have a dependency allowlist in `Cargo.toml` metadata; `just lint` checks it.
- Decide, do, and show the evidence; no change waits on a person's approval. A choice between options is the session's to make on its own recommendation, recorded where the work is (the ADR, the card, the commit message), and a `brigade decision`, a question a session records through Brigade (ADR 0004) for a person to answer, goes to a person only for what needs their own hand, a host, a credential, an account, or reverses a decision they made. An ADR lands Accepted on its evidence (`docs/how-to/implement-a-feature.md`, step 2), one on a surface that cannot be undone included, and names no person to accept it; an acceptance that an Accepted ADR already leaves to a person stays theirs. A decision that cannot be undone is an ADR before it is acted on (Documentation, above), and a weakened gate says why in its commit message.
- A new crate or feature starts with its README, its ADR and its example, in that order, before the code. Make the example compile first.
- When the docs failed to tell you something you needed, fix the docs in the same session. That is a bug report with the fix attached.
- Keep this file under 200 lines. Instructions that matter only for part of the tree go in `.claude/rules/` with a `paths:` scope.

## When in doubt

- Copy Aeron's semantics and name.
- Make it bounded first, then fast. Measure at p99.99. Never quote a mean.
- A counter beats a log line. A `doctor` check beats a documentation page.
- If a decision cannot be undone (wire format, on-disk log format, C ABI), write it up as an ADR, then act on it.

# Overrides
- @./.brigade/CLAUDE.brigade.md
