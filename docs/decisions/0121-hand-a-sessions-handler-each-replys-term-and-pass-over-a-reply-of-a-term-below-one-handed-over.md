# 0121. Hand a session's handler each reply's term, and pass over a reply of a term below one handed over

- **Status:** Accepted
- **Date:** 2026-11-14
- **Invariants affected:** 1, 2, 4, 6
- **Supersedes:** none. It answers the last clause of ADR 0118's Revisit, in work
  item 229, the cluster client (ADR 0052, feature 5), before the session's handler
  reaches the C ABI and the five clients. Amends `spec/cluster.md`, whose version
  stays 0, since no field of a message changes: "Sessions" and "Replies" say what a
  client hands its caller of the replies it reads, and "Counters" gains
  `cluster.replies_stale`.

## Context

A service's reply is a session message on the egress stream, and it carries the
leadership term id of the entry it answers (`spec/cluster.md`, "Replies"). One
service answers the entries of a term: the service beside the member that led it,
while that member's counters say it leads the term still (ADR 0118). Each service
sends on a publication of its own, so a session's replies are in order from one
service and in no order between the services of two terms' leaders.

ADR 0118 names the case in which that shows. A leader that has lost its quorum
leads its term, by its own counters, until it holds an election, which is within the
leader heartbeat timeout, 10 s. Its service answers what it reads of the term's
committed entries until then. When the next term's leader is elected and its
service answers before that, a client reads a reply of the earlier term after a
reply of the later one. `ClusterSession::poll` handed its handler the payload and
the cluster's time, and not the term, so a program could not tell the two apart.

Two kinds of program read replies. One pairs each reply with a request by an id of
its own in the payload: a late reply costs it nothing. The other applies each reply
as the newest state its service holds, a balance or a price. A reply of the earlier
term, applied after one of the later, takes that program's state back, and nothing
says so. The case needs a change of leader and a former leader that still answers,
so no test of a program meets it by chance.

A reply is not a delivery (ADR 0118). It is lost when no service ran beside the
entry's leader, when the member stopped leading the term before its service read
the entry, and when the client took nothing for 500 ms. So a program that needs an
answer already waits no longer than it chooses and sends again, and its service
already treats a message it has applied before as it chooses.

The next change of the work item gives the session's poll to the C ABI and to five
generated clients, so what the handler is given is decided once for six languages.

Aeron, read on 2026-11-14 at `master`, version 1.55.0-SNAPSHOT, in
`aeron-cluster/src/main/java/io/aeron/cluster/client/`. `EgressAdapter.onFragment`
hands every session message that carries the session's id to
`EgressListener.onMessage(clusterSessionId, timestamp, buffer, offset, length,
header)`. The leadership term id is not among the arguments, and the adapter
compares no reply's term with anything. In
`aeron-cluster/src/main/java/io/aeron/cluster/service/`, `ClientSession.offer`
returns `MOCKED_OFFER` unless the service's role is leader, which is the role its
consensus module last told it. A leader that has lost its quorum keeps that role
until its module steps down, so Aeron has the same case and leaves it to the
program.

## Decision

`ClusterSession::poll` hands its handler each reply as its payload and a
`ReplyHeader`: the leadership term id of the entry the reply answers, and the
cluster's time when the leader appended that entry. They are two fields of the
reply's session message header; the third, the session id, is the session's own.

A session hands over no stale reply. A reply is stale when its leadership term id
is below that of a reply the session has handed over before. The session keeps the
highest term among the replies it has handed over, −1 before the first. A reply of
that term or a higher one is handed over and its term is kept. A reply of a lower
term is passed over: the handler is not called, and the poll does not count it in
what it returns. So the terms of the replies a program reads never fall.

A stale reply is counted in `cluster.replies_stale`, counter 158, a client's
counter (ADR 0062) keyed by the session id. `Cavalry::connect_cluster` adds it on
the client's own driver once the leader has opened the session, and the session's
close removes it. `ClusterSession::stale_replies()` reads it, so the program is told
what its session passed over, and `cavalry doctor` on the client's host reads the
same number.

The rule gives a program one sentence to act on. Once the handler has been given
a reply of a term, no reply to an entry of an earlier term follows. A leader
appends a session's message only in the term the message names, so a reply that a
service makes at a message's own entry carries the term the session had when it
offered the message. Where a service answers each message so, a message the
session offered in an earlier term, and that has no answer yet, will have none,
and a program that needs one sends the message again. A service may also answer a
message at a later entry. That reply carries the later entry's term, and is
handed over by the same rule.

Only a reply that is handed over moves the term the session keeps. A new leader
event does not, and neither does a session message for another session.

`ClusterSession::offer`, the connect and the keepalive are unchanged. The poll
gains one compare for each reply, and one relaxed add for a stale one. Nothing is
allocated.

## Alternatives considered

- **Hand over every reply with its term, and leave the order to the program.** The
  smallest change. Lost: every program that applies replies as states must then
  write the compare, in each of six languages, and the program that does not write
  it is wrong only around a change of leader, where its own tests do not look. The
  default has to be the safe one (invariant 5), and a stream that goes back is the
  one thing invariant 1 says a reader is never given.
- **Pass stale replies over and do not give the term.** Lost: the program could not
  tell which of its unanswered messages have no answer coming, and would wait its
  own timeout for each. It also could not log which term a reply came from, which
  is the first question when a client and a cluster disagree.
- **Neither, as Aeron's client.** Lost for the reasons of the two above.
- **Hand a stale reply over with a mark, a third field of the header.** No reply
  is lost. Lost: it is the first alternative again. A program that does not read
  the mark applies the reply, in the case no test meets.
- **Raise the kept term at a new leader event too,** so that a session reads
  nothing of a term once it has turned to the next. It gives a shorter rule: after
  `leadership_term_id()` rises, send again everything that is unanswered. Lost on
  two counts. It passes over replies that are in order: those of the earlier term
  that the session reads after the event and before any reply of the new term. And
  it is not enough alone. A new leader event comes on the new leader's own
  publication and a reply on its service's, two images that the session reads one
  after the other, so a reply of the new term can be read before the event. The
  replies themselves must move the term in any case.
- **Read the images in the order of their terms,** by leaving a later term's reply
  unread, with the controlled poll (ADR 0114), until the earlier term's image has
  nothing more. No reply that waits unread would then be passed over. Lost: a reply
  left unread holds the window of the new leader's service's publication, so that
  service is answered `Reply::Retry` and holds its log for a client that is
  reading. And nothing says when an earlier term's image has nothing more: the
  former leader's service may send again at any time in its 10 s.
- **Two more arguments, the term and the time, each an `i64`.** Lost: a handler
  that takes them in the other order compiles and is wrong. A struct names them.
- **Count in the handle alone,** with no counter. Lost: a reply that a service
  counted as sent and that the program never read could not be explained from the
  counters file (invariant 4).
- **A choice for the program, ordered or all.** Lost: a parameter with a default
  is a knob (`CLAUDE.md`, Configuration and defaults), and the second kind of
  program is the one that would not know to set it.

## Evidence

- `crates/cavalry-client/tests/cluster_session.rs`,
  `a_reply_of_a_term_below_one_handed_over_is_passed_over_and_counted`, against a
  driver in the test's process with the cluster played by a second client. The
  session opens in term 3 and its `cluster.replies_stale` reads 0, with the label
  `cluster.replies_stale 7`, the session's id. The played cluster sends seven
  session messages: replies of terms 3 and 4, one of term 12 for another session,
  replies of terms 3, 4 and 2, and one of term 5. The handler is given the first,
  the second, the fifth and the seventh, each with its term and the cluster's time,
  and nothing more in 200 ms. The polls return 4 in all. `stale_replies()` and the
  counter in the driver's counters file both read 2. The session's own term is
  still 3, and the session is open. After the session's close no record holds the
  counter. With the compare switched off the same test fails: the handler is given
  six replies, with the terms 3, 4, 3, 4, 2 and 5.
- `crates/cavalry-client/tests/cluster_session_allocations.rs`: 5,000 messages
  offered and their replies read, two new leader events followed, a keepalive
  sent, and 10 stale replies passed over and counted, one after every 500 rounds,
  under the counting allocator. No allocation and no reallocation.
- `crates/cavalry-raft/tests/cluster_service.rs`, three members over three drivers
  with a service beside each and a client's session on a fourth. The reply to the
  first message is read with the first leader's term and the entry's time. The
  leader is lost; the replies to the two messages that follow are read with the
  next leader's term. `stale_replies()` reads 0 at the end. In the second test each
  of three sessions reads `welcome` with the term the session opened in.
- Aeron's source named in Context.

## Consequences

Easier: a program that applies replies as they come is right across a change of
leader with no code of its own. A program knows which of its messages have no
answer coming, from the term of what it reads. A reply names its term in every
language, so a log line of a client can be set beside a member's counters.

Harder: a reply is lost in a fourth way, and this one the client's side counts. A
program that pairs replies by an id of its own loses the late replies it could
have used, and sends those messages again. A session reads its images one after
another, so when replies of two terms wait unread at one time, on two services'
publications, the later term's can be read first, and the earlier term's are then
stale though they were sent first. That needs a client that has not polled across
an election. A session costs its client's driver one counter record, of the 256 a
driver holds for clients, and its connect one more command. When the driver
refuses the counter, the connect fails with the driver's refusal, after one
session close offered to the leader. The handler's signature changed: every
program that polls a session is edited once.

Two services beside one leader in one term, one after the other, send replies of
one term on two publications. The term cannot order those. The second answers
nothing that the member held when it opened its log (ADR 0118), and the first has
stopped, so the two sets do not overlap, and the first's replies were sent before
the second started.

Revisit: when a program shows that it needs its late replies and that sending
again is not enough, for a second handler that is given them; when a snapshot is
built and a service starts from one, for what a session reads across it; when a
session connects again by itself after its cluster lost a quorum (ADR 0117), for
whether the term it keeps is kept across the connect.

## References

- `CLAUDE.md`: invariants 1, 2, 4, 5 and 6; Data plane; Configuration and defaults
- ADR 0052, ADR 0062, ADR 0114, ADR 0117, ADR 0118
- `spec/cluster.md`, "Sessions", "Replies" and "Counters";
  `crates/cavalry-client/README.md`; `docs/glossary.md`, stale reply and reply
- Work item 229 of the Brigade Board
- Aeron, read 2026-11-14 at `master` (1.55.0-SNAPSHOT):
  `aeron-cluster/src/main/java/io/aeron/cluster/client/EgressAdapter.java` and
  `EgressListener.java`, and
  `aeron-cluster/src/main/java/io/aeron/cluster/service/ClientSession.java`
  (<https://github.com/aeron-io/aeron/tree/master/aeron-cluster/src/main/java/io/aeron/cluster>)
