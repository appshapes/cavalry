# 0134. Mark a standby in the members string, and change one mark at a time

- **Status:** Accepted
- **Date:** 2026-11-15
- **Invariants affected:** 1, 3, 4, 5, 6
- **Supersedes:** none. Decides the sixth change of work item 230, the standby of
  ADR 0052's feature 6. Changes `spec/cluster.md`: the grammar of the members string
  in "Members and streams", a seventh field `standby`; two paragraphs of "Roles and
  the election", "Standbys" and "Changing a standby's mark", and a sentence of its
  first paragraph; three rows of "Counters", which `counters.toml` gains as ids 176
  to 178; the members' row of "Values"; a check of a members string in "What a
  reader rejects", `members-voters`, and its grammar; and the `members` vectors,
  twelve more. The page's version stays 0. No message's layout changes, no file's
  format and no function of the C ABI.

## Context

ADR 0052 put the standby in the open core and said what it costs: "a standby is a
member that does not vote, which costs the election a flag". Work item 230 asks for
"a member that follows the log and takes snapshots without a vote, from the members
string, promoted by configuration", and is done when "a standby promoted leads".
ADR 0052 also left dynamic membership out of the slice: no member is added to or
removed from a running cluster.

A cluster is a members string, the same on every member (`spec/cluster.md`,
"Members and streams"). Nothing checks that it is the same: no message carries it,
and the consensus messages are Aeron's layouts field for field (ADR 0058), with no
field for it. A member takes its string when it starts, so a change of the string
reaches the members one start at a time, and for that time two strings run side by
side.

The election counts members in six places: a quorum, the quorum position, a
leader's active members, a canvassing member's unanimous and quorum candidate, the
votes of a ballot, and the members a candidate has heard and waits for (ADR 0077).

The questions: where the mark is; what a standby does with a ballot; what a voter
does with a member its string marks; and how a mark is changed without losing a
committed entry.

## Decision

A seventh field of a member in the members string, the word `standby`, marks the
member a standby. A string names at least one voter.

A standby is a follower in everything but two things: it never goes from canvass to
nominate, and it answers a request vote with nothing, its candidate term unchanged.
A member counts the voters of its own string in each of the six places, and a
standby in none. A member reads the messages of every member its string names as
it did before, a standby's as a voter's, and sends to every member: it votes, on
the logs compared, for a candidate its own string marks a standby, and follows a
leader its own string marks one.

A mark is changed by starting the members again with a new string, under three
rules that `spec/cluster.md` states and an operator keeps
(`docs/how-to/promote-a-standby.md`): one mark at a time; a change complete, every
running member started with the new string, all of them following one leader with
their elections closed, and a quorum of the new string's voters among them,
before the next begins; and no member started with an older string.

Two gauges say which string a member runs: `cluster.voters`, the voters as one bit
each in the rising order of the ids, and `cluster.standby`. `cavalry doctor`
names the first by the members' ids in every member's note (`cluster-member`), so
that an operator holds every member's to one set. It compares it with the voters
of the string its host is configured with, and reports a member that runs another
(`cluster-voters`). A request vote that reaches a standby is counted,
`cluster.standby_vote_requests`, and reported.

## Alternatives considered

- **A mark on the id, such as `3s`.** Lost. An id is an integer in every reader of
  the string, the five clients among them, and a suffix makes it a new grammar for
  each. A member with one field more is refused whole by a reader of the version
  before, `members-grammar`, which is the right failure: that reader would count
  the standby's vote.
- **A configuration key beside the string, `CAVALRY_CLUSTER_STANDBY=1`, on the
  standby alone.** Lost. The voters must know which members to count, so the mark
  is every member's to hold, and a key needs a reason why its default cannot be
  right (invariant 5). The string is already what every member is given.
- **A standby votes against, or votes for.** Lost. A vote against ends the
  candidate's ballot (ADR 0105), so one standby would stop every election. A vote
  for would be counted by a candidate whose string gives the standby a vote, and
  then the standby is a voter.
- **A voter answers nothing to a candidate that its own string marks a standby.**
  Lost. It is safe, and it stops a promotion until every voter has the new string:
  the promoted member could win nothing while one voter still ran the old one. A
  vote is a promise about the voter's own log and term, whoever asks, and the
  candidate counts by its own string. Raft's servers vote for a candidate outside
  their configuration for the same reason. It is also one rule fewer: the mark
  changes what a member counts and what a standby does, and nothing a member reads.
- **Replace a voter by a standby in one change.** Lost: it is not safe. The voters
  0, 1 and 2 and the voters 0, 1 and 3 have quorums with no member in common, 1
  with 2 and 0 with 3. A test runs it (Evidence).
- **Two changes with no election between them.** Lost: it is not safe either, and
  this one was found by a counterexample and not known beforehand. Each string is
  one mark from the one before it, and the first and the third are two marks
  apart: a quorum of the third can hold none of what the first committed, when no
  leader of the second has carried it to a quorum of the second's voters. An
  election closed in the second string is that: its leader's term is recorded at
  its log position by a quorum of that string's voters (state 8).
- **A change complete when a leader that was started with the new string has
  closed its election.** Lost: its safety would rest on clocks. A leader that is
  parted from the others says that it leads until its own clock has counted the
  leader heartbeat timeout, and a process that was stopped for a while, or a host
  that was paused, counts late. In that time a member that still runs the string
  before can lead a later term and commit by that string, on a quorum that is no
  quorum of the new one. When a quorum of the new string's voters follows the one
  leader, no such term exists: the quorum that elected its leader shares a voter
  with them, and a member that voted in a later term follows no leader of an
  earlier one.
- **A change complete when every member that runs follows one leader, however few
  run.** Lost, after it was the rule here and a review of the pull request found
  its history. Five voters and a standby that is being promoted: three members,
  started with the new string, follow a leader among them. The three others still
  run the string before, are parted from them and are a quorum of its five
  voters: they elect a leader of a later term and commit, while the first three
  stand still, and then they stop. Every member that runs was started with the
  new string and follows one leader, so the rule read complete; and the next
  change, one mark on, had those three, a quorum of its voters, elect a leader
  without the entry. The members that do not run were the hole: the rule's own
  reason against one member that says it leads came back through them. A quorum
  of the new string's voters among the members that follow the leader closes it
  with no clock, since a quorum of the string before shares a voter with it.
- **A member checks the rules.** Lost for this version. A member does not know
  another member's string. To carry the string, or a hash of its voters, in a
  consensus message is a change of a layout that is Aeron's, a version of the page
  and a rolling upgrade's work (work item 231). To keep the last string in the log
  directory would refuse a member that was away for two changes, which is allowed.
  What a member can see, it counts: a request vote that reaches a standby. And
  `cavalry doctor` holds each member to the string its host is configured with,
  which is where an operator makes the change. The string was unchecked before
  this ADR, when every member had to hold the same one.
- **The change as an entry of the log, as Raft and Aeron's deprecated dynamic
  membership do it.** Lost: ADR 0052 left dynamic membership out of the slice. It
  is the way to make the three rules the leader's and not the operator's, and the
  Revisit names it.
- **A value of `cluster.role` for a standby.** Lost. A standby is a follower: every
  reader of the gauge that asks whether a member leads or follows is right about
  it as it is. A gauge of its own says the one thing that differs.
- **`cluster.voters` as a count.** Lost. Two strings one mark apart differ in their
  count, and two strings two marks apart, the ones that lose an entry, can have the
  same count. The bits tell them apart.

## Evidence

- `crates/cavalry-raft/tests/standby.rs`, eighteen tests, on an Apple M5 Pro laptop,
  macOS 26.6.1. Seven drive one core by hand. A standby with both voters' reports
  stays in canvass past the startup canvass timeout, 60 s, where a voter nominates
  itself on its next cycle. It answers two request votes with no vote and no
  stored term, and counts two. Its election closed, it holds an election on a
  request vote for a higher term and none for its own. A candidate with a standby
  is a unanimous candidate on the two other voters' reports, asks those two alone,
  and leads at once on their votes with the standby heard and a vote against from
  it read. A voter votes for a candidate its string marks a standby and follows it.
- The same file, clusters in memory, with the check of invariant 1 at every step:
  no committed byte lost or replaced, no commit above what a quorum of the leader's
  voters has synced, no two leaders of one term. Three voters and a standby, at
  twenty seeds: the winner's ballot took under 100 ms each time, and the standby
  closed its election behind the leader with no vote cast and none asked. A
  standby that does not run: the voters led within 1.5 s, not at 60 s. With two
  voters stopped, an entry on the leader and the standby was not committed, the
  leader lost its quorum once, and nobody led for 30 s. One voter and two standbys:
  the voter led from init. Strings one mark apart: the standby counted the request
  votes that reached it and cast none, and nothing committed was lost.
- A standby promoted leads, at five seeds: two voters and a standby commit a
  hundred entries, each member is started with the string that gives the standby
  a vote, the leader last, and then one voter stops and the other starts again;
  the promoted member leads and commits, and its log holds the hundred entries. A
  lost voter replaced in two changes, at five seeds: the promoted member leads,
  with what each of the three strings committed.
- The counterexample, `two_marks_changed_at_once_elect_a_leader_that_lacks_a_committed_entry`:
  five voters and a standby; members 0, 2 and 3 elect a leader and commit an entry,
  parted from members 1, 4 and 5; those three are started with a string two marks
  away, member 2 a standby and member 5 a voter. One of them leads, in the first
  leader's term, without the entry. With one mark changed, the same three hold two
  votes of four and elect nobody for 70 s, and when the parting ends every member
  holds the entry.
- The quorum in the second rule,
  `a_change_is_not_complete_on_fewer_members_than_a_quorum_of_its_voters`: five
  voters and a standby that is being promoted, members 3, 4 and 5 started with
  the new string, a member of them leading. Members 0, 1 and 2, parted from them
  while their processes stand still, elect a leader of a later term by the string
  before and commit an entry, and then stop. The three that run each say that
  their election is closed behind the one leader among them, with six voters;
  started with the next string, member 0 a standby, they elect a leader without
  the entry. Its control: with member 1 started with the new string as well, four
  of its six voters follow one leader, members 0 and 2 elect nobody for 62 s, and
  the leader after the next change holds what the four committed.
- `crates/cavalry-raft/tests/standby_drivers.rs`, over three drivers on the
  loopback: two voters and a standby with a service beside each; the standby
  records the log, takes the snapshot its leader asks for, with the bytes the
  voters' hold, and commits nothing by itself; it is promoted, by three starts
  with the new string; the promoted member leads, and the three services hold
  every entry.
- `spec/vectors/cluster.json`, 270 vectors, twelve of them new: four strings with a
  standby, four that the grammar refuses, two with no voter, and two that hold the
  checks to their order. `cavalry-proto`'s conformance test reads them.
- `crates/cavalry-control`, two tests: a standby is named in its note and told
  what elects its leader, with the quorum as `2 of 3` of four members; and a member
  that runs with other voters than its host's string is reported with both sets by
  their ids, and one that runs with the same, or holds no such gauge, is not. Each
  member's note ends with the voters it runs with, by their ids; a member whose
  host holds an older string has no finding and names the older voters.

## Consequences

- A cluster can hold a member that is ready to take a vote: it has the log, the
  snapshots and the sessions, and it costs no election a delay.
- A cluster of three voters that lost one can be given its third voter back
  without a stop of the whole cluster, in two changes, where it could only be
  started again as a new cluster before.
- The safety of a change rests on three rules that an operator keeps. A string two
  marks from another member's, or a second change begun early, can lose a
  committed entry, and no member refuses it. A member that the operator cannot
  reach is one that may run: a change counts a member out only when its process
  has ended, which the operator makes sure of by the host's power or its instance
  when the host does not answer. A lost voter that still ran through both changes
  of its replacement, parted from the others, would hold a string two marks from
  theirs and never be started with another; the how-to's replacement of a lost
  voter begins with that step. `cavalry doctor` shows a member that
  runs another string than its host's, and a standby counts a request vote that
  reaches it. Neither sees a host whose configured string is older and whose member
  was started with it: that member agrees with its host, and a standby is asked for
  a vote only when the older string gives one to a member that is a standby by its
  own. One such host is enough: the next change puts its member two marks from the
  others. It is seen where the notes of all members are read together, since each
  names the voters its member runs with, which is what the how-to's last step asks
  of an operator; no member and no single host sees it.
- The log publication is `fc=min`: a standby that runs slowly holds the leader's
  log back, as a follower that runs slowly does. One that stops is dropped at the
  receiver timeout.
- A member of a version before this one refuses a string with a standby, and does
  not start: a cluster marks its first standby once every member is of this
  version.
- A standby casts no vote and counts toward no commit, so a member on a new log
  directory can follow as a standby without what ADR 0104 refused: a vote it has
  forgotten, and a log it reported and no longer holds. This ADR does not decide
  that start. A log purged below a snapshot gives such a member nothing to
  replicate from position 0, and a snapshot from another member is not built (work
  item 286).
- Revisit: when a member must be added to or removed from a running cluster, for
  the change as an entry of the log, which makes the three rules the leader's;
  when a message of the page gains a field, for the voters in it, so that a member
  refuses a peer whose string is two marks from its own; when a standby should not
  hold the leader's log back, for a flow control that does not count it (ADR
  0052's `tagged`); and with work item 286, for a member that starts as a standby
  on a new log directory.

## References

- Work item 230 on the Brigade Board, "Catch a member up from a peer's recording,
  snapshot the service and follow as a standby"; work item 286, a member that lost
  its log directory; work item 231, a rolling upgrade.
- `spec/cluster.md`: "Members and streams", "Roles and the election" ("Standbys",
  "Changing a standby's mark"), "Counters", "Values", "What a reader rejects",
  "Vectors".
- `crates/cavalry-proto/src/cluster.rs` (`Member::standby`, `STANDBY_MARK`,
  `MembersError::NoVoter`); `crates/cavalry-raft/src/consensus.rs`
  (`Consensus::with_standbys`, `is_standby`, `voters`), `src/memory.rs`
  (`Cluster::with_standbys`, `restart_with`), `src/module.rs`, `tests/standby.rs`,
  `tests/standby_drivers.rs`; `crates/cavalry-control/src/doctor.rs`
  (`cluster-voters`); `counters.toml`; `docs/how-to/promote-a-standby.md`.
- [ADR 0052](0052-build-raft-on-the-durable-log-fifth.md), the slice and the
  standby in the open core;
  [ADR 0077](0077-elect-the-leader-by-vote-and-close-a-ballot-on-the-members-heard.md),
  the election and the members a candidate has heard;
  [ADR 0104](0104-have-the-consensus-module-elect-its-leader-and-take-the-appointed-leader-out.md),
  a member on a new log directory;
  [ADR 0105](0105-lose-a-ballot-at-its-first-vote-against.md), a vote against.
- Diego Ongaro, "Consensus: Bridging Theory and Practice", 2014, chapter 4, on a
  change of one server at a time; and his note of 2015 on the raft-dev list, "bug
  in single-server membership changes": two changes of one server each, begun from
  one configuration with neither committed, have quorums with no server in common.
