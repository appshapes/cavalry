# 0093. Take where a member's recording of the log begins from the catalog, and count it once its image is checked

- **Status:** Accepted
- **Date:** 2026-11-12
- **Invariants affected:** 1, 3, 6
- **Supersedes:** none. Adds three clauses to `spec/cluster.md`: in "Members and
  streams", where a member takes a recording's start from; in "The log", that a
  recording of an image with another session id is no part of a member's log; and
  in row 15 of "Roles and the election", that the join position checked is where
  the member's recording of the image begins. No message's layout changes, and the
  page stays at version 0.

## Context

A follower joins its leader's log with two commands: `add subscription` on its `log`
endpoint, and `start recording`, which arms a recording on that stream (ADR 0059,
ADR 0060). The driver arms only on a stream that has no image. An image that arrives
between the two commands is already there when the second is handled, and the
recording then begins at the image's rebuild position, what the image has received
by then (`spec/client-protocol.md`, "Commands", start recording). `spec/cluster.md`,
"Members and streams", says why the recording is armed first. It did not say what a
member does when the image came first.

The consensus module did not know. It took the image's join position, as its own
subscription saw it, for where the recording begins. It told the election that
position, and it counted the recording's synced position as contiguous from there.
`recording ready` carries no start position.

**Shown** (card 269). Two members, each a module on a driver of its own. The
follower subscribes and does not arm. The leader appends an entry of 96 bytes, and
the entry reaches the follower's driver. Then the follower arms. After a second
entry the catalog says the follower's recording begins at 96. The module had noted
0, told the election 0, and reported an append position of 192. The follower said
it held the log from 0 to 192, and its only recording holds 96 to 192.

**When it can happen.** The image must arrive, and data after it, between the two
commands. At a cold start no entry is appended before the followers have joined, so
a recording armed late still begins at the join position. It needs a follower that
joins while its leader appends: one that joins after the leader closed its election
with a quorum, or one that comes back level with the leader, which appends inside
the gap. Follower catch-up (state 13) adds the live log while the leader appends,
so there it would be common; no member over a driver catches up yet (ADR 0081).

**A second gap of the same kind.** An armed recording takes the stream's next image
on the endpoint, whatever its session id. The election asks for the image of one
session, the leader's (`Io::log_image`). The module counted any recording that
began where the member's log ended, whether or not the election had been told its
image. With one appointed leader a member's `log` endpoint receives one session.
With elections it can receive two: a leader that was deposed and cannot hear it
goes on sending.

## Decision

**The catalog says where a recording begins.** The module opens the driver's
catalog in its log directory when the member starts. When the driver hands over a
recording of the log, the module reads the recording's start position from its
descriptor and holds the recording with that start. The image's join position, as
the subscription saw it, is no longer used for anything but telling the image from a
later one.

**The election is told the recording's start.** `Io::log_image` for a session
returns nothing until the member's recording of that session's image has started,
and then returns where that recording begins. A recording that began past the join
is then an image that joined past the member's append position: follower log await
counts a join position mismatch, the member's election fails, and init leaves the
log. A member whose leader is appointed joins again; one that elects catches up.

**A recording counts once its image is checked.** The module counts a recording in
`Io::append_position`, and keeps it when the member leaves the log, only after
`Io::log_image` has returned its start for the session the election asked for. A
recording of another session's image is never told to the election, so it is never
counted and never kept: it is stopped when the member leaves the log and stays in
the driver's catalog.

**A catalog that cannot be opened stops the member at its start.** The module's log
directory must be its driver's, as `ConsensusModule::start` has always said. A
directory with no catalog is `RaftError::Catalog`, whose message says to start the
member on its driver's log directory. The module opens the catalog before the
`terms` file, which it creates when there is none, so a directory it refuses is left
as it was. A recording whose descriptor cannot be read when it is handed over is the
same error from that duty cycle; the module holds the recording and reads again on
the next.

**A slot that does not hold the recording gives no start.** The driver writes a
recording's descriptor before it says the recording started, so the slot of the
driver's catalog holds the recording when the module reads it. A slot that is empty,
or that holds a recording of another session id or stream id, is in a catalog that
is not the driver's, and its start position would be another recording's, which is
the fault this decision removes. The module takes it as `RaftError::Catalog` with
the catalog's `unknown`, as it takes a slot the catalog does not have: the election
is told nothing, nothing is counted, and the next duty cycle reads again.

## Alternatives considered

- **Read the recording's position counter when it is handed over.** Lost. The
  driver sets the two counters to the start position before it sends `recording
  ready`, but the recorder may have written and synced by the time the client reads
  them.
- **Add the start position to `recording ready`.** Lost for now. It is a change to
  `spec/client-protocol.md` with a protocol version, for a value the catalog
  already holds durably.
- **One command that subscribes and arms.** Lost for the same reason, and the gap
  would still be there for a client of an older version.
- **Subscribe, wait until no image is there, then arm.** Not possible. The member
  cannot make a leader stop sending.
- **Count the bytes from the recording's start and report the hole.** Lost. A
  member's log is contiguous from position 0 (`spec/cluster.md`, "The log"); what
  lies past a hole is never counted, and the member that joins again records the
  image from its join.
- **Tell the election the image's join position and fail later.** Lost. The
  election would close on a member that holds nothing of what it reports.

## Evidence

- `crates/cavalry-raft/src/module/tests.rs`, three tests over two drivers: the
  leader and the follower are each a module on a driver of its own, and the test
  drives the `Io` of both.
  - Armed late. The follower subscribes, the leader's first entry, 96 bytes,
    reaches the follower's driver, and then the follower arms. The module tells
    the election 96, which is the start position the catalog holds, where the
    image joined at 0. While the recording syncs a second entry, to 192, the
    member's append position stays 0 and the module names no recording. The
    member leaves the log and keeps nothing. On the code before this change the
    same steps gave: start noted 0, `log_image` 0, append position 192.
  - Armed first. The follower subscribes and arms before the leader's log exists.
    The election is told 0, the catalog's start; the append position reaches the
    first entry's end; the module names the recording; and the member that leaves
    keeps it from 0 to 96.
  - Another session. A publication of the test's own on the follower's `log`
    endpoint takes the armed recording, which begins at 0, where the member's log
    ends, and syncs an entry. Asked for another session's image the module says
    nothing; the append position stays 0, no recording is named, and nothing is
    kept.
- Two tests of a directory that is not the driver's.
  - No catalog. A member started on a directory with no catalog is refused with
    `RaftError::Catalog`, reason `catalog`, its message naming "Members and
    streams", and the directory is still empty: no `terms` file was written.
  - Another driver's catalog. The leader's driver holds a recording of another
    stream in slot 0. With that catalog in the member's hands, the hand-over of the
    member's recording 0 is `RaftError::Catalog` with `unknown` on every duty
    cycle, the election is told nothing and the append position stays 0. With its
    own driver's catalog the member takes the recording on the next duty cycle and
    tells the election 0.
- Each rule taken out. With a recording's start taken to be where the member's log
  ends, the first test fails. With a recording counted before its image is told,
  the third fails. With the `terms` file opened first, the test of no catalog
  fails; with the slot's session id and stream id not compared, the test of
  another driver's catalog fails.
- `cargo test -p cavalry-raft`: 151 passed. The tests over three drivers, `cluster`,
  pass unchanged: their followers arm before the leader appends.
- Not measured: how often the gap is met. It needs an image and data after it
  between two commands a few hundred microseconds apart.

## Consequences

- A follower whose image arrived before it armed fails one election, counted in
  `cluster.join_position_mismatches`, and joins again. Before, it reported bytes it
  did not hold.
- `Io::log_image` reports an image a few duty cycles later than it did: when the
  driver has said the recording started. Follower log await waits for it within the
  leader heartbeat timeout, as it waits for the image.
- The module reads the catalog once for each recording of the log it is handed. It
  is a file read with an allocation, at a join, never once an entry.
- A recording that began past the join, and one of another session, stay in the
  catalog with nothing that refers to them. Retention removes them as any recording.
- The member's catch-up subscription (the next change) uses the same two rules for
  the recording of a replay's image.
- Not caught: a catalog of another driver whose slot holds a recording of the same
  session id and stream id, which is what another member's recording of the same
  log is, in a cluster whose members run on one host with their log directories
  exchanged. Its start position can differ from this member's. Such a member also
  reads and writes another member's `terms` file, which no check of this decision
  sees. Telling the two apart needs the driver to say which log directory is its
  own, or the start position in `recording ready`.
- Revisit if `recording ready` gains the start position: the module then needs no
  catalog for this.

## References

- `spec/cluster.md`, "Members and streams", "The log", and row 15 of "Roles and the
  election".
- `spec/client-protocol.md`, "Commands", start recording; `spec/durable-log.md`,
  "Descriptor".
- [ADR 0059](0059-create-a-publication-at-a-position-and-arm-a-recording.md), the
  armed recording.
- [ADR 0060](0060-replicate-the-log-with-an-appointed-leader.md), a follower's
  recording as its log.
- [ADR 0092](0092-have-the-member-ask-for-each-replay-of-a-replication-so-that-a-recording-is-armed-for-its-image.md),
  the same rule of the driver met on the catch-up endpoint.
