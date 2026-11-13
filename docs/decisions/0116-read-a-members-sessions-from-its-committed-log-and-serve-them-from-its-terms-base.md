# 0116. Read a member's sessions from its committed log, and serve them from its term's base

- **Status:** Accepted
- **Date:** 2026-11-13
- **Invariants affected:** 1, 2, 3, 4, 6
- **Supersedes:** none. Amends `spec/cluster.md`, whose version stays 0, since no
  field of a message changes: states 6 and 10 of "Roles and the election" and "A
  member's start", which had the consensus module's own state come from a replay at
  each election; "Sessions", which says how a member reads its log for its sessions,
  when a leader serves them, and that a message of the ingress stream is one frame;
  "Members and streams" and "Values", for the log replay stream, 103, on which the
  module reads, and for the length of the ingress endpoints; and "Counters". Amends ADR 0115 in
  one clause, where a member reads the entries of the terms before its own: ADR 0115
  has it read them "in leader replay", before its election closes, as the page then
  said, and this ADR has it read them in the read that runs all the time, before it
  serves sessions. What ADR 0115 needs of that read is kept: a leader accepts no
  connect before it has read every session open and close event below its term's
  base. It is the third change of work item 229, the cluster client (ADR 0052,
  feature 5).

## Context

ADR 0115 put a member's sessions in `Sessions`, a table that the committed session
open and close events of the member's own log open and close, on every member, and
left to this change how the consensus module reads that log and when a leader may
use the table. `spec/cluster.md` had an answer from work item 226 (ADR 0058; ADR
0066): in state 6 a leader "replays its own log, its recordings in position order,
from its log position to its append position, into the consensus module's own
state", and in state 10 a follower does the same to the lesser of its append
position and the commit position it was told. Nothing of it was built: `Consensus`
steps through state 6 at once.

Two things make the page's replay the wrong read for `Sessions`.

A leader in state 6 has not closed its election. Its log above the commit position
is its own claim: if its election fails, the next leader may truncate that span
("The term's first entry"). A session a replay to the append position opened from
such an entry would be in the member's table and in no log, and nothing takes a
session out of the table but a session close event.

And a replay at each election reads the whole span since the last one, however long
the term was, while the election waits. This version takes no snapshot, so on a
member's start that span is the whole log.

What exists to read a member's log no further than the commit position: the read a
service makes (ADR 0066, ADR 0103). `Cavalry::committed_log(position)` opens a
replay that follows the member's recordings, across them, bounded by the member's
`cluster.commit_position`, on the `cavalry:ipc` service log stream, 104, where a
reader reads past every replay but its own.

Aeron, read on 2026-11-13 at `master`, version 1.55.0-SNAPSHOT, in
`ConsensusModuleAgent.java`: a follower's module reads its log as it follows, no
further than the commit position it was notified of, `logAdapter.poll(min(
notifiedCommitPosition, limit))`, and a leader replays to its append position in
`LEADER_REPLAY`. Aeron's services read the same replay under the module's control;
Cavalry's read for themselves (ADR 0066).

## Decision

A member's consensus module reads its own committed log as a service does, in every
role and every state of its election, from position 0 when it starts. It reads on a
stream of its own, the log replay stream, 103 on `cavalry:ipc`, with
`Cavalry::committed_log_on`, which is `Cavalry::committed_log` on a stream the caller
names. Every subscription on a stream is given every publication's image, so on one
stream a reader that opens its log again would cost every other reader on that
driver an image to open; on its own stream the module's replay and a service's never
reach each other. In Aeron, 103 is the cluster's replay stream. Each entry it reads goes to `Sessions::on_entry`. When
the handle ends, at a recording's stop, the module opens the log again where it
stopped, as a service does. States 6 and 10 replay nothing into the module's state:
the read that runs all the time is that state's one source, and nothing above the
commit position is ever in it.

A leader serves sessions from when its election has closed and its read has reached
its term base log position. To serve is to accept a connect, append a session's
message and send the new leader event. Before that the member's table may lack a
session, or a session id, that the terms before it gave. The span of its log between
the last commit position it knew and its term's base is committed once a quorum has
recorded its term, when its election closes (`spec/cluster.md`, "The commit
position"), and its read of that span, through its driver's replayer, ends some duty
cycles later. From then on it reads no entry of a term before its own: every one is
below its term's base. Until it serves, a connect
stays unread on its ingress image, as ADR 0115 says of a member whose election runs,
and what a client sends for a session is ignored: a client sends with the new term
only once it has the new leader event.

The module reads the ingress stream on its member's `ingress` endpoint with the
controlled poll of ADR 0114. A message `Sessions` leaves unread stays at its image's
position, and until that message is taken the module aborts every fragment of
another image, so that it "reads nothing more from its ingress images", as the page
says. An image that goes while its message is held holds nothing any more.

A message of the ingress stream is one frame, as an entry of the log is (ADR 0066):
a session message carries at most 1,344 bytes of payload. The module reassembles
nothing. It counts in `cluster.messages_rejected` a fragment, and a session message
whose payload is longer than 1,344 bytes and that arrives whole in one frame, which
a sender with a longer MTU delivers. Every member checks that length before it acts
on the message in any other way. It takes such a message from its ingress image, so
that it holds back no other client's message, and appends nothing of it: the
message is never its session's activity. The log's append refuses an entry longer
than one frame as a fault, and never as back pressure, which waiting would not end.
An entry of its own log that is a fragment, that does not parse, or that the table
cannot hold is counted there too.

An egress publication is an exclusive publication on the client's response channel,
one for each of the 20 slots `Sessions` names. The driver's refusal of the channel
is the refusal ADR 0115 counts as a connect dropped.

The members' ingress endpoints, as a redirect lists them, must fit a session
event's detail in one frame: 1,320 bytes. A member does not start on a members
string that lists more, `RaftError::IngressEndpoints`, and says what to do.

The module allocates the eight counters of ADR 0115 with its others and stores them
each cycle.

## Alternatives considered

- **The page's replays, in states 6 and 10, to the append position.** Lost: a
  session read from an entry above the commit position is in the table before it is
  in the log for good, and a leader whose election then fails keeps it. Aeron's
  module does replay so, and Aeron's election does not go back from `LEADER_REPLAY`
  to a follower with that state kept; Cavalry's member begins again at init with
  its table as it is.
- **A replay at each election, bounded by the commit position.** Lost: the election
  waits for a read as long as the last term, and at a member's start as long as the
  log. A read that runs all the time is one batch behind.
- **Read the entries from the live image as the member records them.** Lost: a
  member's log reaches it four ways, the live image, a replication, a catch-up and
  the recordings it kept at its start, and only the recordings hold all four. An
  entry read from an image is also above the commit position.
- **The module's replay on the service log stream, 104, beside the services'.** The
  first form of this change, since `Cavalry::committed_log` reads that stream and a
  second stream is a second constant and a second call. Lost on a measurement. A
  follower that joins the log late holds it in two recordings, and its module opens
  its log again where the first one stops. On one stream that new replay's image is
  announced to the service's subscription on the same driver, whose client then
  opens and maps it. `tests/module_allocations.rs`, which counts the allocations of
  three members' duty cycles and their services' polls, counted one in 9 of 25 runs
  on a macOS laptop, 120 bytes, the mapping of that image in the service client's
  conductor; with the module on its own stream it counted none in 25 runs. On one
  stream each service would also read past the module's replay at every poll, 64
  fragments at most (ADR 0066).
- **Serve sessions as soon as the election closes.** Lost: the leader's read may
  not have reached its term's base yet, and it would give a session id that a
  session open event of the term before already holds; two sessions with one id are
  then in the log for good, and a leader that reads the earlier entry late takes it
  for its own connect (ADR 0115, the alternative that lost there).
- **Hold the election in state 6 until the module's read has reached the term's
  base,** which is the page's order and ADR 0115's words. Lost: the read is a round
  through the driver's replayer, and an election that waits for it is an election
  whose length depends on the log's length at a member's start. The election's own
  work needs nothing of the sessions. What needs them is serving, so serving waits.
- **Reassemble a fragmented session message.** Lost: a buffer for the longest
  message of each of ten ingress images, to allocate and to bound, for a log whose
  entries are one frame. The client refuses a longer message before it sends it.
- **Answer on a longer session event, fragmented, when the ingress endpoints are
  long.** Lost for the same reason on the client's side: a client reads one frame.
  Eight members with host names of 150 bytes each fit; a string that does not fit
  is refused at the member's start, where an operator reads it.

## Evidence

- `crates/cavalry-raft/tests/cluster_sessions.rs`, over three drivers on the
  loopback, with a client made by hand on a fourth driver:
  `a_client_is_redirected_opens_a_session_and_is_closed_when_it_falls_silent`, a
  follower's redirect with every ingress endpoint, the leader's ok event with
  session id 0, `cluster.sessions` at 1 on all three members, the session's message
  read by a service beside the leader with the leader's term and the cluster's
  time, and the closed event with `TIMEOUT` no sooner than 10 s after it; and
  `a_session_survives_its_leader_and_is_told_of_the_new_one`, the leader and its
  driver lost, the new leader event from the member that replaces it, with its
  term, a message with that term committed and read by a service, and the client's
  close closing the session on both members.
- `crates/cavalry-raft/tests/module_allocations.rs`: three members, their services
  and now each member's own read of its log, twenty thousand cycles with an entry
  offered in each, no allocation; 25 of 25 runs on a macOS laptop with the module on
  the log replay stream, and 16 of 25 with it on the service log stream.
- `crates/cavalry-raft/tests/sessions.rs` and the module's own tests, added on the
  review of pull request 278: a session message of 1,345 bytes of payload is
  rejected on a leader and on two members that do not lead, with the log
  back-pressured or not, and does not keep its session from its timeout; one of
  1,344 bytes is appended as one entry of 1,376 bytes; a rejected message of one
  image holds back no message of another image in the same poll; an entry of 1,377
  bytes is a fault of the append, `too-long`. A members string whose ingress
  endpoints are listed in 1,320 bytes passes the check and one of 1,321 bytes is
  refused, and a redirect with a detail of 1,320 bytes encodes to one frame of
  1,376 bytes.
- The pull request's body has the runs: how many times each test ran, on the laptop
  and on both runners, and with a follower forced to join late.
- Aeron's source named in Context.

## Consequences

Easier: one read gives a member its sessions in every role, so nothing is rebuilt
at an election and nothing is undone after a truncation; the path is the service's,
with its checks and its tests; a client now has a member to connect to.

Harder: each member holds one replay of its own log for as long as it runs, of the
driver's 64, and one more stream id is the page's; the ok event, a
session's opening and a new leader's serving each wait for the member's own read of
an entry it has committed, a round through its driver's replayer; a leader does not
serve sessions between the close of its election and the cycle its read reaches its
term's base; a member that starts reads its whole log before its table is whole, and
serves no session until then; and a client that was told its session is closed can
be sent a new leader event for it. The closed event of a timeout follows the close
event's append on the leader, not its commit (ADR 0115), so a leader lost between
the two leaves the session open in the log, and the next leader, which has the
session from that log, takes it up and closes it again by the timeout, 10 s later.
Aeron has the same window. The page now says so.

Revisit: when a snapshot is taken, for where
the module's read begins (ADR 0052, feature 6); when a client needs a session
message longer than one frame, for reassembly on the ingress and the egress
streams; when a cluster's host names do not fit the ingress endpoints' bound, for a
redirect that names the leader alone.

## References

- `CLAUDE.md`: invariants 1, 2, 3, 4 and 6; Data plane; Protocol
- ADR 0052, ADR 0058, ADR 0066, ADR 0103, ADR 0114, ADR 0115
- `spec/cluster.md`, "Members and streams", "Roles and the election", states 6 and
  10, "A member's start", "Sessions", "The service" and "Counters";
  `crates/cavalry-raft/README.md`
- Work item 229 of the Brigade Board
- Aeron, read 2026-11-13 at `master` (1.55.0-SNAPSHOT):
  `aeron-cluster/src/main/java/io/aeron/cluster/ConsensusModuleAgent.java`
  (<https://github.com/aeron-io/aeron/blob/master/aeron-cluster/src/main/java/io/aeron/cluster/ConsensusModuleAgent.java>)
