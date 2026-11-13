# 0115. Keep a cluster's sessions on every member, opened and closed by the log

- **Status:** Accepted
- **Date:** 2026-11-13
- **Invariants affected:** 1, 2, 3, 4, 6
- **Supersedes:** none. Amends `spec/cluster.md`, whose version stays 0, since no
  field of a message changes: "Sessions" says who opens and closes a session and
  what a leader does with a connect it cannot finish, and "Counters" names three
  counters more and widens two. It is the second change of work item 229, the
  cluster client (ADR 0052, feature 5).

## Context

`spec/cluster.md`, "Sessions", says what a member does with a client: a follower
redirects a session connect, the leader refuses it or opens a session, appends the
session's messages to the log, closes a session that is silent for the session
timeout, and tells every open session of a new term. It says "The session ids and
open sessions are the log's: a new leader recovers them from the session open and
close events it has replayed", and states 6 and 10 of "Roles and the election" have
each member replay its own log "into the consensus module's own state: the open
sessions and their ids". The page was written for work item 226 (ADR 0058). None of
it is built: `cavalry-raft` has an election and a replicated log (ADR 0104), its
consensus module reads no ingress stream, and `Consensus` steps through state 6 at
once.

Building it needs one thing the page leaves open: where a member's table of
sessions comes from. The leader could keep the table from the connects it accepts
and the closes it decides. A follower then has none, and the page's own sentence
fails: a new leader must rebuild the table from its log at the election. Two tables
kept two ways must also agree, and a leader's table holds sessions whose session
open events a later leader may truncate.

The page also leaves five smaller things unsaid, each of which the code must
decide: what a member does with a connect while its own election as leader is still
running; what happens to a session open event the back-pressured log does not take;
what a leader that stops leading does with the connects it had accepted; what a
leader does with a client's messages for a session it has begun to close; and how
long a response channel a session holds.

Aeron, read on 2026-11-13 at `master`, version 1.55.0-SNAPSHOT, in
`ConsensusModuleAgent.java`. A follower's consensus module reads its own log no
further than the commit position, `logAdapter.poll(min(notifiedCommitPosition,
limit))`, and that read opens and closes its sessions, `onReplaySessionOpen` and
`onReplaySessionClose`. So every member holds the sessions, and a new leader has
them. The leader holds a connect it has accepted as a pending session until the
session open event is appended. The review of pull request 277 read
`SessionManager.checkSessions`, in `SessionManager.java` at `master`, on the same
day: for a timed-out session the leader sends the `CLOSED` event only after
`appendSessionClose` has succeeded.

## Decision

A session is opened by its committed session open event and closed by its
committed session close event, on every member, the leader among them. `Sessions`,
in `cavalry-raft`, is that table and the leader's part beside it. It is plain
state, as `Consensus` is: its caller hands it the messages of the ingress stream
(`on_ingress`), the entries of the member's own log at or below the commit position
(`on_entry`), and a duty cycle (`do_work`); what it sends and appends goes through
a trait, `SessionIo`. The consensus module will drive it over a driver in the next
change of the work item, and how the module reads its own log is decided there.

The table holds 10 sessions, and a member holds 10 pending answers, the bounds
"Values" names: 20 egress publications at most, each in a slot of its own.

What the leader adds to the table:

- A connect it accepts takes a free slot as a **connecting** session, with an egress
  publication on the response channel and the next session id, and the leader
  appends its session open event. When the leader reads that entry back, committed,
  the session is **open** and the leader sends the ok event. A follower that reads
  the same entry opens the same session, with no publication.
- The next session id is one more than the highest the member has given or has
  read in a session open event, so a new leader gives no id twice.
- A session open event the back-pressured log does not take waits, as the page
  says of every entry of the leader's own. One the log has not taken at the
  session connect timeout, 5 s, is given up with its connect.
- A leader that stops leading gives up its connecting sessions and removes every
  egress publication. The open sessions stay: they are the log's.
- A member that begins to lead a term adds an egress publication for every open
  session, owes each the term's new leader event, and counts each session's timeout
  from that cycle.
- A session open event a leader reads that it did not write, an entry of the term
  before its own that is committed in its term, opens a session the leader takes up
  the same way, with no ok event: the leader that accepted the connect owed that.
- A session the leader closes for the timeout is told so only once the log has
  taken its session close event, as in Aeron. The closed event is then owed, as
  the ok event is, until the session's publication takes it. The committed close
  frees the session and ends the debt, and so does the leader's stopping leading.
  A client that is never told finds its leader silent for the leader heartbeat
  timeout.
- When the leader has decided a session's close, by the client's session close or
  by the timeout, it ignores what the client sends for that session. It sends the
  session nothing it still owed, an ok event or a new leader event, but the closed
  event of a timeout. The session stays open on every member until the session
  close event is committed.

A member that leads a term whose election has not closed leaves a connect unread
on its ingress image (`Ingress::Unread`, the abort of ADR 0114's controlled poll),
as it leaves a session message the back-pressured log did not take.

A response channel is `cavalry:udp?endpoint=<host:port>` and at most 280 bytes: the
prefix, a host name of 253 bytes, `:` and five digits. A session holds it in the
table, since a new leader needs it.

Eight counters, each a client counter keyed by the member id. Five are on the page
already. Three are new: `cluster.sessions_accepted`, `cluster.sessions_opened` and
`cluster.sessions_closed`. The session's life is a state machine in `counters.toml`,
`cluster_session`, closed, connecting and open, with a counter for each transition.
`cluster.sessions`, `cluster.sessions_opened` and `cluster.sessions_closed` count
committed entries, so they agree between members that have read the log to the same
position.

## Alternatives considered

- **The leader's table from the connects it accepts, rebuilt by a new leader from
  the log.** Lost: two ways to reach one table, which must agree at every election;
  a leader's table would hold a session whose session open event the next leader
  truncates; and a follower's `cluster.sessions` would read 0, so that `doctor`
  could not compare the members.
- **The table rebuilt from position 0 at each election.** Lost: this version takes
  no snapshot, so the replay grows with the log, and an election would wait for it.
  A member that reads its committed log as it follows is at most one batch behind.
- **Sessions inside `Consensus`.** Lost: that file is 3,000 lines of election, and
  the sessions need nothing of it but who leads, in which term. A type of its own
  is tested by itself, with no cluster.
- **A redirect from a member whose own election has not closed.** Lost: the
  redirect names the member itself, and the client's next connect meets the same
  member in the same state. A connect answered with nothing waits on the ingress
  image, bounded by the image's window, and is answered when the election closes or
  fails.
- **A session message kept while the log is back-pressured.** Lost in ADR 0114: the
  page says "it buffers no message", and the message stays unread.
- **The closed event of a timeout sent when the timeout is decided, or tried
  once.** Lost: sent before the log holds the close, the event tells the client
  of a close that a leader which stops leading first never appends, and the next
  leader takes the session up. Tried once, the event never reaches a client whose
  publication is not connected, or is back-pressured, in that one cycle, where the
  ok event is owed until it is taken.
- **A session's close applied by the leader when it appends the close event.**
  Lost: the leader's table would run ahead of the log, and a leader that stopped
  before the entry committed would have closed a session every other member holds
  open.
- **More counters: one for each refusal's reason, one for each close reason.**
  Lost: the refusal's reason is in the session event the client reads, and
  `cluster.sessions_timed_out` beside `cluster.sessions_closed` gives the two close
  reasons of this version by subtraction.
- **A longer response channel, or one held outside the table.** Lost: 280 bytes is
  the longest channel of the form the page names, and a table of fixed slots is
  what keeps a member's sessions free of allocation.

## Evidence

- `crates/cavalry-raft/tests/sessions.rs`, thirteen tests on one `Sessions` driven
  by hand, the bounds first: an eleventh session is refused with `concurrent session
  limit` and a connect that finds the ten pending answers taken is dropped and
  counted; a connect is accepted, opened by its committed event and told ok, once,
  when its publication takes the event; a session open event waits for the log and
  is given up at 5 s; a follower redirects, with −1 when it knows no leader, and a
  member whose election runs leaves the connect unread; the refusals for the
  version, the response stream and three response channels, and the connects no
  member can answer; an answer unsent at 5 s is dropped; a session message is
  appended with the leader's term and time, left unread under back pressure, and
  ignored for another term, another session or on a follower; a silent session is
  closed at 10 s and a keepalive holds one; a timeout's closed event waits for its
  close event to be in the log; a closed event is owed until its publication takes
  it or the close is committed, and no ok event follows it; a client's close; a
  new leader takes up the sessions its log holds, with the
  timeout counted from its election's close and the next id after the log's; and
  the entries no leader of this version writes change nothing.
- `crates/cavalry-raft/tests/sessions_allocations.rs`: ten thousand rounds of a
  session message, a keepalive and a duty cycle, with a refusal, a close and a new
  session every hundredth round, under the counting allocator. No allocation and no
  reallocation.
- Aeron's sources named in Context.

## Consequences

Easier: one rule says who holds a session, so a new leader, a follower and
`doctor` read the same table; the next change of the work item gives the module an
ingress subscription and a read of its own log and changes nothing here; a member's
sessions are tested with no driver and no cluster.

Harder: the ok event waits for the leader to read its own committed entry, one
read of its log behind the commit; a session open event that a leader appended and
then gave up, and that a later leader commits, opens a session no client knows,
which holds a slot until the session timeout closes it, 10 s; a session whose
response channel the new leader's driver refuses hears nothing and is closed the
same way; and until the module drives `Sessions`, the eight counters are registered
and no member allocates them.

Revisit: when a client needs more than 10 sessions, for the bound and its key;
when the challenge of the session protocol is built, for a connect's credentials
and the principal a session open event carries; when a snapshot is taken, for the
sessions it must hold (ADR 0052, feature 6); when a service may close a session,
for the close reason service action.

## References

- `CLAUDE.md`: invariants 1, 2, 3, 4 and 6; Data plane; Protocol; Observability
- ADR 0052, ADR 0058, ADR 0066, ADR 0104, ADR 0114
- `spec/cluster.md`, "Sessions", "Roles and the election", states 6 and 10,
  "Counters" and "Values"; `counters.toml`, `cluster_session`;
  `crates/cavalry-raft/README.md`
- Work item 229 of the Brigade Board
- Aeron, read 2026-11-13 at `master` (1.55.0-SNAPSHOT):
  `aeron-cluster/src/main/java/io/aeron/cluster/ConsensusModuleAgent.java`
  (<https://github.com/aeron-io/aeron/blob/master/aeron-cluster/src/main/java/io/aeron/cluster/ConsensusModuleAgent.java>)
  and `aeron-cluster/src/main/java/io/aeron/cluster/SessionManager.java`
  (<https://github.com/aeron-io/aeron/blob/master/aeron-cluster/src/main/java/io/aeron/cluster/SessionManager.java>)
