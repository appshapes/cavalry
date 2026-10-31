# 0036. Hand the replayer a recording's stop and bound the replay publications

- **Status:** Accepted
- **Date:** 2026-10-31
- **Invariants affected:** 1, 3, 6
- **Supersedes:** none; amends ADR 0030 in two places and settles three notes from
  pull request 64's review (card 160)

## Context

Two notes the reviews of pull request 61 left standing. First, a replay of a live
recording learns the recording's stop from its catalog slot, read once a second, and
until then reads the extent from the recording's `recording.synced_position` counter,
a handle the conductor gave it at the start. The conductor frees that counter when the
recorder reports the stop, and a freed counter record is reusable after the counter
reuse timeout, one second. A replayer whose duty cycle was late, parked or writing a
1 MiB block, could read the next slot refresh as not yet due and the counter as another
counter's value, above the stop; standing exactly at a segment boundary, where the
recorder removed the segment file it had just rolled into, it would roll into the
removed file and end the replay with reason 3, a failure, for a recording that stopped
cleanly. Second, a replay leaves the 64-slot replays registry when it ends, but its
publication lingers until every subscriber has reached the end of stream or the
publication linger has passed, so replays that start and end under the linger leave
publications behind that no registry slot counts, and the `publications` list, built
with a capacity, could grow past it: a collection in the driver with no bound (invariant
3). A removed client publication follows the same pattern with the registration count as
its bound; the replay publications had none.

Three notes from pull request 64's second review travel with them: `MAX_REPLAYS` lived
twice, in `cavalry-client` and in `cavalry-driver`, both 64, with the client's error and
the how-to's "64" to move with it; the client's error for its own full replay
registration queue was `ClientError::Driver`, whose text says "the driver answered
with", for a queue the driver never saw; and `--from` had no spelling for a recording's
start position when the recording began above 0, other than an instant before its first
index entry.

## Decision

When the recorder reports a recording's stop, the conductor hands the replayer the stop
position in a request of its own, `RecordingStopped`, sent once per recording stop
when a registered replay plays that recording. The replayer makes it the final extent of
every replay of the recording that still followed the synced counter, in the duty cycle
that takes the request, before any read, so the replay ends at the stop position with
reason 0 without trusting the counter past it. The catalog slot is still read once a
second and on a zero frame length, as a second source for a stop the hand-off missed.
The request is one more deferred request per registered replay, so the replayer's
deferred list holds three per replay slot.

The replay publications have a bound of their own, `MAX_REPLAY_PUBLICATIONS`, twice
the replays registry: 128, counting every replay's publication, live or lingering after
its end, together with the replays still starting whose publication is to come. A
`start replay` that would pass it is refused with error code 10, naming the replay
publications, and counted in `driver.registry_full` as every registry refusal is; it is
admitted again once a lingering publication has closed. The `publications` list is
built for the publications registry plus this bound and never grows.

`MAX_REPLAYS` has one definition, in `cavalry-proto`, which the driver's `spec` and the
client's conductor take. The client's full replay registration queue is
`ClientError::ReplayQueueFull`, reason `replay-queue-full`, naming the queue's own
bound and saying nothing reached the driver. `cavalry replay --from start` names the
recording's start position, sent as the null position the driver resolves; `--to start`
is refused before the command is sent.

## Alternatives considered

- **Keep the catalog read as the only source of the stop, and refresh it before every
  read of the synced counter.** Lost: a catalog read per block is a syscall per block on
  the replay's path for a case that is the conductor's to know; the conductor holds the
  recorder's reply and the replay's entry, and a bounded request costs nothing per
  block.
- **Let the replayer hold the recording's stop through the counter by never freeing
  it while a replay plays the recording.** Lost: it ties a recording's counter lifetime
  to the replays of it, which the counters file's bound does not account for, and the
  catalog read would still be needed for a replay that started after the stop.
- **Bound the replay publications by closing a lingering one at once when the
  registry is full.** Lost: the linger exists so that a subscriber reads to the end of
  what it was shown (`spec/log-buffer.md`, "Closing"); cutting it for a new replay trades
  one client's delivery for another's start. A refusal with code 10, which the client
  retries once a linger has passed, keeps invariant 1 for the subscriber already there.
- **A counter of its own for the replay publications refusal.** Lost: every registry
  refusal is `driver.registry_full` with the error message naming the registry, and a
  per-registry breakdown is a change to all of them, not to this one.
- **`--from 0` as the spelling of the start.** Lost: the command sends 0 as the start
  position only when the recording started at 0; a recording that started later refuses
  a position before its start with code 23, which is the gap `start` closes.

## Evidence

- `mod replay` in `crates/cavalry-driver/src/conductor.rs`: a recording stopped exactly
  at a segment boundary, 128 MiB of 64-byte frames, with a replay following it at the
  boundary; the recorder removes the second segment file at the stop, and the replay
  ends with reason 0 at the boundary within the stop's own cycles, `replay.failed` 0,
  `log_buffer.rejected_frame` 0. The live-follow test now ends on the stop within five
  cycles where it waited for the second's catalog read before.
- The same module: 128 replays of one recording started and ended under a subscription
  that never polls, so 128 publications linger; the 129th `start replay` is refused with
  code 10 naming the replay publications registry at 128, `driver.registry_full` one
  up, nothing registered; after the linger the publications close and a start is
  admitted. The deferred-request test fills the replayer's deferred list with one
  attach and one stop per replay, then one recording stop for all of them, within the
  capacity and without growing it.
- `cargo test -p cavalry --bin cavalry`: `--from start` parses to the start point and
  `Start` is not a position.
- Histograms before and after on the laptop in the commit; the hosts' jobs compare the
  pull request against their baselines. The change adds one request type on the
  conductor's side of the replayer's queue and a field on the publication; no message
  path changes.

## Consequences

Easier: a replay of a live recording ends on the recorder's word within a duty cycle,
not within a second, and never on a counter another holder has reused; every
collection the replayer's and the conductor's replays touch has a bound with a test at
it; the replay sizes move in one place. Harder: a client that starts replays faster
than their subscribers read them will see code 10 on the 129th within five seconds and
must wait or stop them, which the error says; the replayer's deferred list is 192
entries where it was 128.

Revisit: when 64 live replays are not enough (ADR 0030), when the replay publications'
bound should follow a different ratio than two per slot, and when ADR 0031's truncate
reopens a stop, which must reach the replayer by the same request.

## References

- `crates/cavalry-driver/src/conductor.rs` (`on_stopped`, `start_replay`, the
  `Publication::replay` field), `crates/cavalry-driver/src/replayer.rs`
  (`Request::RecordingStopped`), `crates/cavalry-driver/src/spec.rs`
  (`MAX_REPLAY_PUBLICATIONS`), `crates/cavalry-proto/src/client_protocol.rs`
  (`MAX_REPLAYS`), `crates/cavalry-client/src/error.rs` (`ReplayQueueFull`),
  `crates/cavalry/src/main.rs` (`Point::Start`)
- `spec/client-protocol.md`, "start replay", the replayer paragraphs and "Sizes";
  `docs/how-to/replay-a-recording.md`
- ADR 0030, ADR 0035; card 160; pull requests 61 and 64
