# 0103. List the recordings of a member's log in its driver directory, for a service to read in order

- **Status:** Accepted
- **Date:** 2026-11-12
- **Invariants affected:** 1, 3, 5, 6
- **Supersedes:** none. Decides what ADR 0066 left to this card under "To maintain":
  how a service reads a log of more than one recording. Changes `spec/cluster.md`:
  "The service" says which recording a service replays and when it moves to the
  next; a new section, "The log's recordings", gives the file's layout; "Counters"
  and "The service" gain a third rule of order; "What a reader rejects" and
  "Vectors" gain the file; "Out of this version" loses its last paragraph. Adds one
  row to "The driver directory" of `spec/client-protocol.md` and changes two
  sentences of the committed log's row in `spec/c-abi.md`. No message's layout
  changes and no function of the C ABI: the cluster page stays at version 0, the
  client protocol and the C ABI at theirs. Amends ADR 0100: this is one change more
  in its order, before its fifth, the move.

## Context

A service reads its member's log from one recording, the one the member names in
`cluster.log_recording_id` (ADR 0066). A member's log is more than one recording
after the member left the log and joined it again, after a later term, and after
a span it replicated or caught up (`spec/cluster.md`, "The log"). A service whose
position is below the start of the recording the member names is refused the
replay with code 23, and the page leaves the read across recordings to "the
features that add the election and the catch-up".

**With the election that refusal is no longer rare.** ADR 0100's fifth change was
tried in a scratch branch on 2026-11-12, the consensus module building its core
with the election. Two cases came with it.

- A leader that is elected closes its election with a quorum and appends without
  waiting for the last member. A follower that is slow at a cold start joins after
  the term's first entry, catches up through a replay, and holds its log in two
  recordings: the replay's, from 0 to 96, and the live one from 96. A service that
  opens the log at 0 beside it was refused: "position 0 is not a multiple of 32
  from 96 to 96". In 30 runs of the twelve tests over three drivers, run at once
  on a laptop, a follower joined late in 2, and in one of those the service of a
  test was refused so. In 40 cold starts of one cluster to a process, none did.
- After every later term each member records the new term's publication or image
  in a new recording. A follower's service has read to the commit position it
  took, and the recording the member left holds entries past it, which the next
  term commits. That service opens the log below the new recording's start, and
  is refused, after any election in which entries were in flight.

So the move cannot land before a service can read below the recording its member
names.

**Who knows which recordings hold a member's log.** The consensus module does, in
a table in its memory: the recordings it stopped, each with where it begins and
where it stopped, at most 1,024, in the order they begin (ADR 0091). The driver's
catalog holds more than those: a recording the member's election was never told
of, and one that began past where the member's log ended (ADR 0093), are in the
catalog and are no part of the log. A service is a client of the driver, in any of
five languages, and knows the driver directory alone: no command lists
recordings, and the log directory is the driver's, named to the driver by a
configuration key that a service is not given.

**What a service may take as committed from a recording its member has left.** ADR
0066 left this open because of truncation. The recording a member left holds an
uncommitted tail, which the next leader may ask the member to cut (ADR 0083). A
replay of that recording that began before the cut has played the tail into its
image, and a commit position stored after the cut covers new entries at the same
positions. A service that took that commit position as the bound of that image
would deliver entries that were never committed.

## Decision

**The member lists the recordings below its live one in a file in its driver
directory**, `log-recordings-<member id>`, the id in decimal. The file names the
member, the recording the member records the live log into and where it begins,
and then each recording that holds the member's log below it, in the order they
begin: the recordings the member stopped and keeps, and, in a follower's catch-up,
its recording of the replay, which is still being made, with the position it had
synced when the file was written. The layout is `spec/cluster.md`, "The log's
recordings": a header of 40 bytes with a magic and a version, and one entry of 24
bytes for each recording, at most 1,025. The consensus module writes the file
whole under another name and renames it over the last one, so a reader reads one
file or the other and never a part.

**A third rule of order.** A member writes the file before it stores a recording's
id in `cluster.log_recording_id`, each time it names a recording. No recording
the file lists is cut while the member names a recording. A member cuts a
recording only in a truncation, and it truncates only with every recording stopped
(ADR 0095) and −1 stored in the counter, a condition this decision adds to the
truncation so that the rule does not rest on the order of two steps elsewhere. A
table that is full forgets its oldest recording and leaves it as it is in the
catalog. So a file whose live recording is the id the counter holds lists
recordings that hold the member's log as it is now. A file that cannot be written
is the duty cycle's error, with what to do, and the member stores −1 until a later
cycle has written it: it records and replicates, and names no recording to a
service.

**A service takes each position from the first recording in the file that holds
it, and from the live recording when none does**, which is the rule the page
already has for a replay of the log ("The log"). `Cavalry::committed_log(position)`
finds the member's counters as before, reads the file, and starts the replay,
still one that follows, of the recording that rule gives. Where the file names
another live recording than the counter does, the member has moved on, and the
call answers as it does while the member names none. Where there is no file, the
member is of a version that writes none, and the call replays the live recording
as before. A file that fails a check of "What a reader rejects" is an error that
names the file and the check.

**The bound is the commit position while the member names the same live
recording**, whichever recording the handle replays. This is ADR 0066's check
unchanged: after each load of the commit position the reader loads
`cluster.log_recording_id` and compares it with the id it read when it opened the
log. It is enough for a recording below the live one because of the third rule's
reason: no recording of the member's log is cut while the member names a
recording, so every byte a replay played from a recording in the file is still
the member's log at that position when the commit position is loaded, and the
member's log below its commit position is committed.

**The handle ends at the end of the recording it replays, and the service opens
the log again**, as it already does when its member leaves a recording. A replay
of a recording that has stopped ends at the stop position; the handle is ended
once it has read to there, with no report set, and
`Cavalry::committed_log(log.position())` gives the next recording, or the live
one. A handle is one recording's, and `CommittedLog::recording_id`, the C ABI's
`recording_id` constant, is the recording it replays. No function is added and
none changes its type.

**What stays refused.** A position below the first recording the file lists, when
that does not begin at 0: the member's table forgot it, past 1,024 recordings.
The replay is asked of the live recording and the driver refuses it with code 23,
as before.

## Alternatives considered

- **The file in the log directory, durable, read by the service.** Lost. A service
  would need the log directory: a second argument in six APIs that must match the
  driver's key, or a driver that publishes its path, and the service would read in
  a directory that is the driver's. The table in memory is rebuilt at a member's
  start; whether that start reads a durable record of its own is ADR 0100's sixth
  change to decide, and it does not need this file's place or reader.
- **One counter for each recording, its stop as the value.** Lost. The driver
  holds at most 256 counters for all its clients (`spec/client-protocol.md`,
  "Sizes"), and a member's table holds 1,024 recordings.
- **The service asks the member over an IPC stream, and the member answers.** Lost
  in this version, for ADR 0066's reason against a service container: it is a
  protocol with an order of starting and a timeout, which an operator has to
  understand when it fails. A file needs neither. A snapshot, card 230's, needs
  the service to answer its member, and that ADR decides its channel.
- **A command of the driver that lists recordings.** Lost. The catalog does not
  say which recordings are the member's log; the member would have to tell the
  driver, which is this file with a command, a response and a version more.
- **One recording for a member's whole log, extended at each join, as Aeron's
  `extendRecording` does.** Lost. A recording holds one session id
  (`spec/durable-log.md`), and ADR 0091 and ADR 0094 built the log from one
  recording for each image. Extending a recording across sessions is a change to
  the catalog's format and to the recorder.
- **The recording id of the `terms` file's entries.** Lost. A term can lie in
  several recordings, a replay's and the live one, and a replay's recording can
  hold the end of one term and the start of the next.
- **A handle that moves to the next recording by itself.** Lost. Starting a replay
  is a command and a wait for its answer, on the path of `poll`, and the handle
  holds no client. A service's loop already opens the log again when the handle
  ends, and a recording's end is the same event.
- **No bound below the live recording: everything below its start is committed.**
  Lost, because it is false. The recording a member left can hold entries past
  the commit position until the next term commits them, and a service that read
  them early would hold entries a truncation may still remove.
- **A check of the replayed recording's own entry at each poll.** Lost. It needs a
  shared layout read on the per-message path. The live recording's id already
  changes before any cut.

## Evidence

- `spec/vectors/cluster.json`, kind `log recordings`, 31 vectors: six files with
  the fields a reader decodes and the recording each of several positions is read
  from, two entries that overlap, one inside another and two that begin together
  among them; and 25 rejections, each check and the order of two that both fail.
  `crates/cavalry-proto/tests/cluster_conformance.rs` reads each, writes its fields
  back to the same bytes and answers each position; a file of 1,025 entries is
  written and read, and one entry more is refused. `cluster_no_panic.rs` reads over
  60,000 byte strings and writes 20,000 files from entries at the edges of their
  types: each is taken or refused by name.
- A prototype on the laptop, 2026-11-12: the member's write and the service's read
  built on this change's reader and writer, as the two changes that follow it.
  - Three members over three drivers; a follower left the log and joined it again.
    Its file listed the recording it left, from 0 to its stop, below the one it
    named, and its counter held the id the file named. A service that started at 0
    read the first recording to its stop, was ended there with no report set,
    opened the log again and read on in the live recording: the 21 entries two
    services had read before, once each, in order.
  - A follower whose file could not be written, a directory in the place of the
    name it is written under: every duty cycle returned the error; the member
    recorded and counted toward the commit of ten entries; its
    `cluster.log_recording_id` stayed −1 and a service found no log. With the
    directory removed, the next cycle wrote the file, the counter named the
    recording, and a service read the eleven entries.
  - A leader and a member over two drivers, the member in a catch-up: the file
    listed the replay's recording, still being made, to what it had synced, at or
    past the live recording's start; after the member left, both recordings it had
    stopped, which overlap, in the order they begin. A truncation asked while the
    counter still named the recording the member left cut nothing; asked after, it
    cut.
  - A service beside a client that played the member. Below a live recording, a
    recording of ten entries with four committed: the service was given four,
    though the replay had played all ten, and the other six once the commit
    position covered them. The member named another recording and stored a commit
    position past the first recording's stop, as a member does that cut its log
    and went on: the handle was ended at the bound it took, four entries, and
    delivered nothing past it. A commit position that stayed below a stopped
    recording's end: the handle was ended after the 5 s linger with `withdrawn_at`
    at the service's position, and the log opened again there was the same
    recording. A file that was not one, and a file four times the longest: an
    error that named the file and the check, and no replay started.
- The tests this decision lands with are named in the pull requests that build
  it, as ADR 0066's were: those of the prototype above, over drivers.

## Consequences

Easier. A service reads from position 0, or from any position it had, beside a
member whose log is in any number of recordings the member keeps (invariant 1),
with nothing to configure (invariant 5). The move of the module to the election
no longer turns a late follower, or any election, into a refused service.

Harder. A service that starts at 0 after many terms opens the log once for each
recording, a subscription and a replay each time. A service whose member's commit
position stays below the end of a stopped recording for the publication linger,
5 s, is ended with `withdrawn_at` and opens the log again at the same recording.
The consensus module is the first client that writes in the driver directory:
the directory must be writable by the member's user, which it is where `cavalry
run` hosts both.

To maintain. The third rule of order, beside ADR 0066's two. The file's version:
a reader refuses one it does not know. The rule that a member cuts no recording
while it names one, which ADR 0095's truncation holds and this decision now
depends on. `cavalry doctor` does not read the file yet; a finding that says from
which position a member's log can be read on its host would use it.

Revisit when a service needs the log below 1,024 recordings back, which a
snapshot (card 230) answers first, or when a member hosts enough services for the
opens to show in a measurement.

## References

- [ADR 0066](0066-feed-the-committed-log-to-a-service-through-a-replay-of-the-members-recording.md),
  the service's read and its two rules of order;
  [ADR 0091](0091-give-a-leaders-log-up-and-replay-it-over-a-driver-from-the-recordings-the-module-stopped.md),
  the table of recordings a member stopped;
  [ADR 0093](0093-take-where-a-members-recording-of-the-log-begins-from-the-catalog-and-count-it-once-its-image-is-checked.md)
  and
  [ADR 0094](0094-record-a-members-replays-over-a-driver-and-count-the-live-recording-once-the-log-reaches-it.md),
  which recordings are part of a member's log;
  [ADR 0095](0095-truncate-a-members-log-over-a-driver-recording-by-recording-then-its-record-of-terms.md),
  the truncation;
  [ADR 0100](0100-move-the-module-to-the-election-in-ordered-changes-around-one-that-takes-the-appointed-path-out.md),
  the order this change joins.
- `spec/cluster.md`, "The log", "The service", "The log's recordings", "Counters";
  `spec/client-protocol.md`, "The driver directory" and "Commands", start replay;
  `spec/c-abi.md`, "Functions", committed log.
- Aeron: `RecordingLog` and `AeronArchive.extendRecording`, the one recording a
  member's log is there.
- Card 228.
