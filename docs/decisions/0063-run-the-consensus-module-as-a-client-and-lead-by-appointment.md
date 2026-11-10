# 0063. Run the consensus module as a client of its driver, and lead by appointment through the election's states

- **Status:** Accepted
- **Date:** 2026-11-10
- **Invariants affected:** 1, 3, 4, 6
- **Supersedes:** none. Amends ADR 0052 and ADR 0060 in one clause, how the consensus
  module reaches the driver, and adds the appointed leader's path to `spec/cluster.md`,
  whose version stays 0: no field of a message changes.

## Context

Card 227 builds the replicated log with an appointed leader and no election (ADR
0052, feature 3; ADR 0060). `cavalry-raft` is a scaffold with no public item. Five
things stand between ADR 0060 and its first line of code, and each is a decision.

First, how the module reaches the driver. ADR 0052 and ADR 0060 say the consensus
module is the driver's sixth dedicated thread, "linked to the conductor by SPSC queues
as the recorder, the sender and the receiver are". ADR 0062 says the module "reaches
the driver as every program does, as a client". The two disagree, and only one can be
built. Since ADR 0060 was written, every capability the module needs has become a
command of `spec/client-protocol.md`: a publication created at a position and a
recording armed on a subscription (ADR 0059), and a counter the module owns (ADR
0062). In Aeron, the consensus module is a client of the media driver and of the
archive.

Second, `spec/cluster.md` has no path for an appointed leader. Its state 0 sends "a
cluster of one member" to leader log replication and "every other member" to canvass,
and a member becomes leader of three only through nominate and the ballot, which card
228 builds. ADR 0060 appoints "the member with the lowest id in the string" and says
nothing of the states it passes through. Behaviour the page does not state is a bug in
the page (`CLAUDE.md`, Protocol).

Third, what a member of this feature cannot do, and what its leader must not do. A
follower joins the leader's log at the image's join position, which must be its
append position (state 15). A member that arrives after the leader's first entry, or
restarts with a log, needs the entries it lacks from a replay, which is the catch-up
of states 9 and 11 to 13 and card 230's work, and a member ahead of the leader needs a
truncation that only an election decides. Aeron covers a cold start in canvass: a
member nominates itself at once only when every member has reported with no log
ahead of its own, and with a quorum only after the startup canvass timeout, 60 s.
Aeron's appointed leader (`ConsensusModule.Context.appointedLeaderId`) canvasses so
too, then nominates and wins a ballot, and every other member canvasses and never
nominates. A leader appointed straight from init, as a cluster of one is, compares no
log. Restarted on a lost or new log directory, a container's ephemeral one or an
operator's remedy for a refused restart, it would lead again the term its followers
already hold, from position 0, count their old positions at that term toward its
commits, and commit entries that no follower holds (the review of pull request 165).

Fourth, the log has no entries to replicate until card 229's sessions give a client a
way in, and this card's gates need 100,000 of them.

Fifth, ADR 0060 has the module ask the recorder to sync after each append position it
reports, through "the bounded request queue the conductor already holds". A client
has no command for that.

## Decision

The consensus module is a client of the driver in its directory. `cavalry-raft` takes
`cavalry-client` as a dependency and holds one `Cavalry`: it adds the consensus
publications and subscription, the log publication with its destinations or the log
subscription with its armed recording, and its counters, through the commands every
program uses. `cavalry run` starts it on the driver's sixth dedicated thread, as ADR
0060 says, with a client of the driver's own directory in place of SPSC queues to the
conductor. The crate has two layers. The core is the state machine of
`spec/cluster.md` and does no I/O of its own: it takes the time and the messages, and
reaches everything else through the few operations it is given, send a message to a
member, store the candidate term, record a term, add or join the log, read the append
position, append an entry. The module gives it those operations through the client
and the `terms` file. A test gives it the same operations in memory, three cores at
once, with any order, loss and delay of delivery, and card 228's election extends the
core without touching the module's I/O.

`spec/cluster.md` gains "The appointed leader", the path this feature takes through
the states of "Roles and the election" until card 228 replaces it. The member with the
lowest id in the members string is the appointed leader. Every member of a cluster of
more than one canvasses, as in Aeron. Where canvass's rule makes the appointed leader
a unanimous or a quorum candidate, it goes to leader log replication as leader of
candidate term + 1, without nominate and the ballot, since no member of this version
votes. Every other member canvasses and never nominates. Canvass's rule gains one
condition for the appointed leader: it does not lead while a member that has reported
holds a log ahead of its own ("Logs compared"), whatever the deadline. In this version
every member starts with no term, and the appointed leader never gives up a
leadership it has taken. So a member ahead of it in canvass holds a history the
leader lost, and no truncation can bring that member back. Aeron's quorum candidate
would lead without it, and discard the history.

In leader ready the appointed leader waits again with canvass's two cases, since a
member that reported in canvass has still to join the term. It closes its election at
once when every member counts for its term at or past its log position, and with a
quorum once the startup canvass timeout has passed since its canvass began, the one
deadline of a cold start, so that a cluster started together replicates to every
member from the term's first entry. It does not close while the last report of any
member holds a log ahead of its own: that is a member that holds the history and was
silent through the canvass. Each member the appointed leader finds ahead, in canvass
or in leader ready, is counted once in an election in
`cluster.appointed_leader_behind`.

A follower that cannot join with what this version has fails its election, counted in
`cluster.elections_failed`, and canvasses again: one whose new leadership term asks
for a truncation, a replication or a wait, and one whose append position is behind
the leader's log position after its replay. A leader whose active members fall below a
quorum stays the leader, and its commit position stops, since there is no election to
hold. Each fall is counted in `cluster.leader_quorum_losses`, so that a stopped commit
position is told apart from a slow one; the election's leader holds an election
instead. A member starts with a record that holds no term and no vote. One whose
`terms` file holds either refuses to start, with an error that says what to do next:
start a new cluster, with a new log directory on every member. A new log directory on
the leader alone is the lost history that the canvass guard keeps from leading. On a
follower alone it is a member that cannot join until card 230's catch-up.

The `appointed-leader`, `appointed-canvass` and `leader-ready` vectors pin the three
rules that are arithmetic. The page's Counters table gains `cluster.terms_led`, the
transition from follower to leader, `cluster.appointed_leader_behind` and
`cluster.leader_quorum_losses`, three transitions that no counter named.

Until card 229's sessions, the leader's module appends for its own process:
`ConsensusModule::offer(payload)` writes a session message whose cluster session id is
−1, no session's, with the leader's term and the cluster's time, once its election has
closed. The page says what −1 means there. A session's id is 0 or more, so the two
never meet.

The request to sync waits for a command of its own in `spec/client-protocol.md`,
under its own ADR. Until then a member's append position rises at the recorder's own
sync, once a second, and a commit follows the slowest member of the quorum by up to
that second. Nothing committed is lost by the wait: the commit position is still a
quorum of synced positions (ADR 0060).

## Alternatives considered

- **SPSC queues between the module and the conductor, as ADR 0060 wrote.** Lost. The
  conductor would need a second entry for every command the module uses, the log
  publication, the destinations, the subscription, the armed recording, the counters,
  each with the registry rules and the answers the client protocol already gives, for
  one user in the same process. The module would be testable only inside a driver.
  As a client it runs in a test beside three drivers, and in another process if a
  deployment ever wants that.
- **One layer, the state machine calling the client as it goes.** Lost: invariant 1 is
  proved by orderings, a heartbeat late, an append position before the term is
  recorded, a message from an older term, and a state machine that does I/O can be
  driven through them only with real sockets and sleeps. The core's cost is a call
  through a type parameter, resolved when the module is compiled.
- **Appoint the leader from init, with no canvass, as a cluster of one goes.** Lost:
  it compares no log. A leader restarted on a lost or new log directory leads term 0
  again from position 0. Its followers time out and canvass with term 0 and their old
  positions, and a canvass position at the leader's own term updates its table. So it
  counts them at those positions, closes its election and commits its own appends up
  to them, while no follower holds those entries. Each follower fails state 15 with a
  log of different bytes at the same term and positions, and the committed entries
  live on one member until its next ephemeral restart. This was the first draft of
  this ADR.
- **Canvass's rule as Aeron has it: lead as a quorum candidate with a member ahead.**
  Lost for this version. In Aeron a minority ahead holds only entries that were never
  committed, and the new leader truncates them. Here a member ahead of the appointed
  leader holds a history that the leader lost, and there is no truncation. In three
  members of which two ran, the leader's directory lost, the leader and the member that
  never ran make a quorum at 60 s. The leader would discard the third member's log,
  committed entries among them, and count its old positions under the same term id.
  The added condition costs nothing at a cold start, where no member is ahead.
- **No canvass, and only the leader-ready guard: in states 5 to 8, count no member
  ahead and do not close while one is.** Lost as the only guard. The leader would
  record a term and send new leadership term in a cluster it must not lead, and the
  members with empty logs would record that term over nothing. Canvass stops it before
  any member records anything. The guard is kept as the second one, for a member that
  holds the history and was silent through the canvass.
- **Close leader ready on a quorum, as the election's leader does.** Lost for the
  appointed leader alone. Two of three members would close the election and append
  the term's first entry while the third, which reported in canvass a moment before,
  joins an image whose join position is past its append position, fails state 15 and
  cannot catch up before card 230: a cluster of three would run as two at most
  starts. Canvass's startup rule, every member or a quorum after 60 s, is Aeron's own
  answer to a cold start, applied again in the state where the members join.
- **Let a member restart on its log.** Lost for this feature. A restarted leader's
  append position is what its recorder had synced; a follower may hold more, which
  only a truncation resolves, or less, which only a catch-up resolves. Starting it
  anyway would have it lead a term its followers cannot join, or follow a log it
  cannot extend. Refusing with the reason costs the operator a new cluster, a new log
  directory on every member, and loses nothing a later card depends on. A new log
  directory on one member is not the remedy: on the leader it is the lost history
  above, which the canvass guard keeps in canvass, and on a follower it is a member
  that cannot join until the catch-up.
- **A test-only way to append, behind a feature flag.** Lost: the README's example is
  a doctest and must replicate something, and a service in the leader's own process
  is a use Aeron has too, its service messages, which it also appends under ids no
  session holds.
- **A sync after every append position, by a command added here.** Deferred, not
  lost: it is a change to `spec/client-protocol.md`, a version and an ADR of its own,
  and the replicated log is correct without it. It comes before the commit latency is
  measured.

## Evidence

None measured. What informed it: `spec/cluster.md`, "Roles and the election", states
0, 1, 5 to 8, 10 and 14 to 17, and canvass's startup rule; ADR 0059, whose two
commands are the ones a follower and a leader need; ADR 0062, whose Context already
describes the module as a client; Aeron's `ConsensusModuleAgent`, a client of its
media driver through `Aeron` and of its archive through `AeronArchive`, and its
`Election`, read 2026-11-06 for ADR 0058, whose canvass lets no member but the
`ConsensusModule.Context.appointedLeaderId` one nominate; the `candidate` vectors,
whose two cases `appointed-canvass` and `leader-ready` copy; and the review of pull
request 165 on 2026-11-10, which traced a leader restarted on a lost log directory
through the page's own rules to entries committed on one member. What the
implementation proves goes in its commit messages: three cores in memory under
reordered delivery, three modules beside three drivers replicating 100,000 entries
with equal recordings, a member that starts late refused and counted rather than
joined wrong, and an appointed leader started on an empty log directory beside two
followers that hold term 0, kept in canvass and counted in
`cluster.appointed_leader_behind`.

## Consequences

Easier: the driver gains no code for the cluster beyond starting a thread; the module
is tested without a driver, and with three; card 228 adds nominate and the ballot to
a core that already has every other state; a program can run a member in its own
process.

Harder: the module pays the client protocol's round trip for each command, at its
start and at a change of term, never per entry; a cluster of this feature starts
together or waits 60 s for a member that does not come, and then runs without it until
card 230; no member restarts on its log until cards 228 and 230; a commit waits up to
a second for the slowest member of its quorum. An appointed leader whose log directory
was lost waits in canvass, counted, until the operator starts a new cluster. The guard
sees only a history that a reachable member reports. A leader whose log was lost,
together with a quorum of members that never ran or lost theirs too, leads before a
silent member that holds the history comes back. That is a quorum that lost its log,
which neither Raft nor Aeron can recover. The remedy is the same: a new cluster, with
the old directories kept for what they hold.

Revisit: when card 228 builds the election, which removes "The appointed leader" from
the page and the refusal to start on a log from the module; when card 230 builds the
catch-up, for the follower that joins late; when the commit latency is first
measured, for the sync command; when card 229 gives sessions their ids, for the
meaning of −1.

## References

- `spec/cluster.md`, "The appointed leader", "Roles and the election", "The log" and
  "Counters"; `spec/vectors/cluster.json`, rules `appointed-leader`,
  `appointed-canvass` and `leader-ready`.
- ADR 0052, feature 3; ADR 0058; ADR 0059; ADR 0060; ADR 0062.
- `crates/cavalry-raft/README.md`.
- Pull request 165's review, 2026-11-10, for the path of a leader restarted on a lost
  log directory.
- Aeron's `io.aeron.cluster.ConsensusModuleAgent`, `io.aeron.cluster.Election` and
  `ConsensusModule.Context.appointedLeaderId` (`aeron.cluster.appointed.leader.id`).
