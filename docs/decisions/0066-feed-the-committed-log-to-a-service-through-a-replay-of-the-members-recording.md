# 0066. Feed the committed log to a service through a replay of the member's recording

- **Status:** Accepted
- **Date:** 2026-11-10
- **Invariants affected:** 1, 2, 3, 5, 6
- **Supersedes:** none. Amends ADR 0052, feature 3, and ADR 0060 in one clause, what
  a service subscribes to, and `spec/cluster.md`, whose version stays 0, since no
  field of a message changes: "The service"; states 6 and 10 of "Roles and the
  election", whose replays feed the consensus module's own state and no service;
  and "The log" and "Counters", which say what a member's log is made of once the
  member has joined the log again, and which recording the member names.

## Context

Card 227 feeds the committed log to a service: a program beside a cluster member that
applies each entry once a quorum holds it. ADR 0052 asks for "a subscription on the
member's log stream bounded by the commit position, through the client API and the
ABI", and ADR 0060 names the subscription: the member's `log` endpoint and stream 100.
Building it found three things that sentence does not survive.

The leader has no such subscription to add. The leader's `log` endpoint is the control
address of its own log publication (`spec/cluster.md`, "Members and streams"), and the
publication sends to the other members' endpoints. Nothing sends the log to the
leader's host. Aeron's answer is a spy subscription, which reads a publication's log
buffer on the publisher's own driver. Cavalry has none: `spec/udp.md` and
`spec/client-protocol.md` name no such channel.

A subscription joins a stream where the stream is when the subscription arrives
(`spec/client-protocol.md`, "Commands": a subscription added later "joins there and
sees nothing before it"). A service is a process of its own, in any of five languages,
started by an operator. If it starts after the leader's first frame, or stops and
starts again, a subscription on the log gives it the log from the middle. Aeron closes
that gap with a service container that the consensus module starts, joins to the log
and waits for, over a control protocol of its own. That protocol is the kind of thing
an operator has to understand when it fails.

A follower's image holds what arrived, not what is on its disk. A member counts for a
commit by its recording's synced position (ADR 0060), so what a member has durably is
its recording, and the image is ahead of it.

The durable log already gives the other path. A replay with a length of 2^63 − 1
follows a live recording: it "reads up to the synced position and waits for it to
move", and it ends at the recording's stop position when the recording stops
(`spec/client-protocol.md`, "Commands"), which is a position a sync covered whatever
ended the recording (ADR 0064, ADR 0065). Every member records the log, the leader
its publication and each follower the leader's image, at the same positions.

## Decision

A service is a client of its member's driver, as the consensus module is (ADR 0063).
It reads the member's own recording of the log through a replay that follows it, into
a `cavalry:ipc` subscription of its own on stream 104, the service log stream, and it
polls the replay's image no further than the member's commit position. The path is
the same on the leader and on a follower, and a service may start at any time, from
any position of an entry that the recording its member names holds.

The member's own replays of its log, states 6 and 10 of `spec/cluster.md`, leader
replay and follower replay, feed the consensus module's own state, the open sessions
and their ids, and no service. The page said "into its service" there, a mechanism
nothing defines once a service reads the recording itself.

The member says which recording holds its log in a counter, `cluster.log_recording_id`,
one per member with the member id as its key like the member's other counters: −1
while the member records no live log, else the id of the recording it records the
live log into, the leader's of its log publication or a follower's of the leader's
image it joined last, and never the recording of a replicated or caught-up span. The
service's
bound is the member's `cluster.commit_position`. Two rules of order make the pair
safe to read. A member adds `cluster.log_recording_id` and stores −1 in it before it
adds `cluster.commit_position`, and a service reads the recording's id only of a
member whose commit position it has found, so it never reads the 0 a counter holds
between its allocation and its first store, which is a recording id. And a member
stores another value in `cluster.log_recording_id` before it stores a commit position
that the recording does not hold.

The client API gains two things.

- `Subscription::bounded_poll(session_id, handler, limit_position, fragment_limit)`
  polls one image, the one of `session_id`, and delivers no frame that begins at or
  past `limit_position`. In Aeron, this is `Image.boundedPoll`. `spec/log-buffer.md`,
  "Reading", says what a bounded read is. A bound cannot be applied after the read:
  a frame a poll has delivered is behind the image's position and cannot be handed
  back.
- `Cavalry::committed_log(position)` returns a `CommittedLog`, or nothing while the
  driver hosts no member that has a recording of its log. It finds the member's two
  counters, adds the subscription, and starts the following replay of the recording
  from `position`. `CommittedLog::poll(handler, fragment_limit)` loads the commit
  position and makes the bounded poll; the handler gets each entry's bytes, in
  `spec/cluster.md`'s encoding, the frame's header and the entry's position. An entry
  is one frame in this version, since the leader refuses a payload that does not fit
  one; a longer entry would arrive as its fragments, as from `Subscription::poll`.
  Another session on stream 104, a second service's replay on the same driver, is
  read past and dropped in the same call, so that no service holds another's replay
  at its own position.

The bound is read as a recording's position is read since card 227's change to the
`Recording` handle, with one check more. After each load of the commit position the
reader checks that the counter's record is still the member's, by its type and the
member id in its key; then it loads `cluster.log_recording_id` and checks that it
still holds the id of the recording being replayed, and that this record too is the
member's. From the first check that fails it reads the counters no more and keeps the
highest commit position it took. A member's counters are freed when its consensus
module closes, and the driver gives a freed record to another counter after the
counter reuse timeout, to a later member with the same id among others; a service
must never take another counter's value, or a commit position of another log, for
its bound. The second rule of order above is what makes the check enough: an id that
is still the recording's, loaded after the commit position, says that commit position
was stored while the member's log was that recording. The bound never falls.

When the member's recording stops, because the member closed, left the log to begin
its election again or, from card 228 on, left a term, the replay ends at the stop
position. The service has read no further
than the last commit position it took, `CommittedLog::is_ended()` says that the handle
has come to its end, and the service opens the log again at its own position,
`CommittedLog::position()`, when a member has a recording again.

A member that left the log and joined it again names a new recording. An armed
recording starts with the stream's next image, at its join position (ADR 0059), and
a follower joins an image only at its append position (ADR 0063), so the new
recording starts at the member's append position and holds nothing below it. A
service whose position is that start reads on. A service whose position is below
it is refused the replay, `start replay`'s code 23, and reports the refusal to its
caller. The entries between are in the recording the member left, past the last
commit position the service took. This version stops there: it names no earlier
recording to a service, and it does not say when a service may take the entries of
a recording its member has left as committed, which depends on what an election may
truncate (To maintain).

The C ABI's calls for this, and the five clients', come with the ABI's next version
under an ADR of their own.

## Alternatives considered

- **A subscription on the member's `log` endpoint and stream 100, ADR 0060's
  sentence.** Lost, for the three reasons above: no such stream reaches the leader's
  host, a service that arrives late or returns has missed entries, and an image is
  ahead of what the member holds durably. A follower's service could use it and the
  leader's could not, which is two paths to test and to explain.
- **A spy subscription for the leader's service, as Aeron has.** Lost. It is a new
  channel form in `spec/udp.md` and in the driver for one reader, and it leaves the
  second reason standing: a spy joins where the publication is.
- **A service container the consensus module starts and waits for, Aeron's
  `ClusteredServiceContainer` with its join log and service ack messages.** Lost in
  this version. It exists to order a service's start against the log's, and the
  recording removes the need for an order. A snapshot, card 230's, will need the
  service to answer the member, and that ADR decides how.
- **The consensus module publishes each committed entry on an IPC stream for
  services.** Lost. The module does not hold the entries: it would read its own log
  back, which is a replay, or keep each entry until it commits, a buffer whose bound
  is the uncommitted log. A service that starts later still needs what came before.
- **The service reads the segment files.** Lost. The files are the driver's
  (`spec/durable-log.md`, "The driver"); a file does not say how far it is synced;
  and every client language would parse them.
- **A poll with no bound, and a handler that stops at the commit position.** Lost: a
  delivered frame cannot be handed back, so the service would hold what it read past
  the bound, a queue with no bound of its own.
- **The member id and the consensus module's client id as the counters' key, so that
  a later member's record can never pass for an earlier one's.** Lost as the only
  check: within one member's life the commit position goes on into the next term
  while the recording a service replays has stopped with entries past the last commit
  in it, and only the recording's id tells the two apart. With that check made, the
  longer key adds nothing the order of the stores does not give.
- **No counter for the recording: find it among the driver's recording counters by
  its channel and stream.** Lost. The leader records a publication on a control
  address and a follower an image on an endpoint, two forms to match from text
  written for people, and a member with more than one term may hold more than one
  recording. A counter the member stores is the member's own statement.

## Evidence

- A prototype on the laptop, three drivers and three consensus modules in one process
  on the loopback, with a second client on each member's driver following that
  member's recording: the three services read the same 5,001 entries, the term's
  first and 5,000 offered, in 0.97 s, the recorder's one-second sync, and no replay
  had ended. Two of three members did not yet have their recording when their
  election closed, which is why the counter starts at −1.
- `spec/client-protocol.md`, "Commands": a replay of a live recording reads to the
  synced position, and ends at the stop position with reason 0.
- The tests this decision lands with are named in the pull requests that build it:
  the bounded read at a frame's boundary and inside a frame; the bound kept when the
  member names another recording, when its records are freed, and when they come back
  as a later member's with the same id and the same recording id, 0; and a service on
  each of three members reading the same entries at the same positions while the
  leader appends, none of them an entry the leader's recording holds and no quorum
  has yet.
- The allocation count that test's twin ran under found the driver's replayer
  allocating twice a second for every replay that follows a live recording, the
  catalog descriptor's two strings. That is fixed apart from this decision, with a
  gate of its own, since a service is such a replay.

## Consequences

Easier. A service starts before, with or after its member, and reads from position 0
or from the position it had: there is no order of starting to get wrong (invariant
5). A service reads an entry only when it is on this member's disk and a quorum holds
it (invariant 1). One path serves the leader and a follower. Card 230's snapshot gives
a service a position to start from, and this call takes one.

Harder. An entry reaches a service after this member's recorder has synced it, at
most the recorder's second behind the commit until the consensus module can ask for a
sync, which needs a command the client protocol does not have yet. Each service is
one of the driver's 64 replays and a replay log of the recording's term length, three
terms of 16 MiB of shared memory, and each entry is copied once from the file into
that log by the replayer. A service on a member that left the log and joined it
again reads on only if it had read to the end of the recording the member left;
otherwise it is refused with code 23 and says so. In this version that member is a
follower whose image was lost, or that heard nothing from its leader for the leader
heartbeat timeout.

To maintain. The reader's two counters by type and key, and the member's two rules of
order. Card 228 decides how a reader passes from one term to the next. If a term's
log is a recording of its own, the counter names it and a service opens the log again
there. If a member's log stays in one recording across terms, as Aeron extends its
recording, the id alone no longer says that the log changed under a reader, and that
ADR gives the counter something that does. Card 228 also truncates a follower's
uncommitted entries, which the driver refuses while a replay reads the recording
(`spec/client-protocol.md`, code 24); the recording's stop ends this replay first, at
the stop position. Cards 228 and 230 also decide how a service reads a log of more
than one recording: which recordings hold the positions below the one the member
names, how a service finds them, when it may take their entries as committed, and so
what it does where `start replay` answers code 23 today. A member has more than one
recording after it joined the log again, which this version already has, after a
later term (card 228) and after a replicated or caught-up span (card 230). Services
on one driver share stream 104 and each reads past the
others' replays: a subscription on one session, Aeron's `session-id` key, would end
that, and is worth its change to the channel URI when a member hosts enough services
for it to show in a measurement.

Revisit when the bench measures the time from a commit to a service's read as larger
than one sync, or when a service needs the log sooner than its member's disk has it.

## References

- ADR 0052, feature 3; ADR 0060; ADR 0063; ADR 0059 (an armed recording starts with
  the stream's next image); ADR 0036 (a recording's counters after
  its stop); ADR 0064 (the position a stop that failed is announced at); ADR 0065
  (a recording one of whose syncs failed is given up at the position last synced).
- `spec/cluster.md`, "The service", "The log", "Counters" and states 6 and 10 of
  "Roles and the election"; `spec/log-buffer.md`, "Reading";
  `spec/client-protocol.md`, "Commands", start replay.
- Aeron: `Image.boundedPoll`, `BoundedLogAdapter`, `ClusteredServiceContainer`.
- Card 227.
