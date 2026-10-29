# 0030. Replay a recording into a subscription, with Aeron Archive's replay commands

- **Status:** Accepted
- **Date:** 2026-10-29
- **Invariants affected:** 1, 2, 3, 4, 6
- **Supersedes:** none; it takes the client protocol to version 2 (ADR 0019, ADR 0025)

## Context

Card 94 is the third feature of the second slice (ADR 0023): replay, the replay CLI and
`just replay-check`, the gate CLAUDE.md names for invariant 1, which still prints `not
built yet`. A recording is on disk as `spec/durable-log.md` lays it out: a catalog slot,
segment files holding the log buffer's raw term bytes from a base position, and a time
index that resolves an instant to a position. What is missing is the way back: a client
asks the driver to play a recording, from a position or an instant, into a stream it
subscribes to, and reads it through an ordinary subscription, so that the same code that
handled the live stream handles the replay, and the hash of what it saw is the same.

A replay is a wire change: two commands and two events in the client protocol, which is
a change to `spec/`, a conformance test and a version bump before any code, and a
decision CLAUDE.md says to ask about first (Working rules for agents). Aeron Archive's
shape is the reference (`ReplaySession.java`, `AeronArchive.startReplay`): a replay is a
publication the archive drives from the recording's segment files onto a replay stream,
paced by the subscriber's window, ending at a length, at the recording's extent, or
following a live recording; the client's `startReplay(recordingId, position, length,
replayChannel, replayStreamId)` returns a replay session id, and `stopReplay(replaySessionId)`
ends it. The frames go out as recorded, with the replay publication's session id and
stream id in their headers, so the subscriber's image is an ordinary image. This ADR
takes that shape, Cavalry's own type ids in the ranges ADR 0025 gave it, and names what
is Cavalry's own: the end rule, the pacing, the thread that does the disk work, the
counters and the gate.

## Decision

`spec/client-protocol.md` moves to version 2 with two commands and two events, taken in
order from the bottom of Cavalry's ranges (ADR 0025):

- **`start replay`** (type id 259, `0x0103`): client id, correlation id, recording id
  (i64), position (i64), length (i64), replay stream id (i32), replay channel (string,
  `cavalry:ipc` in this version). `position` is the start position of the replay: a
  multiple of 32 at or after the recording's start position and at or before its
  extent, or −1 for the recording's start position, as Aeron's `NULL_POSITION`.
  `length` is how many bytes of the recording to replay from that position: 0 or more;
  −1, Aeron's `NULL_LENGTH`, for everything up to the recording's extent when the
  replay starts, the stop position of a stopped recording or
  `recording.synced_position` of a live one, the bytes a replay may read; `i64::MAX`,
  as Aeron's `Long.MAX_VALUE`, to follow a live recording until it stops. The replay
  ends at the earlier of its end position, the position plus the length, and the
  recording's stop position, so a length past a live recording's extent follows it; a
  replay plays whole frames, so an end position inside a frame plays that frame whole
  and the replay ends at the first frame boundary at or after it, Aeron's rule, and the
  position `replay stopped` carries is a frame boundary. A client that wants "between two instants" resolves both through the time
  index, as `spec/durable-log.md` says, and sends positions; the driver never reads the
  time index for a replay.
- **`stop replay`** (type id 260, `0x0104`): client id, correlation id, replay session
  id (i64). Any client may stop any replay; a replay session id no live replay holds is
  already stopped, answered with `operation success` and nothing done, since a
  correlation id is never reused.
- **`replay ready`** (type id 4099, `0x1003`): correlation id, replay session id (i64),
  recording id (i64), start position (i64), session id (i32), stream id (i32). The
  replay session id is the correlation id of the `start replay` command, which is unique
  per driver as every registration id is; the session id is the replay publication's,
  which the subscriber's image carries; the start position is the position the replay
  starts at, which a client that passed −1 needs.
- **`replay stopped`** (type id 4100, `0x1004`): replay session id (i64), position
  (i64), reason (i32): 0 ended, the length or the extent reached; 1 stopped by command;
  2 no subscriber, the replay publication was not connected for the connect timeout;
  3 failed, a segment file could not be read or a frame failed a check, with the
  reason in the counters and the position the last frame delivered.

The driver answers `start replay` after the checks of "What the driver rejects" and
then against its state: a recording id the catalog does not hold is code 21, as for
`stop recording`; a position that is not a multiple of 32, before the start position or
past the extent is code 23, position out of range; a length below −1 is code 7; a
channel other than `cavalry:ipc` is code 1; the replays registry at capacity, or the
counters file with no free record, is code 10; a segment file the replayer cannot open
at the position is code 12. A start refused with code 21, 23 or 12 after the driver took
it adds one to `replay.start_failed`. Otherwise it creates the replay
publication: a log in `publications/<replay session id>.logbuffer` of the recorded
publication's term length, MTU length and initial term id, type 1 (exclusive), with a
new session id and the replay stream id, created at the replay's start position, the
active term count and the tails set so that the first frame lands at its recorded term
offset, which `spec/log-buffer.md`, "Metadata page", now says how to do without a
change to the layout; allocates `replay.position` for it, keyed by the replay session id; answers
`replay ready`; and sends `available image` to every subscription on the replay stream
id and channel, with the subscriber's position counter at the start position, now and
whenever one is added while the replay lives, so a late subscriber joins where the
replay is, as a subscriber of any publication does. The publication's limit is the
window past the lowest subscriber position, as for a client's publication, and the
replay goes no faster: a replay with no subscriber does not move. The driver removes
the log as it removes a closed publication's, after the end of stream position is
reached or the linger (`spec/log-buffer.md`, "Closing").

The disk work is a third driver thread, `cavalry-replayer`, fed and answered through
bounded queues as the recorder is (ADR 0025): the conductor never touches the disk,
and the recorder's writes never wait behind a replay's reads, since a recording
back-pressures a live publisher (invariant 2) and a replay is paced by its subscriber.
While it holds a replay the replayer idles with the conductor's idle strategy; while
it holds none it parks, for at most a second at a time, until the conductor hands it a
request and unparks it: a replayer with nothing to play has nothing to poll but that
queue, and a thread that woke once a millisecond on the housekeeping CPU the recorder
runs on moved `claim_commit_32_recorded` at p99.99 on the bench host (Evidence). On
each duty cycle the replayer visits every live replay: it reads the next block of the
recording, up to 1 MiB and never past the first frame boundary at or after the
replay's end position, from the segment file into the replay log's term at the
recorded term offsets, frame by frame, with every header as recorded but for the
session id and the stream id, which are the replay publication's, a padding frame
re-emitted as padding, and the frame length word stored last with a release store as a
publisher commits, so a subscriber's poll sees whole frames in order; it stops at the
publication's limit and continues when the limit moves. A replay that follows a live
recording reads up to the synced position and waits for it to move, and ends when the
recording stops, at its stop position, which the replayer learns from the catalog slot,
read again once per second and whenever a frame length of 0 is found below the extent. After each block it stores `replay.position`
with a release store. A replay reads no byte the recorder has not synced: it bounds
its reads by `recording.synced_position`, so what it plays is what a restart would find
(invariant 1). The replayer checks every frame as `spec/frame.md` says before it copies
it, and stops the replay with reason 3 at the first that fails, counting
`log_buffer.rejected_frame`.

A replay ends by itself when its length is replayed or its end position is reached or
passed, at the frame boundary: the replayer stores the end of stream position on the
replay log, as a closing
publication does, the conductor sends `replay stopped` with reason 0, and the
subscribers see `unavailable image` once they have reached it or the linger passes.
`stop replay` ends it at the position reached, with reason 1 and `operation success` to
the command; a replay whose publication had no subscriber for the connect timeout,
5 s, Aeron's replay connect timeout, ends with reason 2; one whose publication lost its
last subscriber ends the same way after the same timeout. Every end adds one to
`replay.stopped`, and the reason to `replay.ended`, `replay.failed` or
`replay.not_connected`; a start adds one to `replay.started`; the registry holds 64
live replays, and the counters file one counter per replay, 64, within its 8192
(`spec/client-protocol.md`, "Sizes").

The client API is `Cavalry::start_replay(recording_id, position, length,
replay_stream_id, channel) -> Replay` and `Cavalry::stop_replay(replay_session_id)`,
after `start_recording` and `stop_recording` (ADR 0021, ADR 0025): the `Replay` handle
carries the replay session id, the start position and the session id, reads
`replay.position` from the counters file, and stops nothing when dropped; the client
reads the replay through `add_subscription` on the replay stream id, and a `Replay`
offers `instant_to_position(&self, instant)` only as a free function over the time index
file, `cavalry_log::index::position_at(dir, recording_id, instant)`, which reads the
index as `spec/durable-log.md` says, locking nothing. The `cavalry` binary gains
`cavalry replay --recording <id> --from <position or instant> [--to <position or
instant>] [--stream <id>] [--follow]`: an instant is an RFC 3339 time or a Unix
timestamp in nanoseconds, resolved through the index; the command starts a replay onto a
stream id of its own choosing unless `--stream` names one, subscribes, and prints one
line per message, the position and the body as UTF-8 when it is, else as hex, to
standard output until the replay ends, then prints the replay's start and end positions
and how many messages it delivered; `--to` sets the length from the two positions,
`--follow` sets `i64::MAX`, and neither sets −1.

`just replay-check` is a script, `scripts/replay-check.sh`, under CI's gates jobs on
both runners, as `just quickstart` is: it starts a driver on a directory of its own,
publishes a known stream of 100,000 messages whose bodies are a counter and a hash
chain, records it from the start, stops the recording after the last message is
delivered to a live subscriber that hashes every delivered byte in order, replays the
recording twice from its start into two subscribers that hash the same way, and fails
unless the three hashes are equal, printing all three; it also replays from an instant
resolved through the index and checks the first position it reports is the index's.
The script is the gate for invariant 1 that CLAUDE.md names, and ADR 0023's Revisit
clause for the slice's end stands.

A version 1 client that receives `replay ready` or `replay stopped` from a version 2
driver closes, as the page says of an unknown event type; the upgrade restarts the
clients before the driver, as version 1's did. ADR 0025's Revisit names the
skip-with-counter rule for an unknown event type in Cavalry's range as due before the
first release; this ADR leaves it there, since a client that sends only the commands it
knows can be answered only by event types it knows, and the rule needs a counter and a
conformance vector of its own.

## Alternatives considered

- **The replay on the recorder thread.** Lost: the recorder's duty cycle bounds a live
  publisher's back-pressure, and a replay's segment read, a disk read of up to 1 MiB,
  would sit in it; one more thread with the same idle strategy keeps the two apart, and
  the conductor's queues already exist for the recorder.
- **A replay that reads the time index in the driver, with instants on the wire.**
  Lost: the format page already says how an instant resolves, the index is a file any
  reader may read without a lock, and a position on the wire keeps the driver's work
  one kind; the CLI and the client resolve instants.
- **The replay's frames rewritten with new term ids and offsets, as a fresh
  publication.** Lost: the card and ADR 0023 say headers as recorded but for the session
  and stream ids, which is what Aeron does, and what makes a replay's positions the
  recording's positions, so a subscriber can relate what it sees to the catalog and the
  index; the replay log takes the recording's term length and initial term id for that.
- **A replay that follows a live recording by default.** Lost: Aeron's default, length
  −1, is the extent at the start, and a follow is an explicit `Long.MAX_VALUE`; a
  reader asking "what did the stream see" gets an answer that ends.
- **The replay session id as a new id the driver mints.** Lost: the command's
  correlation id is unique per driver and already names the publication's log file, as
  a registration id does; Aeron returns a separate id because its control protocol runs
  over a network.
- **Reading past `recording.synced_position` for a live replay.** Lost: a replay would
  then play bytes a power loss could take back, and a second replay after a restart
  would differ from the first, which invariant 1 forbids; the cost is at most a second
  of lag behind a live recording, the sync cadence.
- **`just replay-check` as a Rust integration test.** Lost: the gate must cover the
  `cavalry` binary's own path, record and replay as a user runs them, as `just
  quickstart` covers the README's; a script drives the binaries and hashes their output.

## Evidence

- `spec/durable-log.md` (ADR 0024): the segment files hold the log buffer's raw term
  bytes from a base position, so a block of a recording is a block of a term, and the
  recorder scans blocks of whole committed frames (ADR 0025), which is what the replayer
  copies back.
- Aeron's `ReplaySession.java` and `AeronArchive.startReplay`: the replay publication,
  the length conventions, the connect timeout, and the end at the length or the
  recording's extent.
- ADR 0023's Decision, feature 3, and card 94's brief: the commands, the API, the CLI
  and the gate named here.
- The conformance vectors of version 2, 84 in `spec/vectors/client-protocol.py`, 26
  of them new, land in the same change as this acceptance, with the driver's replayer
  and its tests: a recording of a segment and a bit replayed across the segment roll to
  a subscriber that reads every byte in order, a replay of a live recording that plays
  only synced bytes and ends at the stop, and a replay paced through the log's terms by
  a slow subscriber. `just replay-check`'s three equal hashes on both runners are the
  gate's commit's evidence, in its message, as step 2.1 of
  `docs/how-to/implement-a-feature.md` puts what the implementation proves.
- The bench gate on the first head of the change that accepted this ADR (CI run
  40850983644), with the replayer idling under the backoff strategy, waking once a
  millisecond on the housekeeping CPU the recorder shares: `claim_commit_32_recorded`
  read 320 ns at p99.99 (21 runs, 300 to 340) under the `bench-linux` profile and
  330 ns (310 to 340) under `bench-linux-unchecked`, against master's last four runs at
  309 to 310 and 300 to 310, while `claim_commit_32_client`, whose recorder idles, read
  160 ns under both as master does; the unchecked gate failed at +10.0% over its
  baseline's slowest run of 300 ns. The replayer parks instead; the gate's rows on the
  head that merged are in the pull request's body.

## Consequences

- The client protocol is at version 2: every version 1 encoding is unchanged, type ids
  259, 260, 4099 and 4100 are taken, 23 is a new error code, and the next Cavalry
  command is 261, the next event 4101.
- The driver has a third thread, `cavalry-replayer`, a replays registry of 64, one
  counter per live replay beside its publication's two, and six static counters;
  `counters.toml` registers the replay's state machine so `just lint-counters` holds
  its transitions.
- `spec/log-buffer.md`, "Metadata page", says how a log is created at a position; no
  field of the layout changes, so the log buffer stays at version 0, and
  `cavalry-shm` gains `LogBuffers::create_at` and the replayer's `TermWriter`, which
  writes by the claim protocol and adds no `unsafe` block.
- `just replay-check` is a real gate, and ADR 0023's third feature is done when it
  passes on both runners.
- A replay reads only synced bytes, so a live replay lags the recording by up to the
  sync cadence; a user who needs less lag needs the `log.sync` key ADR 0025's Revisit
  names.
- Revisit: when a user needs replay-merge, a replay that catches up a live stream and
  hands over, or an original-speed replay (ADR 0023); when UDP replays onto a channel
  other than `cavalry:ipc`; when the skip-with-counter rule for unknown events lands
  (ADR 0025); when 64 live replays are not enough, which is a registry size and a doctor
  check.

## References

- Card 94; ADR 0019, ADR 0021, ADR 0023, ADR 0024, ADR 0025.
- `spec/client-protocol.md`, `spec/durable-log.md`, `spec/log-buffer.md`,
  `spec/frame.md`.
- Aeron: `io.aeron.archive.ReplaySession`, `io.aeron.archive.client.AeronArchive`
  (`startReplay`, `stopReplay`), and the wiki page "Aeron Archive".
