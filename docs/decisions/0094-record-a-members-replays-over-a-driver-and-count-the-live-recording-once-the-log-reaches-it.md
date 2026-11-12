# 0094. Record a member's replays over a driver, and count the live recording once the log reaches it

- **Status:** Accepted
- **Date:** 2026-11-12
- **Invariants affected:** 1, 3, 6
- **Supersedes:** none. No page of `spec/` changes: the four operations are the
  core's `Io`, and what they do over a driver is what `spec/cluster.md` says a
  member does in states 9 and 11 to 13, with commands `spec/client-protocol.md`
  has. It is what the Revisit of ADR 0088 asks for.

## Context

ADR 0081 left the consensus module on the appointed leader until its I/O can do
what an elected member needs. ADR 0091 built the leader's side: giving the log up
and replaying its recordings. This is the member's side: the subscription on its
`catchup` endpoint, the recording of each replay's image, the catch-up position, and
an append position that spans a replay's recording and the live log's. The module
refused `add_catchup`, `catchup_image` and `remove_catchup` as `unsupported` and
sent no catch-up position. Its own core is appointed until the last change of ADR
0081 and asks for none of them.

Two decisions before this one fix its shape.

**A recording is armed for one image, so a member asks for each replay** (ADR
0092). The core subscribes before each catch-up position it sends and removes the
subscription when the replay's image has gone.

**Where a recording begins is the catalog's word, and a recording counts once the
election was told its image** (ADR 0093).

ADR 0088 left two questions to this change: whether `Io::log_image` can give the
live image's join position while the catch-up subscription is still there, and
whether `Io::append_position` can be the end of what the two recordings hold
together.

## Decision

**`add_catchup` is two commands on a stream with no image.** It subscribes to
stream 100 on the member's own `catchup` endpoint and arms a recording there. The
core calls it with no catch-up subscription held and before the catch-up position
leaves, so no replay it asked for can arrive first.

**The catch-up position carries the member's own endpoint.** The module sends
catch-up position with the member's id and its `catchup` endpoint, `host:port`, as
its own members string has it. It encodes the message into its scratch, 1,408
bytes: with a host name of the length DNS allows the message is longer than the
128 bytes the other consensus messages are encoded into.

**`catchup_image` speaks of the recording, not of the subscription's images.** It
returns where the member's recording of the replay's image begins, from the
catalog, once the driver has started that recording. It returns nothing once the
recording has stopped, which the driver does when the image goes. It returns the
start once at least: a replay short enough to come and go between two duty cycles
has a recording that stopped before the election first asked, and the election
must still check where it began, count its bytes and see it gone. The module reads
the image of the recording's session each duty cycle, so that the replay is not
held back and its image can go when it has ended. An image of another session on
the endpoint, a replay the armed recording did not take, is never read.

**`remove_catchup` stops the recording, then removes the subscription.** The stop
is where the member's log ends when the election was told the recording and it
began where the log ended. The recording is then kept for the replays this member
serves when it leads (ADR 0091).

**The append position is the end of three things in order.** The recordings the
member has stopped; then its recording of a replay, when it was told and begins
where those end; then its recording of the live log, when it was told and the end
so far has reached where it begins. In follower catch-up the live recording begins
ahead of the member's log, and counts from the duty cycle in which the replay's
recording has reached its start. Each recording that counts is asked to sync as
its position moves.

**A live recording that began below the log's end never counts.** The module notes
where the member's log ended when the member subscribed on its `log` endpoint. A
recording of the live log that began below that is of an image that joined below
the member's append position: follower log await fails on it, and its bytes are of
another log. With a catch-up the live recording must be allowed to begin before the
end of the stopped recordings moves past it; this floor is what keeps the rule of
ADR 0093 for the other case. The floor is taken at the subscription and not when
the driver hands the recording over. In a catch-up the replay goes on between the
two, and may carry the member's log past where the live recording begins: a floor
taken at the hand-over would then shut out the very recording the member needs,
and its append position would stop when the leader stops the replay.

**A member that leaves the log in a catch-up stops the replay's recording first.**
Init leaves the log and then removes the catch-up subscription. The live recording
begins inside what the replay's recording holds, so `leave_log` stops the replay's
recording first, keeps it, and then keeps the live one after it. The log ends at
the further of the two stops. The table of kept recordings then holds two that
overlap, in the order they begin, and a replay of this member's own takes the one
that begins first where both hold a position, as `spec/cluster.md` has it.

**`log_image` is not changed.** It reports the live recording's start whatever the
catch-up subscription holds: the two subscriptions are on two endpoints.

**The module names no recording while it catches up.** `log_recording_id` is the
live recording once it counts, and nothing before. A service's read across a
member's recordings is not in this version (`spec/cluster.md`, "The service").

## Alternatives considered

- **Report the image from the subscription's list of images, as `log_image`
  does.** Lost. A short replay's image can come and go between two duty cycles,
  and an image of another session can be first in the list. The recording says
  which image the member holds and when it ended.
- **One structure for both subscriptions.** Not now. The live log's image is told
  by its session and watched for its loss, and the replay's is not; what the two
  share is the hand-over of an armed recording, which is one function.
- **Count the live recording only after the catch-up subscription is removed.**
  Lost. The member must report a position at or past the live recording's start
  with the catch-up flag before the leader stops the replay (ADR 0088), and it
  holds the bytes.
- **Let `remove_catchup` fail when the log was left first.** Lost. Init calls the
  two in that order, and a failed catch-up must still leave the member with what
  both recordings hold.
- **Send the catch-up position in 128 bytes and refuse a longer endpoint at the
  member's start.** Lost. A members string that parses would stop a member from
  ever replicating, and nothing would say so.

## Evidence

- `crates/cavalry-raft/src/module/tests.rs`, eight tests. In seven, the leader and
  the member are each a module on a driver of its own, and the test drives the `Io`
  of both.
  - A term in two of the leader's recordings, 0 to 192 and 192 to 288. For each,
    the member subscribes and sends catch-up position; the leader reads it on its
    consensus stream with the member's id and its `catchup` endpoint; the leader
    replays the recording that holds the position; the election is told where the
    member's recording begins, 0 and then 192, which is the catalog's start; the
    append position reaches the recording's end; the image is said to have gone;
    and the member removes the subscription and keeps the recording. At the end it
    holds the two recordings, 0 to 192 and 192 to 288, and names no recording.
  - A catch-up with the live add. The member records a replay that follows the
    leader's live recording, to 192, then subscribes on its `log` endpoint while
    the replay runs. The election is told the live recording's start, at or past
    192. A third entry moves the append position to 288. The leader stops the
    replay, the member removes its catch-up subscription, and a fourth entry
    moves the append position by the live recording alone. The member that leaves
    keeps both recordings, the replay's first; a position both hold finds the
    replay's, and one past the replay's stop finds the live one.
  - A member that leaves the log in a catch-up, as init does: the replay had
    stopped at the first entry's end and the live recording holds a second entry.
    After `leave_log` and then `remove_catchup` the append position is the second
    entry's end and both recordings are kept in order.
  - A replay that ended before the election asked: the recording has stopped, the
    append position is still 0, the first answer of `catchup_image` is 0, the
    append position is then the entry's end, and the second answer is nothing.
  - A live recording that begins below the member's log end: the member holds 96
    bytes and leaves; a log of another term begins at 0; the election is told 0;
    the recording syncs past 96; the append position stays 96, no recording is
    named and nothing more is kept.
  - A replay whose sender vanished: the leader's module and its driver are
    stopped while a replay runs. The image is said to have gone 9.996 s later in
    one run, the image liveness timeout; the test holds it to 9 s at least. What
    the replay had brought is the member's log, and the recording is kept.
  - A live recording handed over late. The member subscribes on its `log`
    endpoint, the leader appends a second entry, the replay brings it, and only
    then does the member look at its log subscription: the live recording begins
    at or past 96 and below 192, where the member's log ends by then. The replay
    is stopped, and a third entry moves the append position by the live recording
    alone. With the floor taken at the hand-over, as this change first had it,
    this test fails: the append position stays at 192.
  - A catch-up position with a host name of 240 characters does not fit 128 bytes
    and fits 1,408.
- Each rule taken out fails the test written for it: the floor; a live recording
  held to begin exactly where the stopped ones end; an image said to be gone as
  soon as its recording stopped, told or not; `leave_log` without stopping the
  replay's recording first.
- The twenty in-crate tests ran ten times in a row with no failure on a macOS
  laptop, about 11 s a run, which is the image liveness timeout of one test. `cargo test -p cavalry-raft`: 157
  passed. The tests over three drivers, `cluster`, pass unchanged.
- Not shown: a member's whole election over drivers, which comes when the module
  moves (ADR 0081); and a stale sender on either endpoint, which Consequences
  leaves to the fault-injection tests.

## Consequences

- The module refuses one operation, `truncate_log`. It is the next change.
- A member over a driver can replicate a term it missed and catch up within its
  leader's term, once its core elects. Nothing asks it to until the module moves.
- A deposed leader that cannot hear a member may go on sending to that member's
  `log` endpoint. Its image can take the recording armed there, and the join then
  waits for the leader heartbeat timeout before the member begins again (ADR
  0093). The same holds for a stale replay on the `catchup` endpoint, which the
  join position check fails at once when it began elsewhere. Revisit with the
  fault-injection tests of ADR 0077: if a stale sender can hold a member out for
  more than one timeout, the member needs to tell the driver which session to
  record.
- A replay's image is said to have gone when the member's recording of it has
  stopped, which needs the end of stream to reach the member or the image
  liveness timeout to pass. That timeout and the leader heartbeat timeout are both
  10 s. So when no end of stream arrives, the member's progress timer fires no
  later than the image goes, and the member fails its election, counted, and does
  not ask again. That is right when the leader is gone, which is the case a test
  here makes. A leader that lives and whose replay's end of stream is lost costs
  the member one election. Revisit if the fault-injection tests of ADR 0077 meet
  it.
- The kept table can hold recordings that overlap. `truncate_log` must cut or
  forget each one that holds bytes above the position, as its doc comment says.

## References

- `spec/cluster.md`, "The log"; "Roles and the election", rows 9 and 11 to 15;
  "Messages outside an election".
- `spec/client-protocol.md`, "Commands": add subscription, start recording, stop
  recording.
- [ADR 0086](0086-record-a-term-when-its-replication-begins-and-check-each-replays-image-against-the-position-last-sent.md),
  the member's four operations of `Io`.
- [ADR 0088](0088-catch-up-within-a-leaders-term-and-join-the-live-log-from-the-replay.md),
  whose Revisit this answers.
- [ADR 0091](0091-give-a-leaders-log-up-and-replay-it-over-a-driver-from-the-recordings-the-module-stopped.md),
  [ADR 0092](0092-have-the-member-ask-for-each-replay-of-a-replication-so-that-a-recording-is-armed-for-its-image.md)
  and
  [ADR 0093](0093-take-where-a-members-recording-of-the-log-begins-from-the-catalog-and-count-it-once-its-image-is-checked.md).
