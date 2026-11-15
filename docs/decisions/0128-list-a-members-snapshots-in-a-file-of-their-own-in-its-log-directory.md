# 0128. List a member's snapshots in a file of their own in its log directory

- **Status:** Accepted
- **Date:** 2026-11-15
- **Invariants affected:** 1, 3, 6
- **Supersedes:** none. Decides the first change of work item 230, and answers ADR
  0052's question of "the snapshot's shape in the recording log" and ADR 0060's
  Revisit on a type field for a snapshot's entry. Moves `spec/durable-log.md` to
  version 4: a row of "Files", a new section, "Snapshots", a paragraph of "The
  driver", and the file's tables in "What a reader rejects" and "Vectors". No
  other file's format changes, no message's layout and no function of the C ABI.

## Context

A cluster member reads its log from position 0 at every start: its consensus
module reads every entry for its sessions, and a service beside it reads every
entry for its state (`spec/cluster.md`, "A member's start"; ADR 0108, ADR 0116). So
a start takes as long as the log is long, the log can never be made shorter, and a
member whose log is in more than 1,024 recordings does not start at all. A snapshot
ends all three: the state at one position of the log, kept so that a start reads
the log after that position only. It is the second half of ADR 0052's sixth
feature, which ADR 0077 left to work item 230 with the standby.

A snapshot is a recording, as it is in Aeron (`spec/cluster.md`, "The snapshot
marker"). The catalog of a log directory holds every recording there: the
recordings of the member's log, a recording of a replay, the recordings of a
program's own streams, and a snapshot whose writer stopped half way. The catalog
does not say that a recording is a snapshot, whose state it holds or at which
position of the log. A start needs all three, from a record that survives what the
log survives, and it needs to know that the snapshot is whole.

Aeron keeps that record in its recording log, `recording.log`, one file whose
entries are of two kinds, a term or a snapshot: recording id, leadership term id,
term base log position, log position, timestamp, service id and entry type, with
one snapshot entry for each service and one for the consensus module, whose
service id is −1 (read on 2026-11-14 at `master`, in `RecordingLog.java`). Cavalry
has no such file. Its terms are in the terms file, whose entry has no type (ADR
0060), and the recordings of its log are in the log recordings file (ADR 0108).

This decision is where a snapshot is listed and in what shape, and nothing more.
What takes a snapshot and when, what a snapshot holds, how a service's reaches its
member, and how a member starts from one are the next decisions of work item 230,
each with the change that builds it.

## Decision

A member lists its snapshots in a file of their own, `snapshots`, in its log
directory (`spec/durable-log.md`, version 4, "Snapshots"). A snapshot is one a
start may begin from once the file lists it, and never before. The file is a
64-byte header, with the magic `CVLYSNAP`, the version, the slot length and the
member's id, and two slots of 3,088 bytes, 6,240 bytes in all. A slot holds the
whole list under a checksum and a sequence. A write puts the list into the slot it
is not read from and syncs once, so a write that a crash tears reads as the list
before it: the log recordings file's design, with its checks, in their order. The
list names at most 64 snapshots, the newest, in the order they were listed. A
sixty-fifth leaves out the oldest, whose recording stays in the catalog. An entry is
48 bytes, the fields of Aeron's snapshot entry in its order and without its type:
the recording id, the leadership term id, the term base log position, the log
position, the timestamp and the service id, −1 for the consensus module's own
state. The entries of one service id rise in position, and their terms never fall.
Two service ids have no order between them. The consensus module is the file's
only writer. `cavalry-log` reads and writes it, `snapshots::SnapshotsFile`: `open`
creates the file when there is none, `list` adds one snapshot and syncs, and
`newest` gives the last entry of a service id. No member writes the file yet.

## Alternatives considered

- **A type field in the terms file's entry, as Aeron's recording log has (ADR
  0060's Revisit).** Lost, for three reasons. The entry is 32 bytes with no room
  for five more fields, so the entry length changes and with it the file's
  version. A member reads the terms file at every start and refuses a version it
  does not know, so a log directory that one start of a new version wrote could
  not be started by the version before: no way back from an upgrade, and no mixed
  versions on one directory (invariant 3). A file a reader does not name is
  ignored (`spec/durable-log.md`, "Files"), so the version before starts on a
  directory that holds `snapshots` as it did before. Second, every rule of the
  terms file is a term's: an entry's term is above the entry before it, and a
  truncation deletes the entries above a position. A snapshot's entry obeys
  neither, and each of the file's readers would have to pass over it. Third, the
  terms file only grows, to a bound of 1,048,576 entries at which the member
  refuses and the operator starts a new directory. A snapshot is taken far more
  often than a term begins, and would spend that bound.
- **A file of fixed entries that only grows, with a state word, as the terms file
  is.** Lost. A list that only grows needs a bound, and at the bound the member
  refuses to list a snapshot or the operator replaces the directory. Neither is a
  behaviour to give a file that is written on a cadence for as long as the cluster
  runs. The two-slot list has one length for ever and a stated behaviour when it
  is full: it forgets the oldest. It is also the shape that can drop an entry,
  which a later change needs when a snapshot's recording is purged.
- **No list: find the snapshots in the catalog.** Lost. A descriptor holds a
  channel, a stream id and positions of the snapshot's own stream. It does not
  hold the log position, the term or whose state it is, so a start would replay
  the first frame of every candidate, and replay each to its end to learn that it
  is whole. A snapshot whose writer stopped after its last frame and before it
  was told that the recording had synced would look whole. The list is the point
  at which a snapshot counts: one write, synced, after the recording is whole.
- **The list in the driver directory, as the file a member writes for its service
  is (ADR 0103).** Lost. The driver directory is shared memory and is gone when
  the driver starts. This list must survive what the log survives.
- **One entry for a snapshot of the member and its services together.** Lost. A
  member does not know which services read its log (ADR 0066), and each writes
  its own recording at its own time. Aeron lists one entry for each as well.
- **A larger bound, or none.** Lost. A start needs the newest snapshot of each
  state. The older ones are kept for an operator who must go back from a snapshot
  that proves wrong, and 64 entries are the last 32 snapshots of a member with one
  service, or the last 16 of a member with three. A list of that size is read
  whole at a start in 6,240 bytes. More is a new version of the file, when a
  member with many services needs it.
- **Keep Aeron's entry type in the entry.** Lost. Every entry of this file is a
  snapshot. Aeron's third type, the standby snapshot, names a snapshot that
  another host holds, with that host's archive endpoint; it belongs to a backup
  that runs away from the cluster, which ADR 0052 puts out of the slice. The four
  reserved bytes are written as 0 and never read, so a version that needs a type
  has the room.

## Evidence

- Aeron's snapshot entry and its recording log's shape, read in
  `aeron-cluster/src/main/java/io/aeron/cluster/RecordingLog.java`, and the
  snapshot's messages in `aeron-cluster-codecs.xml`, schema version 18, on
  2026-11-14 at `master`.
- `spec/vectors/durable-log.json` gains 54 vectors, 204 in all, written by the
  reference encoder `spec/vectors/durable-log.py`: 13 of the header with the
  file's length, 31 of one slot and 10 of both slots of a file. They pin each
  check and the order of the checks. `crates/cavalry-log/tests/conformance.rs`
  holds the Rust reader to every one, and holds what the reader took to encode to
  the vector's bytes.
- `crates/cavalry-log/src/snapshots.rs`, seven tests on a real file: a full list
  forgets its oldest snapshot and still names the newest of each service; a new
  file lists none and a listed snapshot is read back, and another member's
  directory is refused; nine snapshots that a reader would refuse are refused as
  the writer's fault, with the file's bytes as they were; a write that a crash
  tore reads as the list before it, and the next write goes over it; a creation
  that a crash tore at each of its steps is written again, and a zero magic over
  a list is damage; a file at the highest sequence takes no write.
- `crates/cavalry-log/tests/no_panic.rs`: 20,000 rounds of random and
  nearly-right headers and slots. None panics, every refusal is a reason the page
  names, over 5,000 slots are taken and over 5,000 refused, and each slot taken
  encodes and reads back to the same entries.
- What listing costs its writer, one write of a slot and one sync, timed on an
  Apple M5 Pro laptop, macOS 26.6.1, by the ignored test `a_listing_timed` of that
  file (`cargo test -p cavalry-log --lib -- --ignored --nocapture
  a_listing_timed`), 500 listings each, two runs with the laptop's load average
  between 6 and 7. Into a list of at most 3 snapshots: p50 3.17 ms and 3.20 ms, p99
  6.37 ms and 4.59 ms, max 7.13 ms and 6.79 ms. Into a full list of 64: p50 3.84 ms
  and 3.83 ms, p99 7.58 ms and 7.10 ms, max 11.02 ms and 9.10 ms. One run with
  another project's test suite on every processor, a load average of 27: p50 6.24
  ms and 6.88 ms for the two lists, p99 12.17 ms and 11.17 ms, max 16.34 ms and
  17.32 ms. On macOS a sync is `F_FULLFSYNC`. No Linux host has timed it. ADR 0108
  measured the log recordings file's write, of the same shape, at the same order.

## Consequences

- The later changes of work item 230 have a durable place to list a snapshot, and
  a rule for when a snapshot counts: when its entry is synced.
- The consensus module's thread pays one synced write for each snapshot it lists,
  the figure above, on the thread that appends and commits. A snapshot is rare
  beside an entry. The change that takes a snapshot measures the whole pause.
- A member's log directory holds a third file of the member's, and
  `cavalry-log` holds a second two-slot list. The two share their design and not
  their code: their entries and their checks differ, and the first is pinned by
  an Accepted decision's tests. Revisit when a third such list is wanted, for one
  codec under the three.
- A reader of the file does not check an entry's recording against the catalog.
  Whether the catalog still holds it, stopped and whole, is the check of the
  member that starts from it, in the change that builds that start.
- A snapshot that the list forgets stays in the catalog, and so does one that was
  never listed. Nothing purges them: retention is an operator's commands (ADR
  0031). Revisit when a policy drives those commands.
- No entry can be taken out of the list in this version. Revisit when an operator
  must go back from the newest snapshot of a state, for a command that drops it;
  when a member has more services than 64 entries serve; when a standby needs a
  snapshot that another host holds, for Aeron's standby snapshot entry; and when
  a Linux host has timed the sync.

## References

- Work item 230 on the Brigade Board, "Catch a member up from a peer's recording,
  snapshot the service and follow as a standby".
- `spec/durable-log.md`, version 4: "Files", "Snapshots", "The driver", "What a
  reader rejects", "Vectors"; `spec/vectors/durable-log.py` and `.json`.
- `crates/cavalry-log/src/snapshots.rs`, `tests/conformance.rs`,
  `tests/no_panic.rs`, `README.md`.
- [ADR 0052](0052-build-raft-on-the-durable-log-fifth.md), the slice and its sixth
  feature; [ADR 0077](0077-elect-the-leader-by-vote-and-close-a-ballot-on-the-members-heard.md),
  which left the snapshot and the standby to work item 230;
  [ADR 0060](0060-replicate-the-log-with-an-appointed-leader.md), the terms file;
  [ADR 0108](0108-list-the-recordings-of-a-members-log-in-its-log-directory-and-start-a-member-on-the-log-it-kept.md),
  the log recordings file, whose design this file takes;
  [ADR 0066](0066-feed-the-committed-log-to-a-service-through-a-replay-of-the-members-recording.md)
  and
  [ADR 0103](0103-list-the-recordings-of-a-members-log-in-its-driver-directory-for-a-service-to-read-in-order.md),
  a service beside a member; [ADR 0031](0031-truncate-purge-and-delete-a-stopped-recording.md),
  retention.
- Aeron, read 2026-11-14 at `master` (1.55.0-SNAPSHOT):
  `aeron-cluster/src/main/java/io/aeron/cluster/RecordingLog.java`,
  `aeron-cluster/src/main/resources/cluster/aeron-cluster-codecs.xml`.
