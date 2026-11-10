# 0067. Sync a recording on command, as client protocol version 8

- **Status:** Accepted
- **Date:** 2026-11-10
- **Invariants affected:** 1, 2, 3, 6
- **Supersedes:** none. Adds a command to `spec/client-protocol.md`, which moves to
  version 8; every version 7 encoding is unchanged. Amends `spec/cluster.md`, "The
  log", within version 0, with no encoding and no vector.

## Context

The recorder syncs a recording's segment and time index once a second (ADR 0025), and
`recording.synced_position` is what a sync that returned success covered (ADR 0064,
ADR 0065). A cluster member counts for a commit only by that position (ADR 0060), so
an entry a leader appends is committed when the recorders of a quorum next sync: up to
a second later, with every member idle in between. ADR 0060 says how that was to end:
"the consensus module asks the recorder to sync after each append it reports, the
bounded request queue the conductor already holds, so that a commit waits for one
sync and not the next tick". ADR 0063 then made the consensus module a client of its
driver, and a client has no way to ask: the client protocol has no such command.

Any program that records what it publishes and acknowledges it once it is durable
has the same wait.

## Decision

Version 8 of the client protocol adds one command, `sync recording`, type id 264
(`0x0108`), the next of Cavalry's own: client id, correlation id, recording id.

The conductor hands a live recording's command to the recorder through the request
queue it holds for starts and stops. The recorder, when it takes the request, syncs
what it has written of the recording so far, the segment file and the time index,
stores `recording.synced_position`, and the driver answers `operation success`. A
recording with nothing unsynced is answered without a sync. The recorder's own sync
is next due a second after this one. So for a live recording the answer means: what
the recorder had written when it took the request is covered by a sync that returned
success. It does not mean that what the client published before the command is
covered: the recorder may not have written it yet. A client that needs a position
reads `recording.synced_position` and asks again while it is behind, and one sync
then covers everything written since the last, however many frames that is.

The refusals are `stop recording`'s where they can be. A recording id below 0 or at
or past the next recording id is error code 21. An id below the next that no live
recording holds is a recording that has stopped, as is one the recorder stopped by
itself before it took the request: `operation success`, and nothing is done. That
answer means only that nothing is left to sync; what a stopped recording holds is what
its `recording stopped` said.

The request is never kept for later. With the recorder's request queue full, or
holding requests that still wait for room in it, the command is error code 10,
counted in `driver.registry_full`, and the client asks again. So a client's syncs
are bounded by the queue, and they never take the room the conductor keeps for a
recording's attach and stop, which do wait.

A sync that fails is error code 12, and the recording is given up as ADR 0065 says:
`recording.write_failed` counts it, and `recording stopped` follows with the position
last synced. A sync is never asked a second time of a recording whose sync has
failed: a command for such a recording, taken before the recorder has given it up,
is answered with error code 12 and no sync is made.

The Rust client gets the command twice. `Cavalry::sync_recording(recording_id)`, with
`Recording::sync(client)` for a handle, waits for the answer. And
`Recording::request_sync(client)` sends the command and returns: the caller watches
`synced_position()`, and the driver's answer is dropped when it comes. The second is
for a caller that must not wait, a consensus module in its duty cycle; it sends
nothing for a recording the handle has seen stop, and it allocates nothing.

A version 7 driver answers the command with error code 6, as it answers any type id
it does not know, and a version 8 client's other commands are version 7 commands.

`spec/cluster.md`, "The log", says what ADR 0060 decided and the page did not: a
recording holds a position once a sync that returned success has covered it, so a
member's append position is its recording's synced position. And it says who asks: a
member asks its driver to sync its recording of the log whenever the recorder has
written past what it has synced, once for each position written that it sees, and
without waiting for the answer. It asks once per position and not once per duty
cycle because the recorder writes nothing while it syncs: the next block it takes
holds everything that arrived meanwhile, and one request covers it, so the syncs
group the entries by themselves. A request the driver refuses is not sent again
until the position written moves, and the recorder's own sync covers what it left.

## Alternatives considered

- **A recording that syncs every block, set when it is started.** Lost. It needs a
  field on `start recording` or a key on its channel, which changes an encoding every
  client sends, and it puts the cadence in the wrong hands: a recorder that syncs each
  block it writes makes a sync per duty cycle under load whether anyone waits for one
  or not. A command leaves the cadence to the program that knows when it has
  something to acknowledge, and one command covers all that was written before it.
- **A shorter sync interval, or a configuration key for it.** Lost: CLAUDE.md,
  "Configuration and defaults". An interval is a tick, and a commit would still wait
  for it; a key is a knob that makes a benchmark pass.
- **Answer with the synced position.** Lost: `operation success` carries no position,
  a new event would be a second encoding, and the position is in the counter the
  client already reads.
- **Sync up to the publication's position when the command is handled, waiting for
  the recorder to reach it.** Lost. The recorder would hold a request while it writes,
  a wait with no bound under back-pressure, for what a second command gives a client
  that wants it.
- **One sync in flight for a recording, a second command refused as busy.** Lost: the
  request queue is the bound, a second request that finds nothing unsynced costs no
  sync, and a refusal would make every client write a retry for the common case.
- **Keep a command that finds the queue full, as a stop is kept.** Lost. The
  conductor's list of kept requests is bounded at one attach and one stop for each
  recording, an argument that holds because a recording has one of each. A client can
  send any number of syncs, so keeping them would need a second bound and a second
  full case, for a command the client can send again.
- **An error for a recording that has stopped.** Lost: `stop recording` answers a
  stopped recording with success, a client that syncs as its publication closes would
  race the stop for no reason of its own, and the stop position is the answer it
  wants, which `recording stopped` carries.
- **Count the command: `recording.syncs_requested`.** Lost for now: no state changes,
  and no pathology has been seen that the counter would explain. A sync that fails is
  counted where it always was, in `recording.write_failed`, and a command refused at
  the queue in `driver.registry_full`.
- **The command in the C ABI now.** Lost for this change: the ABI moves to its
  version 2 in one step with the calls the cluster's clients need (card 227), under
  an ADR of its own.

## Evidence

- `spec/client-protocol.md`, "Commands": the recorder's sync once a second, which the
  page named and gave no row in "Liveness and timeouts"; this change adds the row,
  `recording sync interval`. 264 was the next free type id of Cavalry's own.
- A cluster of three members on one host, measured before this change in card 227's
  tests: 5,001 entries appended in a few milliseconds were read by a service on each
  member 0.97 s later, at the recorders' next sync.
- `spec/vectors/client-protocol.json`: the command's encoding and its extremes, a
  message one field short and one four bytes long, the two answers, and 265
  (`0x0109`) as the first reserved id of Cavalry's range.
- `cavalry-log`,
  `a_sync_now_syncs_what_is_unsynced_whatever_the_time_and_the_periodic_sync_waits_a_second_after_it`:
  64 bytes synced one nanosecond after the start; a second call makes no sync; the
  periodic sync is not due a second after the start, and is due a second after the
  sync on request.
- `cavalry-driver`, on a clock the test advances:
  `conductor::replay::a_sync_recording_syncs_what_the_recorder_has_written_without_waiting_a_second`
  (640 bytes written 20 ms after the start, synced position 0; the command moves it
  to 640, then to 1,280, inside the recording's first second; ids 1 and −1 are code
  21; a stopped recording is success with nothing handed to the recorder);
  `a_sync_recording_that_finds_the_request_queue_full_is_refused_and_not_kept` (code
  10 with the queue full and with a request waiting, `driver.registry_full` moved
  each time, nothing kept);
  `a_sync_recording_after_a_sync_failed_is_refused_and_makes_no_sync` (code 12, the
  synced position stays 640 with 1,280 written, `recording stopped` carries 640;
  with the refusal removed the test fails with `left: [50] right: []`, the command
  answered as a success);
  `a_sync_recording_behind_a_stop_in_the_queue_is_success_with_nothing_done`.
- `cavalry-client`, against a driver in the test's process:
  `a_sync_on_request_covers_what_the_recorder_has_written_when_it_returns` (twenty
  rounds of ten messages, the synced position at the position when `Recording::sync`
  returns, with no wait after it);
  `a_sync_requested_without_waiting_is_handled_by_the_recorder_in_order` (twenty
  rounds; with the send removed it fails at round 1 with `left: 0 right: 640`); and
  `tests/sync_allocations.rs`: two hundred rounds of a message, `request_sync` and
  the synced position reaching it allocate nothing in any thread of the process, the
  client's and the driver's.
- The commit this is for, measured on card 227's stack with the consensus module
  asking as "The log" now says, three members over three drivers on the loopback of
  a macOS laptop: ten entries, each appended once every member had committed the one
  before, took 10.013 s without the requests, since two commits are then at least a
  recorder's second apart, and from 186 ms to 228 ms with them, in twenty runs. That
  test, and the module's allocation gate with the requests in it, land with the
  module.
- What no test here covers: an `fdatasync` that fails. A test cannot make one fail on
  the recorder's descriptors without a failing disk, as ADR 0065 records, so the
  branch of `Recorder::sync_now` that marks the recording when the command's own sync
  fails is read and not run; the test of the refusal sets the mark that branch sets.
  And a version 7 driver's answer: the rule that an unknown type id is code 6 is
  tested with id 265, not with a driver of the older version.

## Consequences

Easier: a cluster member asks for a sync when its recording is ahead of its synced
position, and a commit then waits for one sync on a quorum; a program that records
and acknowledges can do the same with one call.

Harder: a client can make the recorder sync as often as it can send commands, one
`fdatasync` pair each time something is unsynced, on the recorder's thread, where
every other recording's blocks wait for it. The request queue bounds what is in
flight, and nothing bounds the rate but the client. The queue is the one a `start
recording` uses: a client that fills it with syncs has another client's start
answered with code 10 until the recorder has caught up, 16 requests per duty cycle.
`cavalry doctor` already reports a recorder that falls behind its publishers
(`recording-stalled`).

To maintain: one more command in the vectors, the conductor, the recorder and the
client. The C ABI does not take the command in this version of the ABI.

Revisit when the bench measures the recorder's other recordings held up by one
client's syncs, or a start refused behind them: a bound on the rate then belongs in
the driver, not in a key.

## References

- ADR 0025 (the recorder), ADR 0060 (a member's append position, and the sentence
  this builds), ADR 0063 (the consensus module as a client), ADR 0064 and ADR 0065
  (what a failed sync leaves).
- `spec/client-protocol.md`, "Commands", sync recording; "Error codes".
- Card 227.
