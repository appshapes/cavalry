# 0108. List the recordings of a member's log in its log directory, and start a member on the log it kept

- **Status:** Accepted
- **Date:** 2026-11-12
- **Invariants affected:** 1, 3, 6
- **Supersedes:** none. Decides the sixth change of ADR 0100, and what three
  decisions left to it: ADR 0089 ("Over a driver", and its Revisit on a recording
  that was not closed), ADR 0091 (its Revisit on the table of recordings at a
  start) and ADR 0095 (its Revisit on the order of a truncation's steps). Moves
  `spec/durable-log.md` to version 3: a row of "Files", a new section, "Log
  recordings", two paragraphs of "The driver", and the file's tables in "What a
  reader rejects" and "Vectors". Changes `spec/cluster.md`: "The log" names the
  file and the rule of order, "Messages during an election" says where a
  truncation writes it, and "A member's start" is written again. No message's
  layout changes and no function of the C ABI: the cluster page stays at version
  0. Amends ADR 0100 in one clause, for this change alone: the change lands in two
  pull requests, and the two pages are true of a member started with `cavalry run`
  only after the second ("Two pull requests", below).

## Context

A cluster member over a driver does not start again on the log directory it had.
The consensus module refuses a directory whose `terms` file holds a term or a
vote, and says to start the cluster again as a new one (ADR 0063, ADR 0104). The
core has been able to take up a kept log since ADR 0089: it reads its stored
candidate term and its record of terms in its first init. What the module lacks is
the log itself: which recordings in the directory hold it, and where it ends.

**The catalog cannot say which recordings are a member's log.** A member's log is
in several recordings: one for each time it joined a leader's log, one for each
replay it replicated or caught up by, one for each term it led (ADR 0091, ADR
0094). The driver's catalog holds those, and it holds three kinds of recording of
the same stream that are no part of the log: one whose image the election was
never told of, one that began past where the log ended (ADR 0093), and one whose
image joined below the member's append position (ADR 0094). The last kind
overlaps the log. It holds the next leader's entries from a position the member's
log has already passed with entries of its own, which that leader is about to ask
the member to cut. A start that chained the catalog's recordings by position
would carry the log on from the old tail into that recording, and report a log
that no member ever held. In Aeron a member's log is one recording, which the
member extends, so the question does not arise; Cavalry chose a recording for each
join and each replay (ADR 0091, ADR 0094).

**A start must never give a member a shorter log than it reported.** A commit
rests on a quorum that has recorded an entry. Take three members, an entry
committed by the leader and one follower, and a third member behind. If the
follower starts again with a log that ends before the entry, and the leader then
stops, the follower and the third member are a quorum in which no log holds the
entry: one of them is elected and the entry is gone. That is why one member is
never started alone on a new log directory (ADR 0104), and it holds as well for
any start that reads back less than the member had reported.

**A truncation is three steps on two files and the catalog.** ADR 0095 cuts the
recordings first and deletes the terms after, and left to this decision what a
member finds when it stopped between the two.

**A recording can still be live when the member starts.** A driver that loses a
client drops the client's registrations and subscriptions (`remove_client` in
`crates/cavalry-driver/src/conductor.rs`), and a recording ends with its stream,
not with the client that asked for it. A member's module can stop while its driver runs on.

## Decision

**The member lists the recordings of its log in a file in its log directory**,
`log-recordings`, beside the catalog and the `terms` file. The layout is
`spec/durable-log.md`, "Log recordings": a 64-byte header with a magic, a version
and the member's id, and two slots of 16440 bytes, 32944 bytes in all, a length
that never changes. A slot holds one whole list under a checksum: a sequence, a
floor position, and up to 1026 entries, each a recording id and the position the
recording begins at. The list is in the slot with the higher sequence of those
whose checksum matches. A write puts the whole list into the other slot and syncs
once, so a crash tears at most the slot being written and the reader takes the
list as it was. This is the discipline of the candidate term in the `terms` file
(ADR 0060), on a list.

**A recording is part of the member's log once a synced list names it, and the
member reports no position it holds before.** The module writes the list at the
end of the duty cycle in which a recording has come to carry the log on, and the
recording counts toward the append position from the next cycle. A leader's own
recording is listed before `add_log` hands the log over, so before the leader
appends. A recording that is stopped needs no write: the list already names it,
and its end is the catalog's stop position. A list that cannot be written leaves
the recording out of the log; the duty cycle returns the error with what to do
and the next cycle writes again.

**The floor position says where the list begins, and where the log ends when the
list is empty.** A member keeps its last 1024 stopped recordings and forgets the
oldest (ADR 0091); a list names those and the two a member can be making. When a
truncation purges every recording the member kept, its log ends below all of
them, in a recording it forgot, and a list of no recording would read back as a
log that ends at 0. So each list carries one position: where its first recording
begins, and with no recording, where the log ends. A start's chain begins there.

**A truncation writes the list between its two steps**: after the last `purge
recording` and `truncate recording` is answered, and before the first term is
deleted. The module purges from the last recording back. So a member that
stopped inside a truncation finds one of two things, and nothing else: the list
names recordings the catalog no longer holds, the list's last ones; or the list
is right and the `terms` file holds a term based above the end of the log.

**A member's start reads the log back and finishes that truncation.** Before it
reads a message, the module takes each recording the list names from the catalog,
in the list's order from the floor: the recording begins at or below where the
ones before it end, and ends at its stop position. One still being made is
stopped first. The list's last recordings may be missing, and the log then ends
where the ones that are left end, at the floor when none is left. The module
writes the list as it took it, when that is not the list it read, and then
deletes each term based above the end of the log.

**Everything else refuses the start.** The member does not start, and its error
names the recording and says what to do, when the catalog holds in place of a
listed recording: no recording, before one that it does hold; a recording the
driver marked damaged; a recording that begins elsewhere or is of another stream;
or a recording that begins past where the ones before it end. Nor does it start
on a file that fails a check or names another member. Each of these would read
back a shorter log, and a member that stays out is a member the cluster already
tolerates. Every recording is checked before any is stopped, so a start refused
for what a recording is has changed nothing.

**A log with no term refuses the start too.** A member records a term before its
list names a recording of that term, and a truncation that deletes every term has
cut the log to nothing first. So a directory whose log ends above 0 and whose
`terms` file holds no term has lost that file, or had it replaced, and the member's
votes with it. Started on it, the member would report a log of no term, be told to
cut all of it, and vote meanwhile as a member that had never voted.

**No list is created beside a term.** A member whose `terms` file holds a term
holds a log. For such a member the module opens the file and never creates it: no
file is `absent`, and a creation a crash tore is `too-short` or `not-ready`, since
creation ends before a member records its first term. A directory that a version
before this one wrote is in that state. Its member is refused as before, with the
advice it was given before: the cluster runs on with a quorum, and a full count is
a new cluster on new directories.

**What a start does not hold.** After a power loss or a crash of the operating
system, the stop position of a recording that was being made is what the driver's
recovery scan found in its files, which can be above what the recording had
synced. The member's log then holds frames past the last append position it
reported, and the durable log cannot tell one of those whose body the crash took
from a whole one (`spec/durable-log.md`, "The driver": the data checksum kind is
0). A sync that returned success covered everything the member reported; the
frames past it are the path that is not covered. After a crash of the member's
process alone the scan finds every byte the recorder wrote, whole. This answers
ADR 0089's Revisit: the read at a start is behind a check of the recordings, and
that check is as strong as the durable log's own.

**Two pull requests.** A change to `spec/` lands with the Accepted decision behind
it, and an Accepted decision is not edited, so every sentence this decision puts
on the two pages lands with it, in the first pull request: the decision, the
pages, the vectors and the file's reader and writer in `cavalry-log`. The second
holds the consensus module's use of the file, its tests over drivers and the
how-to. Between the two, the pages say what a member will do and `cavalry run`
still refuses a log directory that holds a term: ADR 0100 asks that every
sentence of `spec/cluster.md` be true after each pull request, and this change
keeps that only after its second. The crates' READMEs say, between the two, each
thing the pages state and no code does yet.

`RaftError::Restart` goes. `RaftError::Recordings` carries the file's refusals,
`RaftError::ListedRecording` the catalog's, and `RaftError::TermsLost` the log with
no term.

## Alternatives considered

- **Build the log from the catalog alone.** Lost (invariant 1). Context has the
  recording that overlaps the log's end. The module's tests make one: a recording
  from 0 to 192 in the catalog of a member whose log ends at 96.
- **Fill the `recording id` of each `terms` entry.** Lost. A term can lie in more
  than one recording, a replay's and the live one, and the field is written once,
  by the append (ADR 0060).
- **One file for the member and for its service**, the file of ADR 0103 made
  durable. Lost. A service is not given the log directory, and the two files are
  written at different moments for different readers: that one before a counter
  names a recording, with each recording's stop, renamed into place; this one
  before a position is reported, synced.
- **Write the list under another name and rename it**, as ADR 0103's file is.
  Lost. A rename that survives a power loss needs the directory synced as well as
  the file. Two slots in a file of fixed length need one sync of one file.
- **An append-only file, an entry for each recording.** Lost. The list shrinks at
  a truncation and at the table's bound, which an append-only file would record as
  deletions to be replayed; the whole list is 16 KiB.
- **List a recording when it is stopped.** Lost (invariant 1). A member reports
  positions from a recording while it is being made. A crash would leave reported
  positions in a recording no list names.
- **Skip a listed recording the catalog lacks, wherever it is.** Lost. Nothing
  purges a member's recordings from the front, and a rule for it would turn a
  recording that was removed by hand into a shorter log.
- **Shorten the log at damage and let the leader replicate the rest.** Lost
  (invariant 1), by the three members of Context.
- **Create an empty list for a directory of an earlier version**, or build one
  from its catalog. Lost. The first reads back no log and then deletes the
  member's terms; the second is the first alternative.
- **No floor position: an empty list is an empty log.** Lost (invariant 3). The
  table's bound has a behaviour when full, forgetting, and with it a truncation
  can leave a list of no recording over a log that ends above 0.
- **Raise the list's bound to the catalog's**, as ADR 0091's Revisit foresaw.
  Lost for this version. The list is written whole at each change, and 1048576
  entries are 16 MiB a write. The floor makes forgetting safe at a start, and the
  slot length is in the header for a version that needs more.
- **Store the synced position, and cut the log to it at a start.** Not in this
  decision. The module does not sync the recording, the driver's recorder does,
  and a synced position that survives a power loss is a field of the catalog or a
  checksum beside the frames: a change to the driver's part of the format.

## Evidence

- `crates/cavalry-log/src/recordings.rs`, six tests: a created file and a list
  read back; the floor, with a first recording that does not begin at it refused;
  a torn write read as the list before it; a torn creation written again, and a
  zero magic over a list, two torn slots and a wrong length refused; a member that
  holds a log given no new list, with the file left as it was; a list at the bound
  and one past it.
- `spec/vectors/durable-log.json`, 47 new vectors of 150: 13 of the header, 24 of
  a slot, 10 of the choice between two slots, each refusal in the order the checks
  run. `tests/conformance.rs` holds the crate to them, and `tests/no_panic.rs`
  gives the three parsers 20,000 rounds of bytes each.
- A write of the list, timed on an Apple M5 Pro laptop, macOS 26.6, 500 writes:
  3 entries, p50 3.74 ms, p99 4.77 ms, max 7.99 ms; 1026 entries, p50 4.06 ms,
  p99 5.20 ms, max 10.05 ms. On macOS a sync is `F_FULLFSYNC`.
- The module was built on a branch on 2026-11-12 and run on that laptop. The
  tests below are in the change that builds it.
  - The read at a start, with no driver: the log read back from the recordings the
    list names and not from one the catalog also holds across the log's end; none,
    one, two and all three of the list's last recordings missing, and the log
    ending at 608, 512, 416 and at the floor, 320; five refusals, each naming its
    recording, with nothing stopped; 1026 listed and the last 1024 kept.
  - Over two drivers: a recording told to the election and synced, with an append
    position of 0 until the list is written; a member started again beside a
    recording it never counted; a member stopped after a truncation's first
    command, and one stopped after its write of the list, each started on the
    recordings that were left with the term above them deleted; a directory with a
    term and no file refused, and left as it was; a directory with a log and no
    `terms` file refused, twice; a truncation that empties a
    table of three recordings above 128, and a start that reads 128 back.
  - Over three drivers, each member's driver closed and started with it, twenty
    runs of three tests, all passed. A follower lost and started again held the
    ten entries it lacked 0.224 s to 0.444 s after its start, under the same
    leader and term. A leader lost and started again followed the next term's
    leader 0.227 s to 1.069 s after its start. Three members closed and started
    again had a leader in a later term 0.320 s to 0.832 s after their start. In
    each, a service beside the member that started again read every committed
    entry from position 0, the bytes a service beside another member read.
  - Three `cavalry run` processes, stopped by SIGTERM and started again on their
    directories, elected a leader in a term above the one they had held.
- `remove_client` in `crates/cavalry-driver/src/conductor.rs`, read on
  2026-11-12: it removes a client's counters, registrations and subscriptions, and
  stops no recording.
- Not shown. No test cuts the power: what "What a start does not hold" says rests
  on `spec/durable-log.md`'s account of the recovery scan. No test runs a
  directory of an earlier version: none was released, and the refusal is tested
  on a directory whose file was removed.

## Consequences

- A member that stopped comes back on its log directory, and a cluster returns to
  its full count without being started again. "What the election does not do yet"
  in `crates/cavalry-raft/README.md` has no entry left.
- The module's thread makes one synced write for each recording that joins the
  log, each term led and each truncation: about 4 ms on the laptop above, at a
  join, never for an entry.
- A log directory is a member's whole state and moves whole: the catalog, the
  recordings, `terms` and `log-recordings`. A copy of an older state is not a
  backup of a member, since it reads back a shorter log than the member reported.
- A recording told to the election in the cycle in which the member leaves the
  log is not kept: its list was not yet written. The member had reported nothing
  from it, and replicates what it held.
- Revisit when the durable log carries a data checksum or a synced position: a
  start then cuts the log to what was synced.
- Revisit when a member can be replaced (the membership change event, out of this
  version): "start the cluster again as a new cluster" is then no longer the only
  way back from a lost directory.
- Revisit when snapshots let a member purge the front of its log
  (`spec/cluster.md`, "The snapshot marker"): a listed recording missing before
  one that is held is then expected, and the floor position is where that purge
  is recorded.

## References

- `spec/durable-log.md`, "Log recordings", "The driver", "What a reader rejects".
- `spec/cluster.md`, "The log", "Messages during an election", "A member's start".
- [ADR 0089](0089-start-a-member-that-elects-on-the-log-directory-it-kept.md),
  what the core reads at a start.
- [ADR 0091](0091-give-a-leaders-log-up-and-replay-it-over-a-driver-from-the-recordings-the-module-stopped.md),
  the table of recordings and its bound.
- [ADR 0093](0093-take-where-a-members-recording-of-the-log-begins-from-the-catalog-and-count-it-once-its-image-is-checked.md)
  and [ADR 0094](0094-record-a-members-replays-over-a-driver-and-count-the-live-recording-once-the-log-reaches-it.md),
  the recordings that are no part of a member's log.
- [ADR 0095](0095-truncate-a-members-log-over-a-driver-recording-by-recording-then-its-record-of-terms.md),
  the truncation's steps.
- [ADR 0100](0100-move-the-module-to-the-election-in-ordered-changes-around-one-that-takes-the-appointed-path-out.md),
  the sixth change.
- [ADR 0103](0103-list-the-recordings-of-a-members-log-in-its-driver-directory-for-a-service-to-read-in-order.md),
  the file a member writes for its service.
- [ADR 0104](0104-have-the-consensus-module-elect-its-leader-and-take-the-appointed-leader-out.md),
  why one member is never started alone on a new log directory.
