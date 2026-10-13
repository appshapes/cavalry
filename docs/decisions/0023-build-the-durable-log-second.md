# 0023. Build the durable log and replay second, in four features

- **Status:** Accepted
- **Date:** 2026-10-13
- **Invariants affected:** 1, 3, 4, 5, 6
- **Supersedes:** none; ADR 0007's Revisit expected UDP here, and this ADR is the
  new one it asked for

## Context

The first slice is done: ADR 0007's Revisit fired when `just quickstart` passed on both
runners (commit 2a3d9cb, 2026-10-13), and the client rows joined the latency gate on the
bench host (ADR 0016, ADR 0022). ADR 0007 named UDP as the second slice, as the thing
that "adds to" the shared-memory path with the fewest new parts. Three facts, weighed on
2026-10-13, put the durable log before it, and Rjae decided so that day.

Invariant 1 ranks first, and its gate is the one still unbuilt: `just replay-check`,
"replays a recorded log twice and diffs state", prints `not built yet`, while every other
gate of `CLAUDE.md`'s table is real. `docs/explanation/why-cavalry.md` says the product
is above the wire: "Archive plus Cluster is the real product", and "the
deterministic-replay guarantee (same log in, same state out) is the headline promise". The
on-disk format cannot be undone, which `CLAUDE.md` says to stop and write up rather than
choose; it is best decided while the data path is one transport on one host. And UDP
cannot be gated yet: ADR 0016's Revisit needs a second dedicated host for a sender and a
receiver, the one candidate looked at on 2026-10-13 (`bar`, card 90) is a shared machine
on Wi-Fi, and the dedicated host is card 91, which does not exist; the durable log is
gated on the hosts that exist.

What the crate already promises, in `crates/cavalry-log/README.md` since ADR 0003: a
stream recorded to disk as an ordered, replayable log, written by the driver; replay
back into a subscription from a position, at original or maximum speed; `cavalry
replay` answering "what did this service see between two instants" in one command; the
same state from the same log, byte for byte; an on-disk format changed only with an
accepted ADR and a version in the file header; no lost, reordered or prematurely reported
byte; a bound on everything; and no allocation, lock, syscall, await or panic per
message.

What Aeron Archive does, read from its Java source on 2026-10-13 (`Archive.java`,
`Catalog.java`, `RecordingWriter.java`, `ReplaySession.java`) and the wiki page: a
recording is one image of a subscription, identified by a recording id; its data is the
log buffer's raw term bytes, frame headers and padding included, written in blocks into
segment files named `<recordingId>-<segmentBasePosition>.rec` of 128 MiB by default, so
that a recorded position is the stream's position; a catalog file, `archive.catalog`,
holds a header with a version and the recording descriptors (recording id, start and
stop timestamps, start and stop positions, initial term id, segment file length, term
length, MTU, session id, stream id, the channels and the source identity), each
descriptor with a length, a state and a checksum; the recording's progress is the
`rec-pos` counter, which the publisher's limit respects; replay takes a recording id, a
position and a length, reads blocks, rewrites only the session and stream ids of each
frame, re-emits padding, and paces on the publication's window, following a live
recording when the length runs past its stop position; file sync levels 0, 1 and 2, 0 by
default; optional checksums stored in each frame's session-id field; truncate and purge
at segment granularity; replay-merge for a late joiner. Its control plane is a protocol
of its own because the archive is a process of its own.

## Decision

The second slice is the durable log and replay on one host over `cavalry:ipc`: the
driver records a stream to disk and replays it into a subscription, with Aeron Archive's
names and shapes where the concept matches, inside the one driver process (`CLAUDE.md`:
one daemon per host; the crate README: written by the driver). Four features make the
slice, one Trello card each, in this order:

1. **The on-disk format, specified first (card 92).** `spec/durable-log.md` version 0
   with its vectors: the catalog, the segment files and the time index that resolves an
   instant to a position. Its ADR lands Proposed, and Rjae accepts it, because the format
   cannot be undone. `cavalry-log` gains the layout types and the catalog reader and
   writer. It builds the durable log's conformance vectors and a no-panic test over the
   catalog parser.
2. **Recording in the driver (card 93).** `start recording` and `stop recording` in the
   client protocol and the client API, `cavalry record`, and the recorder: a dedicated
   thread with an idle strategy that reads the log buffer as a subscriber does, holds a
   `recording.position` the publisher's limit respects, writes blocks into segments, and
   reports what is written and what is synced as two positions, never a position before
   it is true. It needs the configuration key `log.dir`, because the driver directory is
   tmpfs by design (ADR 0020) and a durable log must outlive a reboot; every key the
   slice adds has its `why_not_default` and doctor check and is approved by Rjae before
   it exists. It builds the allocation assertion over the recorder's block path, the
   recording counters and the doctor checks for a stalled recorder, a full disk and a
   log directory on tmpfs.
3. **Replay and the gate (card 94).** `start replay` and `stop replay`, the client API's
   replay session read through an ordinary subscription, `cavalry replay` with positions
   or instants, and the replay session in the driver: blocks from the segments into a
   publication, headers as recorded but for the session and stream ids, padding as
   padding, paced by the subscriber's window. It builds `just replay-check`: record,
   replay twice into a subscriber that hashes every delivered byte in order, and fail
   unless both replays equal each other and the live stream, on both runners.
4. **Retention and the ten-minute path (card 95).** Truncate and purge with their
   catalog states, a bound and a documented behaviour when full for every buffer and
   file set on the recording path, and the README's ten-minute path recording and
   replaying under `just quickstart`: the slice's acceptance test.

Out of the slice: UDP, which is the third slice once card 91's host passes
`bench/host/check.sh`; Raft; replay-merge to a live stream; the C ABI and the language
clients; OTel export; encryption. Two decisions are left to the feature ADRs that must
carry their evidence: per-frame checksums and where they live (card 92) and the sync
cadence behind `recording.synced_position` (card 93). Until a feature's commit can be
measured with recording on, a commit under a data-plane crate says what the bench rows
show with the recorder idle.

## Alternatives considered

- **UDP second, as ADR 0007's Revisit expected.** Lost for now: no second host exists
  to gate it (card 91), and invariant 1's gate is the one still unbuilt. It is third.
- **Raft second.** Lost: it sits on the durable log and on a stream, as ADR 0007 said of
  both; nothing of it can be gated before replay is deterministic.
- **A separate archive process, as Aeron's.** Lost for this slice: the crate README
  says the driver writes the log, `CLAUDE.md` says one daemon per host, and a second
  process needs a control protocol of its own. It returns as a Revisit if the recorder's
  disk writes show in the bench rows.
- **A database for the catalog.** Lost: a dependency in a data-plane crate for an index
  of a few hundred records, and a format no other implementation could read from a page.
  The catalog is a versioned flat file with vectors, like every other layout.
- **Recording and replay in one feature.** Lost: the recorder alone gives `cavalry
  record`, the counters and the doctor checks, half the size, and the format decision
  waits on Rjae in between.
- **Deciding the format inside the slice ADR.** Lost: the format is the irreversible
  decision, and `CLAUDE.md` says to write it up and stop; it gets its own ADR with
  vectors and Rjae's acceptance.

## Evidence

None measured: a scope decision. What informed it: Rjae's decision of 2026-10-13; the
state of `master` at 4ae5c92, every gate real but `just replay-check`; the crate README's
promises; Aeron Archive's source and wiki page, read on 2026-10-13; cards 90 and 91 on
the second host.

## Consequences

Easier: the headline promise gets its gate; a stranger reads what is next from cards 92
to 95 and this page; the format is decided with one transport in play.

Harder: the format ADR waits on a person; the driver gains a thread and disk writes near
the data plane, which the bench must measure with recording on before the slice ends;
UDP waits on a host that does not exist.

Revisit: when card 95 closes, with `just quickstart` recording and replaying on both
runners, with a new ADR for the third slice, UDP, once card 91's host passes
`bench/host/check.sh`; when the recorder's writes move a bench row, for the separate
process; when a user needs replay-merge or an original-speed replay.

## References

- `CLAUDE.md`: invariants 1, 3, 4, 5 and 6; Gates; When in doubt
- ADR 0007, ADR 0013, ADR 0016, ADR 0019, ADR 0020, ADR 0022
- `crates/cavalry-log/README.md`; `docs/explanation/why-cavalry.md`;
  `docs/how-to/implement-a-feature.md`
- Aeron, `aeron-archive/src/main/java/io/aeron/archive/{Archive,Catalog,
  RecordingWriter,ReplaySession}.java` and the wiki page "Aeron Archive"
  (<https://github.com/aeron-io/aeron/wiki/Aeron-Archive>), read 2026-10-13
- AppShapes Trello cards 90, 91, 92, 93, 94 and 95
