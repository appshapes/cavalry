# 0082. Give up the lead on a higher term or a lost quorum, and count a follower's election on a higher term

- **Status:** Accepted
- **Date:** 2026-11-11
- **Invariants affected:** 1, 4, 6
- **Supersedes:** none. Adds one row to `spec/cluster.md`, "Counters",
  `cluster.higher_terms_seen`, and names it in "Messages outside an election". Adds
  one clause each to the row of state 8, to the row of `cluster.leader_quorum_losses`
  and to the row of the leader heartbeat timeout in "Values"; each says of leader
  ready what the page's paragraph "The appointed leader" already implied. The page's
  version stays 0: no message changes, and no member over a driver takes a vote yet
  (ADR 0081).

## Context

ADR 0081 put the election on a path of its own in the core,
`Consensus::with_election`, and listed its steps. This is the second: the elections
a leader holds. `spec/cluster.md` has a leader hold an election when its active
members fall below a quorum ("The commit position"), fail its election on a canvass
position with a term above its own in leader log replication and leader ready
("Messages during an election"), and, once its election has closed, hold one on a
canvass position, a request vote or a commit position with a term above its own
("Messages outside an election"). Until this change a member on that path did none
of it. A leader that was cut off, or whose process stood still, came back as a second
leader of an older term and stayed one.

Building it found four things that the page and the earlier decisions did not settle.

**The page names no counter for a follower's election on a higher term.** "Messages
outside an election" gives the rule to any member whose election has closed, and
counts it "in `cluster.leader_step_downs` when it was the leader". A follower that
reads a request vote or a commit position of a higher term moves its election from
closed to init too. Invariant 4 asks a counter of every move, and
`crates/cavalry-raft/tests/transitions.rs` fails a move that has none.

**The row of state 8 does not say what a leader in leader ready does without a
quorum.** The row of state 5 has the leader fail its election. "The commit position"
says that a leader whose active members fall below a quorum holds an election, and
names no state. The row of the leader heartbeat timeout says "in state 5 as outside
one". "The appointed leader" says that the appointed leader stays the leader "in the
state it is in, leader log replication and leader ready among them, where the
election's leader fails its election". So the page implies the rule for leader ready,
and one row and one clause leave it out. It matters: a leader cut off in leader ready
that did nothing would stay there after the cut ended. Its followers elect another
leader, no member canvasses to it, and a leader reads no new leadership term and,
while its election is open, no commit position.

**The core cannot give up a leader's log.** `Io` has `leave_log`, for a follower. A
leader that begins again at init holds the publication it added for its term and its
recording of it. The followers' images of that publication would go on, and pull
request 210 made the module refuse a second term while it holds the first's
publication.

**The role has to move with the election.** The reviews of pull request 213 found a
candidate whose election failed and that read candidate for one more cycle. A leader
that holds an election has the same shape: its election reads init from one call, and
its role would read leader until the next.

## Decision

A member that elects holds the elections the page gives a leader, and a follower's on
a higher term, with one new counter, one clause for leader ready and one method of
`Io`. The appointed leader's path is unchanged.

**What moves the member, and what counts it.**

| The member | Reads or finds | Does | Counted in |
| --- | --- | --- | --- |
| a leader, its election closed | a canvass position with either term above its own | holds an election, beginning at init | `cluster.leader_step_downs` |
| a leader, its election closed | a request vote whose candidate term id, or a commit position whose leadership term id, is above its own term | the same | `cluster.leader_step_downs` |
| a follower, its election closed | the same request vote or commit position | the same | `cluster.higher_terms_seen` |
| a leader in leader log replication or leader ready | a canvass position with either term above its own | fails its election, beginning again at init | `cluster.elections_failed` |
| a leader, its election closed | its active members below a quorum | holds an election, beginning at init | `cluster.leader_quorum_losses` |
| a leader in leader log replication or leader ready | its active members below a quorum | fails its election, beginning again at init | `cluster.leader_quorum_losses` and `cluster.elections_failed` |

A leader in leader replay or leader init updates its table alone on such a canvass
position, as the page says. A member whose election has closed answers a request
vote with nothing and keeps its candidate term, as the `request-vote` vectors pin.
The arithmetic is `cavalry-proto`'s, `canvass_position_action` and `request_vote`,
whose vectors already hold every case: the core did not act on their answer before.

**`cluster.higher_terms_seen`.** It counts a follower whose election had closed that
held an election on a request vote whose candidate term id, or a commit position
whose leadership term id, was above its own term: a term the cluster began without
it. The name says the cause, as `cluster.leader_heartbeat_timeouts` and
`cluster.log_images_lost` do for the other two ways a follower's election begins
from closed. `cluster.leader_step_downs` is registered as the page has it.

**Leader ready fails on a lost quorum, as leader log replication does.** The row of
state 8, the row of `cluster.leader_quorum_losses` and the row of the leader
heartbeat timeout now say so. A lost quorum in either state adds one to both
counters, since it is a quorum lost and an election that failed.

**`Io::remove_log`.** A member in init that holds a log gives it up before it
canvasses: the publication closed and the leader's recording of it stopped, as init
has a follower leave the log. The term it leads next gets a publication of its own
at that term's base. A fault leaves the member in init, and the next duty cycle
calls again. The in-memory cluster closes the member's publication, and every
follower's image of it is gone from that step. The consensus module refuses the call
with `RaftError::Unsupported`: its member is appointed until the module moves to the
election (ADR 0081), an appointed leader holds no election, and so its core never
asks. The module's own `remove_log`, which closes the publication, stops the
recording and takes the stop as the log's end, comes with the rest of the module's
I/O for the election, before that move.

**The role moves in the same call.** A leader that holds an election, or whose
election fails, is a follower from the call that moves its election to init. The
machine `cluster_role` gains leader to follower, listed with the counter of each of
its three causes; `cluster_election` gains seven rows, three from closed to init and
two each from leader log replication and leader ready to init.

**Another member's message alone.** A request vote or a commit position from an id
that is no other member's, the member's own among them, holds no election, as the
core reads no consensus message from such an id (ADR 0081).

**What this step leaves as it was.** A member that gave up the lead joins the new
leader only when its log is level with the new leader's: one that holds an entry no
quorum recorded needs the truncation, and one behind the new leader's log needs the
catch-up. Until those steps it fails its election, counted in
`cluster.elections_failed`, and canvasses again every election status interval. The
section "What the election does not do yet" of `crates/cavalry-raft/README.md` says
so. Leader ready still waits for every member heard (ADR 0081).

## Alternatives considered

- **Count a follower's election on a higher term in `cluster.leader_step_downs`.**
  Lost. The counter would rise on a member that never led, and `cavalry doctor`'s
  text for it sends the operator to a leader that stood still.
- **Count it in `cluster.leader_heartbeat_timeouts`.** Lost. No timeout ran: the
  follower's leader may be alive and heard, and that counter's text says to check
  that the leader is running.
- **Count it in no counter of its own, since init counts `cluster.elections`.** Lost.
  The move from closed to init would have no counter, which the transitions test
  refuses, and the cause of the election would be in no counter at all.
- **Leave leader ready with no rule for a lost quorum, and have a leader in leader
  ready begin again on a commit position of a higher term.** That is Aeron's rule:
  `Election.onCommitPosition` raises "new leader detected due to commit position" in
  `LEADER_READY`, and its `leaderReady` has no quorum check. Lost for this change.
  The page already implied the quorum's rule for leader ready, in "The appointed
  leader", and that rule frees the same leader at most one leader heartbeat timeout
  later: a quorum elects another leader only after that long without this one, and
  in that time no member reports to it. A rule for a commit position read during an
  election is new text in "Messages outside an election"; a fault test over drivers
  that shows the ten seconds matter is the reason to add it.
- **Give up the log when the member next leads, in `add_log`.** Lost. The old
  publication would stay open for as long as the member follows, with a recording
  that goes on, and a follower's image of it would end only by the image liveness
  timeout.
- **Use `leave_log` for both.** Lost. The two are different work for the module: a
  follower drops a subscription and holds the recording the driver armed until its
  stop is read; a leader closes a publication and stops a recording it started
  itself. One method that means one or the other by the member's role hides that.
- **Build the module's `remove_log` in this change.** Lost. ADR 0081 puts the
  module's I/O after the core's steps, each testable in `module.rs` then; nothing
  calls it while the module's leader is appointed, and this change is near the size
  one review holds.

## Evidence

- `crates/cavalry-raft/tests/step_down.rs`, seven tests on one core through an `Io`
  that remembers what it was asked. A closed leader answers a canvass position with
  neither term above its own, and on one with either term above is a follower in
  init from the same call, with nothing sent. It does the same on a request vote and
  on a commit position of a higher term, sends no vote and stores no term, and moves
  on neither a message of its own term nor one from an id that is no other
  member's. A follower counts `cluster.higher_terms_seen` on the same two messages.
  A leader in leader log replication or leader ready fails its election on the
  canvass position, and in leader replay and leader init does not. A leader whose
  members last reported 9,999 ms ago leads, and at 10,000 ms holds an election; the
  appointed leader, with the same silence, counts the loss and leads. A leader gives
  its log up in init once, again after a fault, and adds a new one for the term it
  leads next, after that term is stored and recorded. A member whose leader is
  appointed holds no election on a higher term.
- `crates/cavalry-raft/tests/transitions.rs` makes every row of both machines, the
  41 of `cluster_election` and the 10 of `cluster_role`, and checks after every call
  that each move is a registered row and that a counter registered for it rose.
- `crates/cavalry-raft/tests/election.rs`, three members in memory, at seeds 1, 2, 3
  and 12, with the check after every step that no committed entry is lost and no two
  members lead one term. A leader cut off from both followers gives up the lead
  9,802 ms after the cut, when its followers' last reports are 10 s old, by the loss
  of its quorum. An entry it appends in that time reaches no other member and is
  never committed. The other two elect one of themselves from 44 ms to 474 ms after
  it gave up, and commit. A leader whose process
  stood still for longer than the leader heartbeat timeout is a follower one step
  after it runs again, at each seed by the loss of its quorum. After either, the old
  leader fails an election every election status interval, twenty in 2 s, as "What
  this step leaves as it was" says.
- Every test that was on `master` passes unchanged.

## Consequences

- A leader that was cut off leads its term for at most the leader heartbeat timeout
  after its followers' last reports, and one that stood still for one duty cycle
  after it runs again. In that time it commits nothing that a quorum has not
  recorded: its commit position is the position a quorum of members has reported,
  and none reports to it.
- In a cluster of three in memory, every leader that gives up the lead does so by the
  loss of its quorum. A quorum elects without a leader only after ten seconds of its
  silence, and the in-memory cluster's partitions cut both ways, so the leader's own
  check comes first. A partition that cuts one way, a leader that hears and is not
  heard, is the fault that brings it a request vote first. The step-down on a higher
  term is tested on one core until the in-memory cluster has that fault.
- `Counts` has seventeen counts, and a member's module adds seventeen client
  counters; six of them stay 0 while the module's leader is appointed.
- The in-memory cluster takes a follower's image away in the step in which its leader
  gives the log up, where a transport takes the image liveness timeout at most. A
  test of that wait belongs to the tests over drivers.
- Revisit: when the module's I/O for the election is built, at which
  `ClientIo::remove_log` stops refusing; and if a fault test shows a leader in leader
  ready held by the ten seconds of the second alternative.

## References

- `spec/cluster.md`, "The commit position", the rows of states 5 and 8, "Messages
  during an election", "Messages outside an election", "Counters" and "Values".
- [ADR 0081](0081-build-the-election-beside-the-appointed-leader-and-move-the-module-to-it-once.md),
  the electing path, its steps and the module's move to it.
- [ADR 0077](0077-elect-the-leader-by-vote-and-close-a-ballot-on-the-members-heard.md),
  the election and what card 228 builds of it.
- [ADR 0067](0067-register-the-elections-state-machine-with-its-core-and-count-each-step.md),
  the election's state machine in `counters.toml`.
- [ADR 0058](0058-specify-the-cluster-protocol-from-aerons-codecs.md), the protocol,
  Raft's step-down and the departures from Aeron.
- Aeron's `Election.java`, `onCommitPosition` and `leaderReady`, read on 2026-11-11:
  <https://github.com/aeron-io/aeron/blob/master/aeron-cluster/src/main/java/io/aeron/cluster/Election.java>.
- Pull request 210, the module's refusal of a second term while it holds the first's
  log publication; pull request 213, the role that moves with the election.
