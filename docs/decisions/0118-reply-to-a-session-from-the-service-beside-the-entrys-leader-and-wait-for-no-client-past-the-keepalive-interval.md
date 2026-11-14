# 0118. Reply to a session from the service beside the entry's leader, and wait for no client past the keepalive interval

- **Status:** Accepted
- **Date:** 2026-11-13
- **Invariants affected:** 1, 2, 3, 4, 6
- **Supersedes:** none. It is the service's part of work item 229, the cluster client
  (ADR 0052, feature 5). Amends `spec/cluster.md`, whose version stays 0, since no
  field of a message changes: "The service" gains "Replies", which says which
  service answers an entry, what it does not answer, and what becomes of a reply a
  client does not take; a member's rules of order for the service become four;
  "Counters" gains `cluster.replies_dropped`; "Values" gives the keepalive interval
  its second use.

## Context

`spec/cluster.md`, "The service", says how a service reads its member's log, and
ends with one sentence on replies: "The service of the leader replies to a session
with a session message on the egress stream; the service of a follower reaches the
same state and sends nothing." `cavalry-client` has the read, `CommittedLog` (ADR
0066, ADR 0103). It has no reply. A program that wants one must parse the log's
entries, follow the session open and close events for each session's response
channel, hold a publication to each, and decide by itself whether it is "the service
of the leader". ADR 0052 asks for the opposite: a service in any language through
the C ABI, and a client whose message comes back answered.

Three things that sentence leaves open.

Which service is the leader's. A service is a client of its member's driver and
reads the log from the member's recordings, some time after the member appended it.
The member's role when the service reads an entry is not its role when the entry was
appended. The member publishes its role in three counters, `cluster.role`,
`cluster.election_state` and `cluster.leadership_term_id`, which it stores one after
another in a duty cycle, so a reader can load a role of one cycle beside a term of
the cycle before.

What a service does with what it has read before. A service that starts, or starts
again, reads its member's log from position 0, since the log is where its state
comes from. If it answered as it read, every client would be sent every reply
again at each start of a service.

What a service does when a client does not take a reply. The reply goes on a
publication to the client's response channel. That publication is not connected for
the first milliseconds of a session, and it is back-pressured when the client does
not read. A service that waits holds every other session's replies; a service that
does not wait loses the first reply of every session.

Aeron, read on 2026-11-13 at `master`, version 1.55.0-SNAPSHOT, in
`aeron-cluster/src/main/java/io/aeron/cluster/service/`. `ClientSession.offer`
returns `MOCKED_OFFER` when the service's role is not leader, and otherwise what
the session's publication returns, `NOT_CONNECTED` and `BACK_PRESSURED` included
(`ClusteredServiceAgent.offer`). The role is not read from a counter: the consensus
module tells the service its role for each log it joins (`joinActiveLog`), and a
service that recovers replays its log as a follower. A session's publication is
added at its session open when the service is the leader's, and for every session
when the service joins a log as leader. What a service does with a back-pressured
offer is the program's: the usual loop offers again until it is taken, and stops
the service for as long as one client does not read.

## Decision

`Cavalry::cluster_service()` returns a `ClusterService`, and
`ClusterService::poll(client, handler, fragment_limit)` reads the member's
committed log from position 0, across the member's recordings, with a `CommittedLog`
of its own. Each entry is handed to the handler as a `Committed`: an `Entry`, which
is a session's message, a session's open, a session's close, a term's first entry
or `Other`, with its position, its leadership term id and the cluster's time. The
handler returns an `Action` (ADR 0114): `Abort` leaves the entry for the next poll.
An entry that is not of the page stops the service at its position with an error,
since a service that passed it would no longer hold the state the log gives.

The handler answers a session with `Replies::reply(session_id, payload)`: a session
message on the egress stream with the entry's term and time, on an exclusive
publication of the service's own to the session's response channel. The service
follows the session open and close events for the sessions and their channels, 10
at most, as a member does (ADR 0115). A reply is one frame, 1,344 bytes of payload
at most.

**One service answers an entry: the service beside the member that led the entry's
term.** The term's new leadership term event, its first entry, names that member,
and the service compares it with the member id its member's counters are keyed by.
The rule is read from the log, so it is the same wherever and whenever the entry is
read, and no two services answer one entry. The service beside every other member is
answered `Reply::NotAnswering` and sends nothing.

**A service does not answer history.** When it first opens its member's log it loads
the member's `cluster.leadership_term_id` and then its `cluster.append_position`. An
entry of a term below that term, or of that term and below that append position, is
one the member held before the service read its log. The service applies it and
answers `Reply::NotAnswering`. A member adds both counters before
`cluster.log_recording_id` and stores both before it stores a recording's id, in
every duty cycle, which is the fourth rule of order of "The service": the consensus
module already does so, and the page now says it.

**A service waits for one client no longer than the keepalive interval, 500 ms.** A
reply the session's publication does not take, not connected or back-pressured, is
`Reply::Retry`: the handler returns `Action::Abort` and is handed the entry again.
Once a session's publication has taken nothing for 500 ms since it first refused a
reply, the answer is `Reply::Dropped`, counted in `cluster.replies_dropped`, and
each later reply to that session is offered once and dropped at once until the
publication takes one. The service reads its clock for a reply that was refused,
and for no other: a reply that is sent reads none.

**A reply is offered to its session once, however often its entry is handed over.**
A handler may answer an entry with several replies, to one session or to many, and
an entry handed over again makes them all again. For each session the service
counts the replies that the handlings of the entry have settled, sent or dropped,
and answers them as they were without offering them again. Once a reply to a
session is refused, the replies the same handling makes that session after it are
`Retry` too and are not offered, so the settled replies are always the first ones
and a session's replies keep their order. A reply longer than one frame carries is
not one of them: it is `Reply::TooLong`, refused before it is counted in every
handling alike, so it moves no other reply's place in the count. It rests on what
the replay check already asks of a handler: the same replies in the same order each
time. 500 ms is how often a client reads its session at the least
(ADR 0117), so a client that keeps its own rule loses no reply to this one.

**A session is open at its own open event, and a client's connect takes no session
message.** A handler may reply to a session at the session's open event, a welcome,
and at any entry from there to the session's close event, that event included. The
leader's module sends the ok event and the leader's service sends that reply when
each reads the same committed entry, so the reply can reach the client's response
channel before the ok event does, or beside it, while the client is still in
`Cavalry::connect_cluster`. The connect reads the response channel with a
controlled poll (ADR 0114). It ends an image's read at its answer, and it leaves
where it is a session message, everything behind one on the same image, and
everything that follows the answer. The session's first poll then hands over each
session message that carries its session id, as it does every other. A service
replies on a publication of its own and never on the member's, so a reply the
connect leaves holds no session event behind it.

The service holds its sessions' publications from the first entry it answers and
gives them back at the first it does not. Adding one is a command to the driver: at
a session's open event, at the first entry of a term the member leads, and where
history ends. `poll` takes the client for them. No command is sent, and nothing is
allocated, for a session message or its reply.

## Alternatives considered

- **Answer when the member's counters say it leads.** The first form of this
  change. Lost on two counts. The three counters are stored one after another, so
  a service can load `role` leader beside the term of the cycle before, and answer
  the entries of a term its member did not lead while that term's leader's service
  answers them too. And the role now is not the role at the entry: a member that
  restarts and leads again would answer the whole of its log, which it led in terms
  long closed. The log names each term's leader, once and for every reader.
- **History as what lies below the member's commit position when the service
  opens.** The second form. Lost: a member that has just started knows no commit
  position until its election closes, and its counter reads 0 while its log holds
  everything the cluster ever committed. A service that opened then would take all
  of it for new. The append position is the member's own and is right from its
  first duty cycle.
- **History as what lies below the append position alone.** Lost: a follower cuts
  its log on a new leadership term, so a later term's entries can lie below a
  position the member held when the service opened. The term beside the position
  says which they are.
- **Aeron's rule: the consensus module tells the service its role for each log it
  joins.** Lost: ADR 0066 has a service read its member's recordings with no message
  from the module, so that a service may start before its member, with it or after
  it, in any language. A message from the module to the service is a stream, a
  protocol and an order of starting for what the log already holds.
- **Return the publication's refusal and leave the wait to the program,** as Aeron
  does. Lost: the loop every program then writes offers until it is taken, and one
  client that stops reading stops the service of a whole cluster for every other
  client, with nothing counted (invariants 3 and 4). A session's close for the
  session timeout does not end it, since the service is held before the entry that
  closes the session.
- **Drop a reply the publication does not take, at once.** Lost: a session's
  publication is not connected when its first message is read, so the first reply
  of every session would be lost, and back pressure would be absorbed as loss with
  no return to the caller (invariant 3).
- **Wait for a session's publication to connect at its open event.** Lost: it
  holds the log for every session at each connect, the leader's service alone, for
  a client that may never be there; and it does nothing for a client that stops
  reading later.
- **A longer wait, or the program's own.** Lost for the default: 500 ms is the one
  number a client already keeps. A parameter with a default is a knob (`CLAUDE.md`,
  Configuration and defaults).
- **Close a session whose client takes nothing.** Aeron's service can ask its
  module to close a session. Lost for now: a service here sends its member nothing
  (ADR 0066), and the leader closes a session that sends nothing for the session
  timeout. A client that sends and never reads keeps its session and loses its
  replies, counted.
- **A position to start from,** as `Cavalry::committed_log(position)` takes. Lost:
  a service holds its sessions from the log's own entries, so a service that began
  above position 0 would know no session opened below it. It comes with the
  snapshot, which carries them.
- **Reassemble a longer reply.** Lost with ADR 0116's reason: a message of the
  session protocol is one frame in this version.
- **Leave it to the handler to reply before it changes its state.** The first form
  of this change, which the review of pull request 280 refused. Lost: a reply to
  one session is itself what a handler has done when the next reply, to another
  session, is refused. The entry was handed over again and the first session was
  sent its reply at each handling: 420 times in the test that now guards it.
- **Count only the replies a publication took.** Lost: a reply that was dropped
  between two that were sent would shift the count, and the second would be sent
  again. Every settled reply counts, and the first 64 keep whether they were
  dropped, so that a handling after is answered as the first was.
- **Refuse a too-long reply where the publication refuses it.** The fix for the
  first review had this form, and the review of c1e68230 refused it. Lost: the
  reply was counted in a handling that answered it from the count, or held it
  behind a refused reply, and not in a handling that offered it, so the count
  shifted, as in the alternative above. The reply after it was sent again, and the
  handler was told `Sent` for a reply that was never sent. The length is checked
  first now, against the 1,344 bytes the page gives a reply, as a member checks a
  payload before it offers one.
- **Hold in the session what its connect reads.** The connect would copy each
  session message it meets into a buffer of the session's, and the first poll would
  hand them over. Lost: a second place for a message to be, with a bound of its own
  and a rule for when it is full, where the image already holds the message and its
  publication's window already bounds what a service can send ahead (invariant 3).
- **No session at its own open event,** `Reply::NoSession` there. Lost: a welcome is
  what a service most often sends unasked, and Aeron's service may offer to a
  session in `onSessionOpen`. It would also not have closed the gap: a reply to
  every open session at a later entry can reach a client in its connect as well.
- **Let the connect take what it reads,** as its first form did, and say that a
  reply before the connect returns is lost. Lost: the service is told `Sent`, no
  counter moves, and the client has no wait that tells it a reply is missing
  (invariants 1 and 4). The fourth review of this change found it.

## Evidence

- `crates/cavalry-client/tests/cluster_service.rs`, against a driver in the test's
  process with the member played by a client of that driver: a recording of a
  stream it appends the log's entries to, its four counters and its file of
  recordings. A service beside the member that leads term 0 hands over the term's
  first entry, a session's open and its message with their positions, term and
  times, reads nothing above the commit position, and its reply reaches the
  session's client with the entry's term and time; in term 1, which another member
  leads, the same service is answered `NotAnswering`, sends nothing and gives its
  publications back. A service that starts beside a member whose log holds an old
  term and part of its present term applies all five entries, answers none, and
  answers the entry the member appends next. A reply to a session with no client is
  tried again for no less than 500 ms and no more than 5 s, then dropped, and
  `cluster.replies_dropped` reads 1; the next reply to it is dropped with no wait,
  and the counter reads 2; when the client subscribes, a reply reaches it. A reply
  to a session that never opened or has closed is `NoSession`, one of 1,345 bytes is
  `TooLong` and one of 1,344 is sent, and neither refusal is counted as a drop; a
  reply to a session at its own open event and one at its own close event are each
  `Sent`, and the client reads them before and after the reply to its message; a
  timer event is handed over as `Other`; an entry that is not of the page stops the
  service at its position, three polls running. A service holds 10 sessions: an
  eleventh open event opens nothing, and a close gives its place to the next. An
  entry answered with four replies, to a session whose client reads, twice to one
  with no client, and to the first again, is handed over again for the 500 ms the
  second session is waited for: the first session's client reads its two replies
  once each and in order, the reply behind a refused one is not offered, and the
  second session's two are dropped, with `cluster.replies_dropped` at 2. With the
  count switched off the same test reads the first session's replies again at
  every handling and fails. A reply of 1,345 bytes is `TooLong` at every handling
  of its entry, both before a reply to a session whose client reads and behind a
  refused reply to a session whose client subscribes after the first handling; each
  client reads each of its replies once, in order. With the length checked where
  the publication refuses it, as the first fix had it, the first case reads
  `[Sent, Sent, Dropped]` at its last handling, not `[TooLong, Sent, Dropped]`. In a
  probe of the second case alone, outside the tree, the client reads `one`, `two`
  and `two` again over 472 handlings.
- `crates/cavalry-client/tests/cluster_service_allocations.rs`: 4,800 session
  messages read, handed over and answered, and every answer read by the session's
  client, under the counting allocator, with the driver's threads in the process.
  No allocation and no reallocation. The test's first form ran 200 rounds over 220
  s and counted two allocations and one reallocation. A probe of the allocator, in
  a copy of the test that is not in the tree, placed all three in the test harness,
  `test::run_tests::get_timed_out_tests` and the line it prints when a test has run
  for 60 s, and none in Cavalry: the same 5,040 entries in 12 rounds counted none.
  The test now runs for less than 60 s, and says why.
- `crates/cavalry-raft/tests/cluster_service.rs`: three members over three drivers
  on the loopback with a `ClusterService` beside each, in one thread, and a client's
  `ClusterSession` on a fourth driver, in the test's. Nothing of the protocol is
  written by hand. Every service answers a session message with its member's id
  and the message, so a reply says which service sent it. Each member has added its
  term and its append position before its recording's id. The client's first message
  is taken by all three services at one position, with one term and one time; the
  leader's service is answered `Sent` and the two others `NotAnswering`; the client
  reads one reply, the leader's, with the entry's time, and no second in 300 ms. The
  leader is lost, with its service and its driver. The session follows the member
  that leads next, and its next message is answered by that member's service alone,
  once. That service stops and another starts beside the same member: it takes both
  messages again, from position 0, and is answered `NotAnswering` for each, the first
  being of an older term and the second below the member's append position when the
  service opened; the client reads nothing. The third message it answers, once.
  `cluster.replies_dropped` reads 0 beside both members. The pull request's body
  has the runs: how many times the test ran in a row, on a laptop and on both of
  CI's runners.
  A second test has every service reply `welcome` at a session's open event: three
  sessions, each on a response channel of its own, are each handed `welcome` once,
  with the cluster's time, then the one answer to a message of their own, and
  `cluster.replies_dropped` reads 0 beside every member.
- `crates/cavalry-client/tests/cluster_session.rs`, where one thread plays the three
  members: the played service sends a session message on a publication of its own,
  and the played leader sends its ok event 50 ms after that message was taken. The
  connect returns the session, and the session's polls hand the message over once,
  with nothing more in 200 ms. With the connect's first form, which read the
  response channel with `poll`, the same test fails: the session is handed nothing
  in 5 s. The end-to-end test above does not tell the two forms apart on a laptop,
  six runs of six passed with the first form: there the service's publication
  connects after the ok event is sent. So the played test is the one that holds
  the rule.
- Aeron's source named in Context.

## Consequences

Easier: a program writes a service as one handler, the same on every member, and
does not ask whether it leads; a service that starts again is safe by reading its
log; one client that stops reading costs the others 500 ms, once, and a counter
says so; a reply is answered by one service, so a client need not expect the same
reply from two members.

Harder: a reply is not a delivery. It is lost when no service ran beside the
entry's leader, when the leader was lost before its service read the entry, and
when the client took nothing for 500 ms, and of the three the client sees only its
own wait. A reply that reaches a client before its connect returns is not a fourth:
the connect leaves it for the session. A program that must have an answer sends again, and its service must
then treat a message it has applied before as it chooses. A handler that returns
`Abort` is handed the entry again with everything it did for it, so it replies
before it changes its state, and it must make the same replies in the same order
each time, which the service counts on. A service's first reply to a session can hold the
entry for a few milliseconds, until the publication connects. The leader's service
holds a log buffer for each open session, 10 at most. A session message a connect
leaves unread holds its publication's window as any unread fragment does, for as
long as the connect lasts, 20 s at most.

Revisit: when a snapshot is built, for a service that starts from one and for the
sessions a snapshot carries; when a service must close a session, for a message from
a service to its member; when a client must know that its message was answered or
lost, for an event of the leader's that says a reply was dropped; when a reply
longer than a frame is needed, for reassembly; when `doctor` is to say that a
leader's service is not running, for a counter of the service's position.

## References

- `CLAUDE.md`: invariants 1, 2, 3, 4 and 6; Data plane; Configuration and defaults
- ADR 0052, ADR 0066, ADR 0103, ADR 0114, ADR 0115, ADR 0117
- `spec/cluster.md`, "The service" and its "Replies", "Sessions", "Counters" and
  "Values"; `crates/cavalry-client/README.md`; `docs/glossary.md`, service and reply
- Work item 229 of the Brigade Board
- Aeron, read 2026-11-13 at `master` (1.55.0-SNAPSHOT):
  `aeron-cluster/src/main/java/io/aeron/cluster/service/ClusteredServiceAgent.java`,
  `ClientSession.java` and `ContainerClientSession.java`
  (<https://github.com/aeron-io/aeron/tree/master/aeron-cluster/src/main/java/io/aeron/cluster/service>)
