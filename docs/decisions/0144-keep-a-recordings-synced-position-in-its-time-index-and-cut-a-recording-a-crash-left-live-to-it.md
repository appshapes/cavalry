# 0144. Keep a recording's synced position in its time index, and cut a recording a crash left live to it

- **Status:** Accepted
- **Date:** 2026-11-22
- **Invariants affected:** 1, 2, 3, 4, 5, 6
- **Supersedes:** of ADR 0108, its section "What a start does not hold" and the
  consequence that a member whose host lost power stays out by the operator's hand:
  such a member starts on its log directory as after any stop, when the driver it ran
  at the loss was of this version. The operator's rule stays for a host that lost
  power under a driver of an earlier version, whose recording has no synced position. Of ADR 0024 and ADR
  0025, the rule that a recording live at a crash stops at the end of the last frame
  that checks, for a recording whose time index holds a synced position: it stops at
  that position at the most. Of ADR 0065, the stop recovery rebuilds for a recording
  given up after a sync of its own failed: the position last synced, the one the
  driver announced, where ADR 0065 rebuilt what the files held past it; that answers
  its Revisit on whether recovery should sync what it scans, since it keeps nothing
  past a sync. Answers ADR 0089's Revisit. Moves `spec/durable-log.md`
  to version 5; the time index header's version stays 0, and the catalog's data
  checksum kind stays 0.

## Context

After a power loss or a crash of the operating system, the driver's recovery pass
rebuilt the stop of a recording that was being made from what it found in the
recording's files: from the last time index entry at which a frame checks, frame by
frame to the first that fails (`spec/durable-log.md`, version 4, "The driver"). The
recorder syncs a recording's segment and index once a second, and at once for a `sync
recording` command (ADR 0025, ADR 0069). Bytes past the last sync are not guaranteed
after such a crash, and pages can reach the disk in any order. The durable log carries
no checksum beside the frames: the catalog's data checksum kind is 0. So the scan could
not tell a frame whose header survived and whose body did not from a whole one: the
header holds every field the `frame` check reads.

For a cluster member that starts on the log directory it kept (ADR 0108) that meant a
log that could end in frames the member never reported, some with damaged bodies. A
member with the longest log is the likeliest to be elected. As leader it would
replicate those frames, a quorum would commit them, and every service would read bytes
no publisher sent, with no absence reported. A power loss across a whole cluster gives
every member such a tail. ADR 0108 accepted this until work item 287, wrote it on
`spec/cluster.md` as "What a start does not hold", and left the operator a rule: a
member whose host lost power is not started on that directory again. ADR 0089's
Revisit asked that a start's read of its log move behind a check of the recordings if
what a start reads can be above what a power loss left synced. ADR 0138 makes this
work item the first part of feature 3 of the beta, and a condition of the release.

Two facts fix what the change may cost. A member's append position is its recordings'
synced position, never what the recorder has written (`spec/cluster.md`, "The log";
ADR 0060), and a `sync recording`'s answer and `recording.synced_position` name the
same position: nothing a client was told is durable lies past the last sync. And the
directories Brigade's two servers keep were written by the tree at 620a2e29, which
`conformance/upgrade/log-directory.sh` opens in place under this tree in every run of
`just conformance` (ADR 0141): the new format must open those with no step by an
operator.

## Decision

**A synced position in the time index header.** Bytes 24 to 52 of the 128-byte header
of `<recording id>.idx`, reserved until now, hold two synced slots, each a position
(i64), the zlib CRC32 of its 8 bytes and 4 reserved bytes, at 24 and 40. The **synced
position** is the higher of the slots whose checksum matches. A creation writes the
start position into slot 0 and leaves slot 1 zero; each sync writes the other slot,
the one the synced position is not read from. A write a crash tears fails its
checksum and is not read, so the synced position is the one before it, as the terms
file's candidate term is (ADR 0060). A slot whose checksum matches and holds a
position below 0 or not a multiple of 32 is damage, `position`. An index whose slots
both fail has no synced position.

**The recorder's order.** The recorder creates the index before the first segment
file, the start position in slot 0 before the magic. It creates each segment file at
its full length, syncs it, and then syncs the directory, before it writes a byte into
it, so that a power loss keeps the names of the segment and of the index. A sync
syncs the segment file being written, then writes the position into the other slot,
then syncs the index; only then does `recording.synced_position` move. So a synced
position on the disk never names a byte the segment has not kept. A slot's write that
fails is a failed sync (ADR 0065).

**Recovery's cut.** For a recording live at a crash whose index holds a synced
position, the bound is the larger of it and the retained position. The empty
recording, the damage checks, the scan's start and the scan read only segment files
below the bound, and the scan stops at it. When the stop is the bound and the frame
length there is not 0, the recorder had written past what it synced: recovery zeroes
those bytes as a truncate does, one truncation of the segment file to the bound's
offset and an extension to its length again, and syncs the file, before it writes the
stop. It zeroes them whatever it finds there, since a power loss can keep a later page
of whole frames past a page it lost, and a later scan must find none of them. It
counts the recording in `recording.cut_to_synced` (counter 180) when the recorder had
written past what it synced: the frame length at the bound is not 0, or the index's
last entry lies past the bound. A synced slot whose checksum matches and whose position
no writer writes is damage, which no crash leaves: the recording gets state 2 and its
index is left as it is. The rule is the
same after every crash. After a crash of the driver's process alone the bytes past the
sync are whole, and recovery drops them all the same: no start can tell that crash from
a power loss, and they are at most one recording sync interval, 1 s, of frames no
client was told were durable.

**Old directories.** An index with no synced position, which a driver before version 5
wrote, and one recovery writes again, is recovered from its files alone, as version 4
recovered it. A reader of version 4 reads the slots as the reserved bytes they were.
So a directory either version wrote opens under the other in place, and the header's
version stays 0. No checksum is added beside the frames. A recording that a driver of
an earlier version was making at a power loss has no synced position to be cut to,
under any later driver: a cluster member whose host lost power while it ran such a
driver stays out, by the rule ADR 0108 gave, and the how-to says so.

**A member's start.** A member reads where each recording of its log stops from the
catalog, after the driver's recovery. That stop is now at most what the recording had
synced, which is at least what the member reported. So a member whose host lost power
starts on its log directory again as after any stop, with no check of its own: that
answers ADR 0089's Revisit. Its newest snapshot is of a position its log reaches,
since a member lists a snapshot only once its log has synced to it (ADR 0143).

## Alternatives considered

- **A checksum beside the frames, checked by the scan.** This is the work item's first
  option, and in Aeron the archive's optional recording checksum, off by default. Where
  it would live: in the frame's reserved value, which `spec/frame.md` carries from
  publisher to subscriber unchanged as the application's, so a replay would hand back
  other bytes than the publisher sent; or in a file beside each segment, one more write
  for each block and one more file synced apart from the segment, so that a power loss
  can keep either without the other and the scan still needs a rule for the frames
  past the last checksum that survived. Its cost is a CRC over every byte the recorder
  writes, on the recorder's thread: a slicing-by-8 CRC32, the fastest the standard
  library allows with no dependency, took 436 µs at p99.99 for each 1 MiB block on
  `grip` and 537 µs on `bench`, so that a block took 2.5 and 1.8 times as long as its
  write alone ("Evidence"). A recorder that writes 1 MiB in 297 µs would take 731 µs,
  and a publisher held by its recorder (ADR 0084) would wait that much longer. It would buy the unsynced tail after a crash of the
  process alone, at most 1 s of frames no client was told were durable, and a check
  of damage below the sync, at a start only. The synced position holds invariant 1
  without it.
- **Both.** The checksum's cost for the same two gains; the synced position alone
  already decides what a start may hand on.
- **Tell a crash of the process from a restart of the host, and cut only after a
  restart.** Linux names each boot in `/proc/sys/kernel/random/boot_id`; recovery
  would keep the unsynced tail when the index's recorded boot is the running one. macOS
  names it only through `sysctl`, which needs `unsafe`, allowed in `cavalry-shm` and
  `cavalry-ffi` alone, so the rule would differ by platform, and the gates run on both.
  A same boot does not prove the page cache kept the bytes, either: a disk detached or
  failed while the host ran drops writes with no restart. And a recovery that kept the
  tail would have to sync those files before it wrote the stop, or a power loss after
  it would leave a stopped recording whose last frames were never on the disk. Lost:
  a second rule and a platform's case, for the same 1 s of frames.
- **The synced position in the catalog slot.** Every byte of a descriptor from 28 on is
  under its creation checksum, so a writable group needs a third form of the slot whose
  checksum leaves it out, as version 1's form did for the retained group (ADR 0031),
  and every reader of the catalog would try three ranges. Each sync of each recording
  would write and sync the catalog, shared by every recording, beside the index it
  syncs already. The time index is the recording's own file, and its sync is already
  part of every sync.
- **Move the time index header's version to 1.** A reader of version 0, the tree at
  620a2e29 among them, would refuse every index this version writes with `version`,
  and its recovery would write each such index again with its first entry alone: a
  directory that this version wrote would lose its time indexes under a rollback.
  Bytes a version 0 reader never reads need no version.

## Evidence

- The cost on the recorder's thread, measured on the bench hosts by the `bench`
  workflow, run 45013163061 of 2026-11-22, on the throwaway branch
  `287-bench-synced-position`: this change and five rows of `record` that time, with
  21 runs each, the 1 MiB block write `record_block_32`; the same write and a
  slicing-by-8 CRC32 of the block, `record_block_crc32_32`, and the CRC alone,
  `record_crc32_alone_32`; and a `sync_now` after each 64-byte message, alternately
  with the synced slot written, `record_sync_marked_32`, and without it,
  `record_sync_unmarked_32`, as the sync was before this change. The log directory is
  under `/dev/shm`, so a sync there costs its system calls and no device flush; on a
  disk the flush is the same with and without the slot, since the index was synced
  before as well. Nanoseconds, the median run's p50, p99, p99.99 and max:

  | Row, `grip-linux` | p50 | p99 | p99.99 | max |
  | --- | --- | --- | --- | --- |
  | `record_block_32` | 290,815 | 297,215 | 297,215 | 297,215 |
  | `record_block_crc32_32` | 729,087 | 731,135 | 731,135 | 731,135 |
  | `record_crc32_alone_32` | 432,383 | 435,711 | 435,711 | 435,711 |
  | `record_sync_marked_32` | 652 | 747 | 884 | 1,142 |
  | `record_sync_unmarked_32` | 301 | 355 | 410 | 746 |

  | Row, `bench-linux` | p50 | p99 | p99.99 | max |
  | --- | --- | --- | --- | --- |
  | `record_block_32` | 700,415 | 705,535 | 705,535 | 705,535 |
  | `record_block_crc32_32` | 1,233,919 | 1,239,039 | 1,239,039 | 1,239,039 |
  | `record_crc32_alone_32` | 531,455 | 536,575 | 536,575 | 536,575 |
  | `record_sync_marked_32` | 2,000 | 2,141 | 2,751 | 2,931 |
  | `record_sync_unmarked_32` | 900 | 950 | 1,090 | 1,520 |

  The block rows hold 7 blocks each, the most the bench's log and segment allow at
  its term length, so their p99.99 is their max; the sync rows hold 100,000 syncs
  each. The synced slot adds one positional write of 12 bytes to each sync, 474 ns at
  p99.99 on `grip` and 1,661 ns on `bench`, and nothing to a message: a sync comes
  once a second, or once for each `sync recording` command. The checked profiles are
  shown; the unchecked ones of the same run agree within 14 percent on every row.
- `crates/cavalry-log/tests/power_loss.rs`: a recording written by the recorder,
  synced in part, written further and dropped without a stop, then cut as a power loss
  can cut it: a frame past the sync whose body is zero and whose header is whole; a
  page lost between pages that survived; the first page past the sync lost and the
  whole frames after it kept; the unsynced tail whole, as a crash of the process
  leaves it; the tail lost whole, with index entries past it; and a second sync whose
  slot the crash tore. In each, the next open stops the recording at the synced
  position, every frame from the start to it reads back as the recorder wrote it, the
  bytes past it are zero, and the cut is counted. A crash with nothing past the sync,
  and a tail lost with the index entries past it, count none; a crash before the
  first sync leaves an empty recording, not damage, though the frame at its start has
  no body; a synced slot that fails `position` is damage, state 2. The last test holds
  the recorder's alternation of the two slots.
- `spec/vectors/durable-log.json`: eight index header vectors for the slots, the
  creation's, none, the higher slot, slot 0 after slot 1, a torn slot 1, a torn slot
  with no other, and the two `position` refusals, with the order of the checks;
  `crates/cavalry-log/tests/conformance.rs` reads them.
- `crates/cavalry-driver/src/conductor.rs`,
  `a_recording_whose_sync_failed_is_given_up_at_the_position_last_synced`, changed:
  recovery now rebuilds that recording's stop at 640, the position announced, and
  counts the cut, where ADR 0065's evidence had it at 1,280, the position written.
- The tests of `crates/cavalry-log/tests/dir.rs` pass unchanged: they write their time
  indexes by hand with no synced position, which is how version 4's rule is held.
- `conformance/upgrade/log-directory.sh`, in `just conformance`: a directory 620a2e29
  wrote, its second run stopped and then killed, opens in place under this tree, and
  every record it answered reads back at its position. Run on this branch on
  2026-11-22: `conformance/upgrade/log-directory: ok` for both schedules. In the
  killed one this tree's recovery found N's last recording live with an index that
  has no synced position, and kept its 9 records as version 4 would.
- The other way, a directory this version wrote under 620a2e29: `IndexHeader::parse`
  at 620a2e29, read on 2026-11-22, checks the magic, the version and the recording id
  and reads no byte from 24 to 128, so that tree reads an index with synced slots as
  it reads its own, and recovers a recording live in it from its files alone. No run
  holds this direction: `conformance/upgrade/` opens a directory of N under N+1 only.
- `crates/cavalry-raft/src/memory.rs`, `Cluster::restart`, read on 2026-11-22: the
  cluster in memory has played a member's start after a power loss as its log cut to
  what it had synced since ADR 0089, and the cluster's tests over it hold that the
  members elect and lose no committed entry.

## Consequences

- No start hands on a frame past what its recording synced, so no start hands on a
  frame whose body a power loss took, for every recording this version's recorder
  makes. A recording an earlier version was making at a power loss keeps the tail
  that version's recovery would have given it. A cluster member whose host lost power starts
  on its log directory again, and a cluster whose hosts all lost power together
  starts on its directories with every committed entry: the commit position was a
  quorum's synced positions.
- A crash of the driver's process now drops what the recorder wrote since its last
  sync, at most 1 s of a recording, where version 4 kept it. The live subscribers had
  those frames; a replay of the recording does not.
- Each segment file costs one directory sync more when it is created, once per
  128 MiB, and each sync one 12-byte write more. No message pays either.
- `recording.cut_to_synced` counts each cut, and its `doctor` text says what was
  dropped and why nothing a client was told is durable was.
- Revisit if a recording's frames must be checked below the sync, against a disk
  that keeps bytes wrongly: that is the checksum above, with its cost per byte.

## References

- Work item 287 on the Brigade Board, which carries this decision and ADR 0143; work
  item 363, feature 3 of the beta slice (ADR 0138).
- `spec/durable-log.md`, "Time index", "The driver" and "What a reader rejects";
  `spec/cluster.md`, "A member's start".
- [ADR 0108](0108-list-the-recordings-of-a-members-log-in-its-log-directory-and-start-a-member-on-the-log-it-kept.md),
  "What a start does not hold".
- [ADR 0089](0089-start-a-member-that-elects-on-the-log-directory-it-kept.md), its
  Revisit.
- [ADR 0024](0024-lay-out-the-durable-log.md) and
  [ADR 0025](0025-record-a-stream-in-the-driver.md),
  the layout and the recovery pass.
- [ADR 0141](0141-open-in-place-a-log-directory-written-by-the-tree-that-servers-run-and-build-that-tree-on-every-run.md),
  the upgrade gate this change keeps.
- [ADR 0143](0143-list-a-members-snapshot-once-its-log-has-synced-to-the-snapshots-position.md),
  the snapshot that waits for the member's log.
- [Run the bench](../how-to/run-the-bench.md), "Measure a branch".
