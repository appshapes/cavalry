# 0092. Have the member ask for each replay of a replication, so that a recording is armed for its image

- **Status:** Accepted
- **Date:** 2026-11-12
- **Invariants affected:** 1, 3, 4, 6
- **Supersedes:** ADR 0085 and ADR 0086, in one decision each: who starts the next
  replay of a replication that lies in several of the leader's recordings (ADR
  0085), and where the image of a replay after the first must join (ADR 0086).
  "What stands of ADR 0085 and ADR 0086", below, lists the rest, which is unchanged.
  Changes `spec/cluster.md`: row 9 of "Roles and the election", the leader's replays
  in "Messages outside an election", and the rows of `cluster.log_replications` and
  `cluster.join_position_mismatches` in "Counters". No message's layout changes, and
  the page stays at version 0: no member over a driver elects yet (ADR 0081).

## Context

A member that missed a term replicates it from its leader's recordings (state 9).
The leader answers a catch-up position with a replay of the recording that holds
the position. Where the term lies in several of the leader's recordings, ADR 0085
had the leader start the next recording's replay by itself, as soon as the last
`replay stopped` came with reason 0 and the member's reported append position had
reached the position. ADR 0086 had the member check each later image against the
append position it last sent.

Building the member's side over a driver found that the member cannot record the
second image.

**A member's log is its recordings, and a recording begins where it is armed.**
`spec/cluster.md`, "Members and streams", says a member's recording of an image of
stream 100 begins at the image's join position, so it is armed before the image
arrives. `spec/client-protocol.md`, "Commands", start recording, says when a driver
arms: when the stream has a live subscription and **no image**. It holds one armed
recording for an endpoint and stream. A `start recording` on an image that is
already there begins at the image's rebuild position, what the image has received
when the command is handled, and what arrived before lands in no recording
(`crates/cavalry-driver/src/conductor.rs`, `start_image_recording`, and its test
"from the rebuild position").

**The member reports the end before it can arm again.** The member's append
position is what its recording has synced. It reports the last replay's end as soon
as the recording has synced it. The leader has read `replay stopped` by then, and
starts the next replay on that report. The member learns that the image ended only
when its driver has retired the image, after the end of stream has crossed the
network, and only then is the stream without an image.

**Measured once.** On a laptop, with the leader and the member each on a driver of
their own, a leader that started the second replay when the first ended, as ADR
0085 has it: the first replay stopped 8.5 ms after it started; the member's
recording of the first image started at 24 ms; at 42 ms the member's subscription
held both images, and the second had no recording armed for it. A `start recording`
sent then began at the second image's join position, because nothing of it had
arrived yet. That is the order of two threads on one run, and no rule. One run is
not a gate; the rule it shows is the page's.

## Decision

**The member asks for each replay.** A leader answers one catch-up position with
one replay, to the end of the term or the end of the recording that holds the
position, whichever comes first. When that replay has stopped, the leader starts
nothing by itself, whatever the reason. The member asks again.

**When the image has gone.** In follower log replication, a member whose replay's
image has gone from its catch-up subscription, with its append position still
below the stop, removes the subscription. The stop of the image's recording is
then where its log ends. On the next duty cycle it reads its append position. If
that is above the position it last asked from, it subscribes again, which arms a
recording on a stream with no image, and sends catch-up position from its append
position. The leader's next replay finds a recording armed for it.

**A member asks from a position once.** If the member's log ends where it asked
from, the replay brought nothing. It asks no more and subscribes to nothing, and
the progress timer fails its election within the leader heartbeat timeout, counted
in `cluster.log_replications_unavailable`, as a replay that stopped for a fault
did before. So the asks of one replication are at most one for each recording the
leader's term lies in, and one more for each replay that stopped short and had
brought something.

**An image joins where the member asked from.** Every image of a replication is
the answer to a catch-up position, so every image must join at the position the
member last asked from. The member no longer keeps the append position it last
sent for this.

**The leader keeps no continuation.** Of the four things a leader holds for a
member (ADR 0085), "the next recording's replay that it owes, with the position
the last ended at" is no longer entered when a replay ends by itself. It remains
for a request whose start faulted and for the request a member sent while a replay
was stopping.

**The in-memory cluster arms.** `memory::Cluster` models the driver's rule: a
member's `add_catchup` arms one recording, the first image that joins takes it, and
an image that joins a member with none armed brings nothing to its log. A later
change that again sends a member two images for one subscription fails in memory
as it would over a driver.

**Counted as before.** `cluster.log_replications` rises on the member for each
catch-up position that left, now one for each replay, and on the leader for each
replay started. The two rise together.

## What stands of ADR 0085 and ADR 0086

- ADR 0085: the core keeps what a leader serves each member and reaches the
  driver's replays through four operations of `Io`; the replay goes to the endpoint
  the leader's members string names; one replay a member at a time; a catch-up
  position that arrives while a replay runs stops it and is started when it has
  stopped; a canvass position stops the replay and drops the request kept; init
  stops the replays; a start that faults is kept and tried again.
- ADR 0086: a term is recorded when its replication begins; the first image must
  join at the position asked from; the member's two timers; what it does at the
  stop; the four operations of `Io` on the member's side.
- Their Status lines read "Superseded by 0092", the one line an Accepted ADR may
  change.

## Alternatives considered

- **Keep the leader's continuation and have the module record a later image with
  a `start recording` sent when it sees the image.** Lost (invariant 1). The
  recording begins at the image's rebuild position. Whether that is the join
  position depends on which thread ran first. A recording that begins past the join
  leaves a hole, the member's log stops there, and its election fails after the
  leader heartbeat timeout, 10 s, for every recording boundary it lost the race at.
- **Keep the leader's continuation and hold back the member's report of the end
  until it has armed again.** Lost. The member cannot tell the last frame of a
  replay from any other until the end of stream has reached it, and a report held
  back for every frame would stall a member whose replay is waiting for it.
- **Change the driver: arm a recording while every image on the stream is being
  recorded.** Lost for now. It is a change to `spec/client-protocol.md` with a
  protocol version, in another crate, for a need one caller has. The member can
  meet the driver's rule as it is.
- **Replay a term's recordings as one image.** Not possible. A replay is of one
  recording, and its publication has a session id of its own.
- **Ask again without the progress rule.** Lost (invariant 3). A replay that ends
  at once, a segment the leader cannot read, would be asked for again every few
  milliseconds for the leader heartbeat timeout, each one a publication that
  lingers in the leader's driver.

## Evidence

- `crates/cavalry-raft/tests/follower_log_replication.rs`, one member's core driven
  by hand, two tests.
  - A member replicates a term that ends at 64 from position 0. Its replay's image
    joins at 0, brings it to 32 and goes. In that duty cycle the member removes its
    subscription and asks for nothing. In the next it subscribes again and sends
    catch-up position from 32: the calls, in order, are subscribe, ask from 0,
    unsubscribe, subscribe, ask from 32. `cluster.log_replications` is 2. The second
    image joins at 32 with 16 bytes already synced, and the member goes on; a third
    ask is from 48, and an image that joins at 40 fails the election, counted in
    `cluster.join_position_mismatches`.
  - A member asks from 32, its replay's image joins there and goes, and its log
    still ends at 32. In 197 duty cycles it makes no call but the one that removes
    the subscription. At the leader heartbeat timeout its election fails, counted in
    `cluster.log_replications_unavailable`.
- `crates/cavalry-raft/tests/replays.rs`, a leader's core driven by hand. A replay
  from 0 to 64 ends by itself at 32. The member reports 32 and the leader starts
  nothing. The member's catch-up position from 32 starts the replay from 32 to 64 at
  once, with no replay to stop, and `cluster.log_replications` is 2 on the leader.
  `tests/catchups.rs`: a start that faults for one member in every duty cycle does
  not keep the leader from reading the end of another member's replay, so that
  member's next request starts at once; and a request kept after a start that
  faulted is still dropped by a catch-up position the leader cannot serve.
- `crates/cavalry-raft/tests/election.rs`, three members in memory, the safety check
  after every step. A follower is cut off while the leader commits two entries, and
  the other follower holds the first in one recording and the second in another
  (`memory::Cluster::end_recording`). That follower then leads, and the member that
  was cut off replicates the term: two catch-up positions and two replays, each
  counted on both members, no replication counted unavailable, no image that joined
  elsewhere and no failed election, and the member's log is the leader's to the
  term's base byte for byte.
- The same test against the rule this replaces. With `consensus.rs` as it was and
  the in-memory cluster arming as a driver does, the term is "not within 14000 ms"
  replicated: the leader's second replay reaches a member with no recording armed.
  With the leader as decided here and a member that does not ask again, the same
  test and the first test above fail.
- `cargo test -p cavalry-raft`: 146 passed. The 144 of the change before this one
  pass with three of them rewritten for the rule: the leader's next recording, the
  member's later image, and the fault that does not starve.
- Not shown: the member over a driver. The module still refuses the member's four
  operations; its change is the next, and it records each image with the recording
  `add_catchup` armed.

## Consequences

- A replication that lies in N of the leader's recordings costs N catch-up
  positions where it cost one. Each is one consensus message.
- A replay that stops short after it brought something is asked for again at once,
  where it used to wait for the progress timer. A member whose leader's replay
  fails midway on a transient fault goes on.
- The member's side of the module's I/O can now be built on the driver as it is:
  every image it records is one it armed for.
- In Aeron, the follower's archive drives a replication of a recording from the
  leader's archive. This is closer to that than the leader's continuation was.
- Revisit if the driver gains a recording that takes every image of a
  subscription: the member could then stay subscribed through a replication.

## References

- `spec/cluster.md`, "Roles and the election", row 9; "Messages outside an
  election"; "Counters".
- `spec/client-protocol.md`, "Commands", start recording.
- [ADR 0059](0059-create-a-publication-at-a-position-and-arm-a-recording.md),
  the armed recording.
- [ADR 0085](0085-serve-a-replication-from-the-leaders-core-to-the-endpoint-the-members-string-names.md)
  and [ADR 0086](0086-record-a-term-when-its-replication-begins-and-check-each-replays-image-against-the-position-last-sent.md),
  superseded in one decision each.
- [ADR 0091](0091-give-a-leaders-log-up-and-replay-it-over-a-driver-from-the-recordings-the-module-stopped.md),
  the leader's replays over a driver.
