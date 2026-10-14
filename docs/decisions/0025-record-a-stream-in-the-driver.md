# 0025. Record a stream in the driver, with Aeron Archive's commands in the client protocol

- **Status:** Proposed
- **Date:** 2026-10-13
- **Invariants affected:** 1, 2, 3, 4, 5, 6
- **Supersedes:** none

## Context

Card 93, the second feature of the second slice (ADR 0023): the driver records a stream
to the durable log whose on-disk format ADR 0024 fixed. ADR 0023 decided the shape:
`start recording` and `stop recording` in the client protocol and the client API,
`cavalry record`, and a recorder that is a dedicated thread of the driver with an idle
strategy, reads the log buffer as a subscriber does, holds a `recording.position` the
publisher's limit respects, writes whole blocks of term bytes into segment files, and
reports what is written and what is synced as two positions, never a position before it
is true; it needs the configuration key `log.dir`, approved by Rjae before it exists, and
it left the sync cadence to this ADR. What this ADR decides is what ADR 0023 left open:
the commands' and events' encodings, the recorder's block path and its place in the
driver, the directory's default and how two drivers are kept out of one directory, the
sync cadence, and the counters and doctor checks.

Aeron Archive, read from its source on 2026-10-13, records in a process of its own with
a control protocol of its own (`aeron-archive-codecs.xml`): `StartRecordingRequest`
carries a channel, a stream id and a source location, `StopRecordingByIdentityRequest` a
recording id, and `RecordingSignal` announces `START` and `STOP` with the recording id
and position. Its `RecordingWriter` takes blocks from an image's `rawPoll`, whose
`TermBlockScanner` collects whole committed frames up to a block limit and stops at the
first frame not yet committed, and writes each block into a segment file with one
positional write; a `rec-pos` counter per recording tells readers how far it is. Its
directory is `aeron.archive.dir`, by default `aeron-archive` under the working
directory; `aeron.archive.file.sync.level` chooses between no sync, a data sync and a
data and metadata sync per write, and defaults to no sync; a mark file,
`archive-mark.dat`, carries a heartbeat timestamp, and a second archive refuses to start
while that timestamp is fresh.

What Cavalry lacks, and the rules it is built under. `cavalry-shm`'s `TermReader` hands
out frames, not blocks. The driver's limit is computed from `subscription.position`
counters only (`spec/log-buffer.md`, "The driver"). The driver directory is memory and
dies with the host (ADR 0020). The recorder is a data-plane thread, so each block it
writes allocates nothing, locks nothing and never panics (`.claude/rules/data-plane.md`);
the syscalls it makes are its own and never the publisher's. One configuration key needs
a written reason its default cannot always be right and a doctor check that fires when it
is set wrong (`CLAUDE.md`, invariant 5), and every Cavalry key so far has an absolute
default a stranger can find without the process's working directory. The standard
library's `File::try_lock` was stabilised in Rust 1.89; the workspace floor is 1.85,
edition 2024's minimum (ADR 0003), nothing in the repository or its dependencies needs
more, the highest dependency floor is 1.82, and CI builds on stable. Rjae decided the
key, the floor and the cadence one at a time on 2026-10-13, in the working session; the
decisions and the alternatives put to them are recorded here, since a conversation is
not a decision until it is an ADR.

## Decision

`spec/client-protocol.md` moves to version 1 with two commands and two events, the first
of Cavalry's own, so their type ids come from ranges of Cavalry's own that Aeron's ids do
not reach. Aeron's driver numbers its commands up from 1, at `0x12` today, and its events
up from `0x0F01`, at `0x0F0D` today, one id per feature, and ADR 0019 keeps every id in
those two runs for the feature Aeron gave it. The page reserves commands 1 to 255
(`0x01` to `0xFF`) and events 3841 to 4095 (`0x0F01` to `0x0FFF`) for Aeron's ids,
present and future, and gives Cavalry commands 257 to 511 (`0x0101` to `0x01FF`) and
events 4097 to 4351 (`0x1001` to `0x10FF`), each taken in order from the bottom of its
range; an id outside a range a version specifies stays code 6 for the driver and `type`
for the client, as the page already says. Aeron would need 237 more commands or 242 more
events to fill the ranges kept for it, and a Cavalry range that fills is a new ADR. `start recording` (type id 257, `0x0101`: client id, correlation id, stream id,
channel) asks the driver to record the live publication on that stream id and channel
from its producer position, loaded afresh as a join loads it (ADR 0022); the driver
answers `recording ready` (type id 4097, `0x1001`: correlation id, recording id, the
`recording.position` counter id, the `recording.synced_position` counter id) once the
catalog slot is valid and synced and
the first segment file and the time index exist. No live publication matches: error
code 20. One already being recorded: code 22, with its recording id in the message. The
catalog's slots used up, the recordings registry at capacity or the counters file with
no free record: code 10. A segment or index file that cannot be created: code 12. The
new codes sit past Aeron's range, whose 13 and 14 the page keeps reserved. `stop
recording` (type id 258, `0x0102`: client id, correlation id, recording id) stops a recording and is
answered with `operation success` once the stop fields are written and synced; a
recording id the catalog does not hold is code 21, and one already stopped is `operation
success` with nothing written, since a recording id is never reused. A recording belongs
to the driver, not to the client that started it: it outlives that client, and any
client may stop it. The driver stops a recording by itself when its publication closes,
at the end of stream position, once the recorder has written up to it; the log of a
recorded publication is removed only then, and the recorder's lag is bounded by the
window. Every stop, by command or by close, is announced to every client with
`recording stopped` (type id 4098, `0x1002`: recording id, stop position), and the
recording's two counters are freed after the counter reuse timeout. A version 0 client
never sends the new commands; a version 1 client that sends them to a version 0 driver
gets code 6, as the page already says of a type the driver does not know. ADR 0019's
Revisit names the UDP slice's channel status and destination commands as version 1;
this ADR takes version 1, so they come in a later version, under Aeron's numbers, which
this version leaves free.

The recorder is a thread of the driver named `recorder`, with the conductor's idle
strategy, and the conductor never touches the disk: it hands the recorder each start,
stop and close through a bounded in-process queue (`cavalry_shm::spsc`, as the client's
conductor hands images, ADR 0021), the recorder answers through a second queue, and the
conductor sends the event when the answer arrives, within the client's response
timeout. On each duty cycle the recorder visits every live recording. It scans a block
after Aeron's `TermBlockScanner`, a new block scan in `cavalry-shm` with a `loom` model
and Miri: from the recording's position it loads frame lengths with acquire loads, as a
subscriber's poll does, and takes whole committed frames, padding included, up to 1 MiB
or the end of the term, stopping at the first frame whose length is not yet committed.
A committed frame's bytes never change until cleaning, and cleaning stays below the
recorder's position, so the block is written from the mapping into the segment file
with one positional write and no copy in between; a segment rolls at its length as
`spec/durable-log.md` says. After a write it stores `recording.position`, the bytes the
operating system holds, with a release store, and appends a time index entry when a
millisecond has passed since the last. It reads its clock once per duty cycle, as the
conductor does. The publisher's limit for a recorded publication is the window past the
lesser of the subscribers' positions and `recording.position`, and cleaning uses the
same lesser position, so a slow disk back-pressures the publisher and no recorded byte
is cleaned before it is written; the recorder is never lossy. A write that fails counts
`recording.write_failed` and stops the recording at the position written, which frees
the publication's log. The live recordings are bounded by a registry of 1024, one per
publication at most, and the counters file holds two counters per recording within its
8192.

The log directory is the configuration key `log.dir`, Aeron's `aeron.archive.dir`
without its prefix and with Cavalry's name for the thing: type path, flag `--log-dir` on
`run`, `doctor`, `record` and `replay`, variable `CAVALRY_LOG_DIR`. Its default is an
absolute path fixed by the user, as `dir`'s is: `$XDG_DATA_HOME/cavalry/log` when that
variable is set, else `$HOME/.local/share/cavalry/log`, and `/var/lib/cavalry/log` when
the driver runs as root; with neither a home nor root the driver refuses to start and
names the key. Its `why_not_default`: the home filesystem can be small, a network mount
where a sync and a lock mean little, or absent for a service user; a container's is
ephemeral; the operator usually wants the log on a disk chosen for room and durability,
which the driver cannot pick; it is never under the driver directory, which is memory.
Its doctor check, `log-directory-backing`, warns when the directory is on a memory-backed,
overlay or network filesystem, when the driver's user cannot write it, and when the
filesystem has less than eight segment files, 1 GiB, free. One driver per directory: the
recorder takes an exclusive advisory lock on the catalog file when it opens it, without
blocking, through the standard library's `File::try_lock`; a second driver refuses to
start with `log directory locked`, naming the directory and the flag; the operating
system drops the lock when the holder dies, so a crash never leaves a stale one; readers
lock nothing, since the lock is advisory; no file joins ADR 0024's directory. The
workspace's `rust-version` moves from 1.85 to 1.89 for the call.

The sync cadence is fixed in this version: the recorder calls fdatasync on a recording's
segment file and time index once per second while it has unsynced bytes, at a segment
roll on the segment it closes, at stop and when the driver closes, and after each sync
stores `recording.synced_position` with a release store. The catalog is synced at every
transition it writes, before the driver answers, so a reboot never forgets a recording a
client was told about. Only `recording.position` enters the publisher's limit; the sync
never does. A power cut loses at most the last second of a recording, recovery (ADR 0024)
finds the true stop position, and the two gauges tell a reader how far the recorder is
behind the stream and how far the disk is behind the recorder. No sync key exists in
this version; one is a Revisit.

Counters in `counters.toml`, written by the driver: static `recording.started`,
`recording.stopped`, `recording.write_failed`, `recording.catalog_full`, and for the
recovery pass ADR 0024 describes `recording.recovered`, `recording.invalidated` and
`recording.index_rewritten`; instance `recording.position` and
`recording.synced_position`, key: recording id, label: recording id, session id and
stream id in decimal. Doctor checks: `recording-stalled`, a `recording.position` that does
not move while its publication's position does; `recording-write-failed`;
`recording-invalid`, a catalog slot in state 2; and `log-directory-backing` above. Every
message names the page and section that explains it.

The client API gains `Cavalry::start_recording(channel, stream_id) -> Recording`, with
`recording_id`, `position` and `synced_position` read from the counters file, and `stop`;
dropping a `Recording` stops nothing, because the recording is the driver's; and
`Cavalry::stop_recording(recording_id)` for a recording another client started. The
binary gains `cavalry record <stream id> [--channel <channel>]`, which starts a recording
and prints its id, and `cavalry record --stop <recording id>`; `cavalry doctor` reports
every recording's two positions. The `cavalry-log` README's example records what the
ten-minute path publishes.

## Alternatives considered

- **The recording commands and events in the gaps of Aeron's runs, commands 12 and 13
  and events `0x0F0B` and `0x0F0C`.** Lost: Aeron already uses all four, for add and
  remove receive destination, static counter and publication error, and ADR 0019 keeps
  every Aeron id for the feature Aeron gave it, so the UDP slice and a later counter
  feature could not take their commands under Aeron's numbers. The same rule already
  puts the new error codes past Aeron's.
- **The recording commands after Aeron's last ids, command 19 and event `0x0F0E`.**
  Lost: Aeron adds an id at its next feature, and the next Aeron release would take the
  number Cavalry had shipped; a range of Cavalry's own, with room between it and
  Aeron's, never collides.
- **Aeron Archive's ids, `StartRecordingRequest` and `StopRecordingByIdentityRequest`.**
  Lost: they are template ids of the archive's own SBE schema, numbered from 1 in a
  space of their own, and in the client protocol they would land among the ids kept for
  Aeron's driver commands.
- **A separate archive process with its own control protocol, as Aeron.** Lost in ADR
  0023; it returns if the recorder's writes move a bench row.
- **The recorder on the conductor's thread, or the conductor writing the catalog.** Lost:
  a write or a sync that stalls on the disk would stall every publication's limit and
  every client's command; the conductor never touches the disk, and the queue between
  the two threads is the one the client already uses between its threads.
- **The recorder loading the producer position instead of scanning frame lengths.** Lost:
  a subscriber reads the stream by the frame lengths it finds, and the recorder is one
  more reader; the scan stops at the first uncommitted frame on its own, with no second
  position to keep in step.
- **A recording that belongs to the client that started it, removed with the client.**
  Lost: a recording is for whoever replays it later, and a client that crashes must not
  take the recording down with it; Aeron's recordings outlive their requester too.
- **Back-pressure through a subscription of the recorder's own.** Lost: a subscription
  carries an image and a client; the recorder is the driver, and the limit's rule gains
  one position instead.
- **Stopping a closed publication's recording after the linger, where the recorder is.**
  Lost: the recorder's lag is bounded by the window, so finishing costs at most a
  window's worth of writing, and a recording of a closed publication then holds
  everything it published; a disk that cannot write a window is visible in
  `recording-stalled` and ends in `recording.write_failed`.
- **Aeron's working-directory default for the log directory, `cavalry-log` under the
  working directory.** Lost: a systemd unit's working directory is `/`, and a replay or
  doctor run from another shell would resolve a different directory than the driver did,
  which hides the log from the person looking for it (invariant 6); a rule fixed by the
  user makes every command find the same log with no flag.
- **A default under the driver directory.** Lost: the driver directory is memory and
  dies with the host (ADR 0020).
- **No key, and a fixed `/var/lib/cavalry/log`.** Lost: a laptop user cannot write it,
  and the ten-minute path breaks.
- **A separate macOS rule under `~/Library/Application Support`.** Lost: one rule reads
  easier, and the key exists for anyone who wants the log elsewhere.
- **A locking crate, `fs4` or `fd-lock`, in `cavalry-log`.** Lost: a data-plane
  dependency needs an allowlist entry and a justification for twenty lines the standard
  library now has.
- **Calling `flock` through libc.** Lost: `unsafe` is allowed in `cavalry-shm` and
  `cavalry-ffi` only, and `cavalry-shm` has no libc dependency; it maps memory through
  `memmap2` alone.
- **Aeron's mark file with a heartbeat timestamp, refusing to start while it is fresh.**
  Lost: correct after a crash once the liveness timeout passes, but it costs a periodic
  write for the directory's whole life, and a restart after a crash waits out the
  timeout; a plain pid file is the same idea without the heartbeat and lies until someone
  deletes it.
- **No lock, with documentation saying one driver per directory.** Lost: two drivers
  computing the next recording id as the catalog's slot count would write the same slot
  and the same segment files, which is a corrupt catalog and a lie about delivery
  (invariant 1).
- **Never sync, Aeron's default.** Lost: it is not faster in steady state, since the
  kernel writes the same pages back within seconds either way and throttles the
  recorder's writes itself when the disk is slower than the stream, invisibly; a power
  cut then loses a tail bounded only by kernel settings the operator has not seen, and
  no synced position can exist.
- **Sync every block.** Lost: a disk's sync costs from a tenth of a millisecond on NVMe
  to tens of milliseconds on a disk or a cloud volume, and the recorder, and through the
  limit the publisher, would run at the disk's sync rate; the recording is a side effect
  of the stream and must not set its pace.
- **A sync key with Aeron's levels from the start.** Lost for this version: no detection
  can set an application's durability need right, so the key would be a knob set by
  documentation; it is a Revisit, with its reason and a doctor check that times a sync
  against the window at the observed rate, when a user needs no sync or a sync per
  write.

## Evidence

A design decision informed by Aeron Archive's source (ADR 0023) and Rjae's three
decisions of 2026-10-13: the key with the absolute per-user default, the floor at 1.89
for the standard library's lock, and the fixed one-second cadence with the two gauges
and no key. What the sync costs: one call per second is under three percent of the
recorder's time on a spinning disk, and the 32 MiB window of a `cavalry:ipc`
publication absorbs a stall of 32 ms at 1 GB/s and 320 ms at 100 MB/s, both longer than
a sync on any disk that is not failing. What accepts this ADR: the client protocol's
vectors for the new commands and events and the conformance suite over them; the block
scan's `loom` model and Miri run; the recorder's tests at capacity, a publisher faster
than the disk that is back-pressured and never lossy, a full disk that stops the
recording with its counter, a full catalog and a full registry answered with code 10;
the bench's allocation assertion over the recorder's block path; and the bench host's
rows with a recording on. The acceptance commit records them.

## Consequences

Easier: `cavalry record` exists, and a recording survives its client; a slow disk is
visible as back-pressure and in two counters, never as loss; the conductor never waits
on a disk; two drivers cannot share a log directory by mistake; the recorder, the
client's conductor and the driver's conductor all cross threads through the same
bounded queue.

Harder: the limit and cleaning have a third input; a second data-plane thread to idle;
a closed publication's log lives until the recorder has written it; the catalog's
transitions each cost a sync on the recorder's thread; the client protocol is at version
1, with Cavalry's own type id ranges beside Aeron's; the Rust floor is 1.89, so the four
releases 1.85 to 1.88, from February to June 2025, no longer build the workspace. To maintain: the recorder's state machine and its queues, the
block scan's `loom` model, the recovery counters, and the doctor checks.

Revisit: when the bench shows the recorder's writes on the raw rows, for the separate
process; when a user needs no sync or a sync per write, for the `log.sync` key with its
doctor check; when UDP records a channel with a source location, as Aeron's
`SourceLocation`; when card 94's replay needs a recording's extent while it is live, which
the two gauges give it.

## References

- `spec/client-protocol.md`, `spec/durable-log.md`, `spec/log-buffer.md` ("The driver");
  ADR 0003, 0019, 0020, 0021, 0022, 0023, 0024; `crates/cavalry-log/README.md`;
  `config.toml`; `counters.toml`
- Aeron, `aeron-archive/src/main/resources/archive/aeron-archive-codecs.xml`,
  `aeron-archive/src/main/java/io/aeron/archive/{RecordingWriter,ArchiveConductor,
  Archive,ArchiveMarkFile}.java`, `aeron-client/src/main/java/io/aeron/logbuffer/
  TermBlockScanner.java`, read 2026-10-13
- Aeron, `aeron-client/src/main/java/io/aeron/command/ControlProtocolEvents.java`, for the
  command and event ids Aeron uses, read 2026-10-14
- AppShapes Trello card 93
