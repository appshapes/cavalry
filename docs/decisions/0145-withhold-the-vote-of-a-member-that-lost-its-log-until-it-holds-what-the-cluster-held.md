# 0145. Withhold the vote of a member that lost its log until it holds what the cluster held

- **Status:** Accepted
- **Date:** 2026-11-22
- **Invariants affected:** 1, 4, 5, 6
- **Supersedes:** of ADR 0104, the rule "One member is never started alone on a new
  log directory", left to an operator, and its alternative "Have a member whose
  record holds no term cast no vote for a candidate that holds a log, until it has
  joined a log", which it lost and this decision takes up with a vote floor. Of ADR
  0108 and the refusals of `cavalry-raft` and `cavalry-log`, the advice "Do not start
  this member alone on a new log directory". Of ADR 0144, the consequence that a
  member whose host lost power under a driver of an earlier version stays out: it
  starts on a new log directory. Moves `spec/durable-log.md` to version 6; the terms
  file's header version stays 0, and `spec/cluster.md` stays at version 1, since no
  message changes.

## Context

A cluster member that starts on an empty log directory, where it had a log before,
has forgotten the votes it cast and the entries it acknowledged. A container whose
log directory is on no volume does this at every restart, so the trigger is an
orchestrator's default. Since the consensus module elects (ADR 0104) such a member
joins and votes. With a member whose log is behind, it is a quorum that can elect a
leader without an entry the cluster committed: an entry committed by the leader and
this member, the leader stopped, and the member's vote given to the third, which
lacks the entry. ADR 0104 named that cost against invariant 1 and left it to an
operator's rule that nothing checks: `cavalry run` cannot tell that directory from a
new cluster's, no counter counts the start, and `cavalry doctor` cannot say it
happened. Work item 286 is the change that removes the rule, the second part of
feature 3 of the beta slice (ADR 0138), whose boundary leaves a start from a peer's
snapshot, a member replaced on a new host and membership told between members to
feature 5 (work item 365).

ADR 0104 weighed one guard and lost it: a member whose record holds no term casts no
vote for a candidate that holds a log until it has joined a log. It lost for two
reasons. It is not live, since a member slow to join a new cluster's first term never
votes if that term's leader stops. And joining is not enough, since the member does
not know what it had acknowledged. The standby (ADR 0134) gives a member that follows
with no vote, but only when an operator marks it in the members string, and a mark
stays until an operator changes it.

Aeron's answer, read on 2026-11-22: its open-source `Election` has no check on a
member whose node state file and recording log are empty, and votes on the term and
the log compared alone. Its documentation rebuilds a lost member from a Cluster
Backup, an operator's procedure that reuses the member's entry in the cluster's
members (aeron.io, "Cluster Backup", "Rebuilding a Cluster Member"). Cluster
Standby is a premium component. So Aeron holds this fault by an operator's hand, as
ADR 0104 did, and there is no name of Aeron's to keep for what follows.

## Decision

**A member withholds.** It does in two cases: while its log is empty, log leadership
term id −1 at position 0, and a member's report in this election, or the request it
answers, holds a log; and while its append position is below its vote floor. A
member that withholds never goes from canvass to nominate, and it answers a request
vote with nothing, as a standby does, counted in `cluster.votes_withheld`. A member
whose log is empty beside members whose logs are empty too is one of a new cluster,
and votes as any voter does.

**One case in which it votes.** When each voter of its members string has reported
its log in this election, itself and the candidate among them, and the candidate's
log is at least each, a member that withholds answers as any voter does. A log at
least that of a member that holds a committed entry holds the entry, as Raft's vote
rule has it, so such a candidate holds every committed entry that any voter still
holds. This case keeps a cluster live when two of three members lose their logs
while the third holds the whole log: every voter is heard, and the third is voted in.

**The vote floor.** A member whose log is empty that takes a leader on a new
leadership term stores that message's log position, the leader's append position,
as its vote floor, before it records a term. A leader taken by the rule below holds
every entry the cluster committed, so a member whose append position has reached its
floor holds every entry it could have acknowledged before it came back, and votes as
any voter does from then on. A message whose log position is 0, a new cluster's first leader's,
gives no floor. The floor is one slot of the terms file's header, bytes 48 to 60
until now reserved: the position under its CRC32, written once and synced. A torn
write reads as no floor, and since the floor is written before the first term the
member then finds its log empty again at its next start. A member reads its floor
at its start with its candidate term, so a member that stops below its floor and
starts again on that directory withholds still. `cluster.vote_floor` holds it. The
leader's log can end in a tail no quorum has recorded, which a later leader cuts, so
a floor can lie above the log the cluster then holds. The member withholds until the
log grows past its floor again, as the cluster's next entries take it. The floor is
never lowered: lowering it to a later leader's log end would be safe, but a write a
crash tore would then read as no floor at all, and a second slot that keeps the
floor before it, as the candidate term has, does not fit in the 20 bytes the header
has left.

**Taking a leader.** A leader that a later term deposed, and that has not heard of
that term, still leads with its election closed, and it answers every canvass
position whose terms are not above its own. Its log lacks what the later term
committed. A member that kept its candidate term ignores that leader, since it voted
in the later term or followed it. A member that lost its log has a candidate term of
−1, which is above no term. Its floor would then be the deposed leader's log end, and
it and the deposed leader would be a quorum whose commits replace the later term's.
So a voter whose log is empty takes a leader in two cases only. In the first, it
voted for that leader, in that term, in this election. In the second, each other
voter has been heard in this election, by a canvass position or a new leadership
term, and none named a term above the leader's. A canvass position names the
sender's candidate term, and a new leadership term names the term it leads. It
ignores any other new leadership term, counted in `cluster.leaders_withheld`.

The second case holds the floor. A committed entry that counted the member's
acknowledgement was recorded by another voter, which recorded the entry's term first
and keeps it, so that voter names a term at least the entry's. A leader of that term
or a later one holds the entry, by Raft's vote rule. The case holds the forgotten
vote too: the candidate it went to stored its term first, so it names a term at
least the vote's. The member's candidate term becomes the leader's, and it votes in
that term no second time. A member whose election has closed sends no canvass
position, so a follower would never be heard. So a member that holds a log, that
does not lead, and that is in a state other than canvass and nominate answers a
canvass position whose log is empty with its own. It answers that member alone, at
most once each election status interval, 100 ms. A member whose log is empty answers
none, so no two members answer each other. A standby takes any leader, since its
appends count toward no commit.

**Three pure functions and their vectors.** `withholds(own, vote_floor, log_heard)`,
`withheld_vote(voters, reports, candidate)` and `takes_leader(voters, own, heard,
term, voted)` in `cavalry-proto`. The `withholds`, `withheld-vote` and
`takes-leader` arithmetic vectors of `spec/vectors/cluster.json` pin them. They sit
beside the `candidate` and `request-vote` vectors.

**What an operator does now.** A member whose log directory is lost or damaged is
started on a new one. It comes back by itself once every other voter runs and
reaches it, and it waits while one does not. The refusals that named the old rule,
and `docs/how-to/run-a-cluster.md`, say so. Where the leader's log no longer
begins at position 0, its recordings purged below a snapshot, the member cannot
catch up and withholds until a start from a peer's snapshot is built (work item
365); the advice for that case is unchanged.

## Alternatives considered

- **ADR 0104's guard as it was: no vote until the member has joined a log.** A member
  that has joined holds only what it has replicated so far, and its vote for a
  candidate behind the committed log is the same fault a moment later. And a member
  that stops while it catches up starts again with a term in its record and a short
  log, and votes. The floor answers both: what a leader held when the member came
  back, and kept across a restart.
- **An identity that changes with the log directory**, so that the others see a new
  member and count it only once it holds the log. Each member would carry its
  directory's identity in canvass position, request vote and vote, a change of three
  layouts and so a new major of `spec/cluster.md`. Every member would keep a durable
  record of each other's identity. And a follower hears another follower only in an
  election, so after a quiet leadership a follower that never heard a member's old
  identity counts its new one as a new member's, which is the fault again. Closing
  that gap is membership through the log.
- **A change of membership through the cluster's own log, as Raft has it.** The lost
  member is removed and a new one is added by entries the cluster commits. That is
  the whole answer, and it is feature 5's (work item 365; ADR 0138): a member
  replaced on a new host, members telling each other the voters. It is several times
  this change's size, and it adds entries whose effect on every member's state is a
  rule of the major.
- **The others' durable record of what each member reported.** Only a leader reads a
  member's append position, and a leader that stopped takes its record with it unless
  every follower is told, which is a message of its own.
- **Refuse a start on an empty log directory beside members that hold a log.** That
  refuses every member that starts late in a new cluster (ADR 0104's second loser),
  and it leaves the operator a lost member to bring back by hand.
- **Start such a member as a standby** (ADR 0134). A standby's appends count toward
  no commit, and a standby stays one until an operator changes the members string on
  every member, one mark at a time. A container restarted with no volume would need
  an operator at each restart.
- **Withhold only from a candidate whose log is behind the longest log heard.** Safe
  only when the member holding the committed entry is heard. The fault is exactly the
  case in which it is stopped.
- **Take any leader that answers, the floor alone guarding the vote.** This was the
  first draft of this decision, and the pull request's review found the schedule
  that breaks it. A leader that a later term deposed answers the member, which takes
  that leader's log end as its floor. It follows the leader, and the two are a
  quorum that commits over the later term's committed entries. The test of the
  Evidence fails on it.
- **Have the leader answer a member whose log is empty only while a quorum of the
  other voters has recently reported its term.** A deposed leader stays fresh for as
  long as the window, and a report that arrives late refreshes it. Every window is a
  bet on clocks and on processes that never stand still. It also leaves the member
  trusting a leader of a build that does not hold the rule.
- **Count a member whose log is empty out of a leader's quorum.** That stops a
  deposed leader from staying quorate on the member's canvass positions. It does
  not stop the member from taking that leader's term and floor, and the two still
  commit together.
- **Hold the two cases above to the vote too, so that a member whose log is empty
  never votes until it has heard every voter.** That closes the residual case of the
  Consequences. But a new cluster then elects no leader while one of its members has
  not started, and no member can tell the two apart.

## Evidence

- `crates/cavalry-raft/tests/lost_log_directory.rs`, in memory, with the check of
  `elected_cluster` after every step that no committed entry is lost or replaced:
  - `a_member_that_lost_its_log_votes_no_leader_in_that_lacks_what_it_had_acknowledged`
    is the history work item 286 names. Ten entries reach all three members. Ten more
    are committed by the leader and one follower, the third cut off. That follower
    starts again on an empty log directory, the leader stops, and the third runs.
    For 30 s no leader is elected: the wiped member withholds every request, and the
    third stands and is not elected. Once the leader runs again it leads, and the
    three end on one log that holds all twenty entries. The wiped member's floor is
    at least their end, and past it the member is a voter as any other.
  - The same test with `withholds` forced to false fails: a leader is elected while
    the member holding the entries is stopped.
  - `two_members_that_lost_their_logs_vote_in_the_one_that_holds_the_log`: both
    followers stop for 15 s, so the leader loses its quorum and holds an election,
    and both come back on empty log directories. They vote the leader in again in a
    later term, through the one case above, and replicate its log.
  - `a_new_clusters_member_that_holds_nothing_when_its_leader_stops_waits_for_the_leader`
    holds the cost. A new cluster's third member is cut off from the start, and the
    two others elect and commit ten entries. Then the leader stops and the third is
    heard: for 30 s no leader is elected, and the third withholds and never stands.
    Once the leader runs again, the three end on its log.
  - `a_member_that_lost_its_log_follows_no_leader_a_later_term_deposed` is the
    review's schedule. A leads term t and reaches nobody. The voter still reaches A,
    so its canvass positions keep A quorate. The other two elect a leader in t + 1
    and commit ten entries that A lacks. Then the voter loses its log, the new leader
    stops, and A reaches the voter again, while A's election is still closed. For
    6 s A takes offers and commits none. The member with no log takes no leader,
    stores no floor and counts `cluster.leaders_withheld`. Once the leader of t + 1
    runs again, the three end on its log, and the member's floor is at or past what
    it committed. Without the rule of "Taking a leader" it fails at the check of
    every step: `member 1 leads with a log that differs from what was committed`.
    That is the draft of this decision before the review, on db85cd0b.
- `spec/vectors/cluster.json`: sixteen arithmetic vectors, five `withholds`, five
  `withheld-vote` and six `takes-leader`, read by
  `crates/cavalry-proto/tests/cluster_conformance.rs`.
- `spec/vectors/durable-log.json`: five terms header vectors for the floor slot,
  read by `crates/cavalry-log/tests/conformance.rs`. `crates/cavalry-log/src/terms.rs`,
  `the_vote_floor_is_stored_once_and_read_back_and_a_torn_one_reads_as_none`.
- `cargo test -p cavalry-raft`: every test passes, the cluster tests over three
  drivers and the restart tests among them. A member restarted on the directory it
  kept holds no floor. A new cluster's follower takes the leader's append position
  when it first hears the leader, 224 bytes at one seed of the test below, and
  reaches it as it replicates the leader's first entries.
- One test changed: `late_new_leadership_terms_cut_nothing_that_was_committed` in
  `crates/cavalry-raft/tests/election.rs`, three members whose consensus messages
  are up to 2 s late. At seed 3 the first leader was stopped before either follower
  held a byte of its log. Before this decision another member led. Now none does
  until the first runs again, since neither can tell that its empty log lost nothing.
  The test now runs its first election with messages on time. Its counts of the rule
  of ADR 0096 under late messages fell from twelve drops and three truncations to two
  and one, and both of its assertions on them hold.
- Found along the way, on `master` at 61ae0bd7 with none of this change: the same
  schedule with one more wait before each stop leaves two members that hold the same
  log canvassing for 120 s at seed 5. That is work item 406, a liveness fault of the
  election that this decision does not touch.

## Consequences

- A member whose log directory is lost comes back on a new one by itself, follows no
  leader that a later term deposed, and votes no leader in that lacks an entry it had
  acknowledged. The operator's rule of ADR 0104 is gone. One case is left: two
  members that hold no log while the third is stopped, below. No operator's rule
  held it before either.
- A voter whose log is empty takes no leader while another voter, not that leader,
  is stopped or cannot reach it, unless it voted that leader in. This holds even when
  the leader is the newest term's. So a cluster of three in which one member lost its
  log and another is stopped has a leader and no follower of it. It commits nothing
  until the stopped member runs again, and `cluster.leaders_withheld` counts the
  wait. No member can tell that leader from a deposed one while the member that
  would name the later term is stopped.
- A follower that holds a log answers a canvass position whose log is empty, at most
  once each election status interval, 100 ms. A follower of a build before this
  decision does not answer, so a member that lost its log during an upgrade waits
  until that follower canvasses or is upgraded.
- While a member withholds, the cluster tolerates one failed voter fewer, and a
  member that came back catches up from position 0, so the window grows with the
  log. A volume for the log directory spares it, which the how-to's prerequisite
  says.
- A cluster of three in which one member withholds elects no leader while either
  other member is stopped, until it runs again, even when what it held was never
  committed. A new cluster in which one member holds nothing yet when the first
  leader stops is the plainest case, and no member can tell it from the case the
  rule exists for. ADR 0104 lost the guard for this, and this decision takes it for
  invariant 1's sake. A volume for each log directory makes it rare, and
  `cluster.votes_withheld` counts it while it lasts. Feature 4 states the bound
  within which a stopped leader is replaced (work item 364), and names this case
  beside it.
- A floor above the log the cluster holds, after a leader change cut the tail it was
  taken from, keeps the member withholding while the cluster appends nothing. The
  cluster's next entries end it.
- Two members of three that lost their logs while the third is stopped elect between
  them as a new cluster does. A quorum's record is gone, which no rule gives back.
- So do two members of three that hold no log, one that lost it and one that never
  held a byte of it, while the third, which holds the log, is stopped. The one that
  never held a byte is a new cluster's member that started late and has not caught
  up. No member that runs holds what the cluster committed, and neither can tell
  the cluster from a new one whose third member has not started. A rule that held
  this case would keep every new cluster from electing until each of its members
  runs (Alternatives considered). A member that has not caught up shows it in
  `cluster.append_position`, below its leader's, and a member started late beside
  a running cluster catches up within its first seconds.
- A member whose leader's log no longer begins at position 0 withholds until work
  item 365 gives it a start from a peer's snapshot.
- The terms file holds one more field, which a reader of version 5 ignores, so a
  directory either version wrote opens under the other.
- Revisit when a start from a peer's snapshot is built: a withholding member can
  reach its floor from the snapshot's position. Revisit too when membership goes
  through the log, which may make the floor one member's view of a record all the
  members keep.

## References

- Work item 286 on the Brigade Board; work item 363, feature 3 of the beta slice
  (ADR 0138); work items 365, 364 and 406.
- `spec/cluster.md`, "A member that lost its log", "A member's start", "Counters";
  `spec/durable-log.md`, "Terms" and "The driver".
- [ADR 0104](0104-have-the-consensus-module-elect-its-leader-and-take-the-appointed-leader-out.md),
  the rule and the guard it lost.
- [ADR 0108](0108-list-the-recordings-of-a-members-log-in-its-log-directory-and-start-a-member-on-the-log-it-kept.md),
  a member's start on the log it kept.
- [ADR 0134](0134-mark-a-standby-in-the-members-string-and-change-one-mark-at-a-time.md),
  the standby.
- [ADR 0144](0144-keep-a-recordings-synced-position-in-its-time-index-and-cut-a-recording-a-crash-left-live-to-it.md),
  a member after a power loss.
- Aeron's `Election`:
  <https://github.com/aeron-io/aeron/blob/master/aeron-cluster/src/main/java/io/aeron/cluster/Election.java>;
  Aeron's Cluster Backup: <https://aeron.io/docs/aeron-cluster/cluster-backup/>.
