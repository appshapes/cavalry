# 0174. Have a member of a cluster of empty logs vote only once every voter has reported

- **Status:** Accepted
- **Date:** 2026-11-26
- **Invariants affected:** 1, 4, 6
- **Supersedes:** ADR 0145 on one point: its Decision's sentence that a member whose log is empty beside members whose logs are empty too is one of a new cluster and votes as any voter does. Its other rules stand.

## Context

ADR 0145 lets a member whose log directory was lost come back on a new one. It withholds its vote while a report it hears holds a log, and then votes in one guarded case, whose term condition keeps it from voting a second time in a term it voted in before the loss. It left one case open on purpose: a member whose log is empty beside members whose logs are empty too is taken for one of a new cluster, and votes as any voter does.

Work item 467's search found that case losing safety. In a new cluster:
1. Member 2 votes for member 1 in term 0.
2. It loses its log directory before the term's first entry.
3. It votes for member 0 in term 0.
4. Both votes arrive late, and members 0 and 1 both lead term 0.

That is lost-log search seed 804, under the full rule (work item 476).

Two leaders of one term break the log matching that Raft's safety rests on, so an entry one of them commits can be replaced (invariant 1). The guarded case would hold it, but no member withholds when no log is heard.

## Decision

A member whose log is empty withholds its vote and its candidacy while a voter of its members string other than itself has not reported in this election, as well as while a report it hears holds a log. And every member whose log is empty votes by ADR 0145's guarded case, whether it withholds or not. That case needs every voter's report, a candidate's log at least each, and every voter other than itself and the candidate naming a term below the request's. So a member of a cluster of empty logs stands and votes only once it has heard every voter, and then votes no second time in a term. `withholds` gains the input `all_reported`, and its vectors pin it. The page is amended within version 3, with no version moved, since no message or layout changes.

## Alternatives considered

- **The term condition over the voters heard, keeping the startup canvass's escape.** A member would vote after the 60 s startup canvass timeout with the voters it has heard. It lost because it narrows the window and does not close it. The first candidate's word can be as late as the votes were, and a one-way silence of 60 s from it leaves the second vote open. That is the kind of rule work item 476's brief rules out.
- **Record who led each term, and refuse a second leader of it.** It lost because the terms file's header has no room for a leader id, and the rule would stop the cluster at the same point this one waits at.
- **Keep the case open and say so.** It lost because the loss is a committed entry, invariant 1, and the cost of closing it falls only on a new cluster's start, where nothing has been committed that waiting could lose.

## Why a report written before the member's present run cannot count as heard

A report a voter wrote before the member's present run began can reach that run late. A sender holds back the frames its receiver's image is a receiver window behind for, and sends them to the next image (`spec/udp.md`, "The sender"). Such a report, read as "has reported", would let the member take every voter as heard. If the stale report names a term below the request's, the member would vote a second time. The rule needs no binding of a report to the present run, because a stream is read in the order written:

1. The member's earlier run voted for candidate C in term t, so it read C's request vote for t.
2. Every frame C wrote on its stream to the member before that request was sent before it, since a stream is sent in order. Of those, a datagram still on its way at the member's start is dropped below the new image (`spec/udp.md`, "The receiver", steps 1 and 2; `spec/cluster.md`, "What the second case rests on").
3. A frame held back to the new image is one C had not yet sent, so one written after the request, after C stored t. It names t or a later term.
4. The member counts C as heard only from a report it reads in its present election. Every such report names t or later, so the term condition refuses another candidate in t.

A voter other than C that names a lower term in a stale report names what it held then. The case asks only that no voter but the candidate has named the request's term, and C has. The lost-log search draws this. It holds a consensus stream from one member to another, as a sender holds its frames, and delivers it in the order written to the receiver's next run (`Cluster::hold_stream`). Its default seeds draw at least one stream held across a wipe of its receiver, and the run asserts it.

## Evidence

- **Seed 804 of the lost-log search passes, and seeds 804 and 937 fail with this rule taken out**: two members lead term 0. These are the first of seeds 1 to 3,000 at the search's draws, held streams among them. Both are among `just test`'s default seeds.
- **The searches under the rule**, one seed a run in a release build:
  - lost-log 1 to 3,000: none fails the check of every step;
  - faulted-truncation 1 to 300: none fails;
  - timer 1 to 200: none fails, with 0 members whose election had not closed and 0 seeds with no settled quorum.

  The timer search settles every seed with every member running, so elections end with every member up.
- **The regression seeds of the other rules, taken again:**
  - ADR 0145's term condition out: 1765, 1961 and 2459 fail;
  - ADR 0148's rule out: 226, 411, 609, 697, 1145 and 1229 fail.

  The default seeds are now lost-log 1, 804, 937, 1765, 1961 and faulted-truncation 1, 226, 411, 609.
- **On one core**, in `tests/vote_in_one_term.rs`:
  - a member of a new cluster that lost its log withholds while the candidate it may have voted for is unheard;
  - it refuses a second vote in that term once that candidate names it;
  - it votes in the next term;
  - one that has heard every voter votes.
- **ADR 0145's accepted case, held:** `two_members_that_lost_their_logs_while_the_third_is_stopped_wait_for_it`. The two elect nothing in 120 s, then the third leads and the two hold everything committed.
- **The cost:** a new cluster of three in memory, every member up, time to its first settled leader over seeds 1 to 1,000. Before and after, identical: minimum 14 ms, median 121 ms, p99 433 ms, maximum 757 ms (seed 868). With every member up the rule costs nothing. The whole cost is a member that never starts, and then the others wait with no bound.
- **Eleven tests that held the old start**, the startup canvass timeout electing two of three among them, now hold the new one or keep their point with every voter heard.
- **The twenty runs** of the election's driver tests on both runners: TWENTY_RUNS.

## Consequences

- A new cluster elects its first leader only once each of its members has run. Two that run while the third has never started wait for it, with no bound. `cavalry doctor`'s `cluster-no-leader` says so, and the how-to says to start every member. In Aeron, a cluster elects at its start with a quorum after the startup canvass timeout. That rests on a member's state being kept. This page keeps the timeout for a member whose log is not empty.
- ADR 0145's two accepted cases are now held. Each is two members of three whose logs are empty while the third, which holds the log, is stopped: both lost their logs, or one lost it and the other never caught up. The two used to elect between them, as a new cluster does, and start an empty history. Now each waits for every voter's report, and once the third runs its report holds a log, so they withhold and it leads. If the third never comes back, they wait for it with no bound, where before they lost the cluster's history.
- Revisit if a member gains a way to know it never ran before: an identity the cluster records (work item 470), for instance.

## References

- Work item 476; work item 467's search and its seed 804; work item 470 (its rule does not join this one, since a new cluster's first ballot has no records).
- ADR 0145, ADR 0161, ADR 0163.
- `spec/cluster.md`, "A member that lost its log", "What the second case rests on", and the version 3 paragraph.
- `docs/how-to/run-a-cluster.md`, step 3 and "When it does not work".
