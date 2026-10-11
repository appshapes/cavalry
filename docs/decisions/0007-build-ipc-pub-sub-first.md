# 0007. Build publish/subscribe over shared memory first, in five features

- **Status:** Accepted
- **Date:** 2026-10-11
- **Invariants affected:** 1, 4, 5, 6
- **Supersedes:** none

## Context

The repository has its invariants, four of its twelve gates, an agent loop and nine
empty crates (ADR 0003 to ADR 0006). The promise in `CLAUDE.md` spans IPC and UDP
transports, a durable log, Raft replication, a C ABI and clients in five languages,
and none of it exists. The first slice decides what works first, and in which order
the features inside it land, so that every later feature argues from one page instead
of re-opening the question.

Three facts fix most of the order. In Aeron, a message over shared memory is a data
frame written to the log buffer, with the same header the UDP sender puts on the wire
from the same layout (Aeron wiki, Transport Protocol Specification, "Aeron Over SHM"
and "Data Frame"); the frame header therefore precedes the log buffer even while no
byte goes on a wire. Invariant 4 says every state transition
increments a counter in the counters file, so the counters file precedes the first
code with a state transition. Invariant 5 says `cavalry` with no arguments works on a
laptop and `just quickstart` runs the README's ten-minute path, so the slice is not
done until that path exists and passes. A gate that prints `not built yet` is built by
the first feature that needs it (`CLAUDE.md`, Gates), which makes the first feature in
each area larger than the second.

Card 78 asks whether ZigBolt's ring buffer design should inform Cavalry's. Its
verdict lands before the log buffer is specified.

## Decision

The first slice is publish/subscribe over shared memory between processes on one
host: one driver, Rust clients, the `cavalry:ipc` channel, a publication that writes
by claim and commit or by offer, a subscription that reads through a fragment
handler, back-pressure returned to the caller, a counter for every transition,
`cavalry doctor` reading the counters, and the README's ten-minute path run by
`just quickstart` on Linux and macOS as the acceptance test. Out of the slice: UDP,
the durable log, Raft, the C ABI and the language clients, OTel export, discovery and
encryption. Five features make the slice, one Trello card each, in this order: the
data frame header in `cavalry-proto` (card 80); the counters file in `cavalry-shm`
with `counters.toml`, its generated reference page and `just lint-counters`; the log
buffer with its terms, claim and commit, and the lock-free queues in `cavalry-shm`,
after card 78; the driver skeleton in `cavalry-driver` and the `cavalry` binary, with
`cavalry run`, the conductor, the idle strategies and the first `cavalry doctor`
checks; and the client API in `cavalry-client`, re-exported by `cavalry`, with the
ten-minute path, `just quickstart`, `just bench` and `bench/baseline.json`. Each
feature builds the gates it is the first to need: conformance and fuzzing with the
frame header; `lint-counters` and the reference generator with the counters file;
`loom` with the log buffer; `doctor` and `lint-config` with the driver; `quickstart`,
`bench` and `bench-compare` with the client API. Until `just bench` exists, a commit
under a data-plane crate says so in its message instead of quoting a number. The
frame header goes before the counters file, which does not depend on it, because it
is the smallest spec-first change and so the worked example of
`docs/how-to/implement-a-feature.md`.

## Alternatives considered

- **UDP first.** Lost: it needs everything the IPC path needs, plus sockets, setup
  frames, NAKs, status messages and flow control, and a ten-minute path over UDP on
  one laptop is a loopback demonstration. Aeron's UDP path reuses the log buffer and
  the frames of its IPC path, so IPC proves those with the fewest moving parts and
  UDP adds to them.
- **The durable log or Raft first.** `docs/explanation/why-cavalry.md` says the
  opening is everything above the wire. Lost: both sit on a stream, and no stream
  exists.
- **Language clients in the slice.** Lost: the C ABI cannot be undone (`CLAUDE.md`,
  When in doubt), so it follows a client API that has been used, not precedes it.
- **The counters file as the worked example.** Lost narrowly: it is as independent
  as the frame header and more foundational, but it carries `unsafe`, a memory map, a
  generator and a lint, and the example a contributor reads first should be readable
  in ten minutes. It is second.
- **The log buffer first, after card 78.** Lost: the hardest feature first, with
  `loom` to build and the research as a dependency.
- **No slice decision.** Lost: every feature would re-argue scope, and a stranger
  could not tell what is next from the repository (invariant 6).
- **One milestone card for the slice.** Lost: a card per feature gives every commit
  a prefix that names its feature, and a card closes when its feature is on
  `master`.

## Evidence

None measured: a scope decision. What informed it: Aeron's shared-memory path being
data frames in the log buffer, the frames its UDP path sends (Aeron wiki, Transport
Protocol Specification, read on 2026-10-11); the state of the repository on 2026-10-11, nine
empty crates and four real gates out of twelve; and the size of each candidate first
feature, counted in the gates it must build.

## Consequences

Easier: a contributor reads what is next from the Cavalry list and this page; each
commit's prefix names its feature; the first release's contents are known.

Harder: nothing a user can run until the fifth feature; the first four data-plane
commits cite a bench that does not exist; the slice leaves out, for one release, what
`why-cavalry.md` calls the product.

Revisit: when `just quickstart` passes on both runners, with a new ADR for the second
slice, UDP; if card 78's verdict changes the log buffer in a way that changes the
frame layout, which supersedes the frame header's ADR.

## References

- `CLAUDE.md`: invariants 1, 4 and 5; Gates; Protocol; When in doubt
- ADR 0002, ADR 0003, ADR 0006
- `docs/how-to/implement-a-feature.md`, `docs/explanation/why-cavalry.md`
- Aeron wiki, Transport Protocol Specification:
  <https://github.com/aeron-io/aeron/wiki/Transport-Protocol-Specification>
- AppShapes Trello cards 78, 79 and 80
