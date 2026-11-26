# 0174. Have a member that holds nothing vote only once every voter has reported

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

A member that holds nothing withholds its vote and its candidacy while a voter of its members string other than itself has not reported in this election. Holding nothing means its log is empty and its record of terms holds no candidate term, as a lost log directory leaves it. It also withholds, as before, while a report it hears holds a log. And a member that holds nothing votes by ADR 0145's guarded case, whether it withholds or not. That case needs every voter's report, a candidate's log at least each, and every voter other than itself and the candidate naming a term below the request's. So such a member, in a cluster of empty logs, stands and votes only once it has heard every voter, and then votes for no other candidate in a term it named. When it stands, it takes the term above each term a member named in this election, where that is above candidate term + 1, and stores it before it asks for a vote. So the vote it gives itself keeps the same term condition, and the candidate term it stores is above every term it could have voted in before it lost its log. And a member that withholds when its nomination delay ends, having read a report that holds a log during the delay, goes back to canvass and stores nothing. It is then no candidate while it withholds, in the delay as in canvass. A member whose log is empty and that holds a candidate term either kept its record of terms, and with it the vote it cast in that term, or stood while it held nothing, above each term it could have voted in. Either way the rule does not hold it back. `withholds` gains the inputs `all_reported` and `candidate_term`, and its vectors pin both. `candidacy_term` gives the term a member stands in, and its `candidacy-term` vectors pin it. The gauge `cluster.voters_unheard` counts the voters such a member waits for, and `cavalry doctor`'s `cluster-no-leader` says how many there are and to start them. The page is amended within version 3, with no version moved, since no message or layout changes.

## Alternatives considered

- **The term condition over the voters heard, keeping the startup canvass's escape.** A member would vote after the 60 s startup canvass timeout with the voters it has heard. It lost because it narrows the window and does not close it. The first candidate's word can be as late as the votes were, and a one-way silence of 60 s from it leaves the second vote open. That is the kind of rule work item 476's brief rules out.
- **Record who led each term, and refuse a second leader of it.** It lost because the terms file's header has no room for a leader id, and the rule would stop the cluster at the same point this one waits at.
- **Keep the case open and say so.** It lost because the loss is a committed entry, invariant 1, and the cost of closing it falls only on a new cluster's start, where nothing has been committed that waiting could lose.
- **Let a member that holds nothing stand in candidate term + 1, and rest its own vote on the closed ballot.** The manager's read of this branch found that such a member stood in term 0 whatever its voters named, so the vote it gave itself was outside the term condition. In a new cluster of members 0, 1 and 2:
  1. Members 0 and 1 both stand in term 0, and member 2 votes for member 1.
  2. Each of 0 and 1 reads the other's vote against, loses its ballot, and canvasses again naming term 0, its log empty.
  3. Member 2 loses its log directory and starts again. It hears both, so every voter has reported and no log is heard.
  4. Member 2 stands in term 0: a second vote of its own in that term.

  No second leader follows, since member 1 had left its ballot: a candidate writes no canvass position while its ballot is open, and had it won, its report would hold a log. This alternative lost because the safety of that path would rest on what a candidate's states write and when, which no vector pins. A later change that had a candidate send a canvass position during its ballot would open it with every gate green. With the term above each term named, it rests on the term condition the page already states, and member 2 stands in term 1. And the closed ballot does not cover every schedule: the one in "The schedule the term above each term named is needed for" leads one term twice without it.

## Why a report written before the member's present run cannot count as heard

A report a voter wrote before the member's present run began can reach that run late. A sender holds back the frames its receiver's image is a receiver window behind for, and sends them to the next image (`spec/udp.md`, "The sender"). Such a report, read as "has reported", would let the member take every voter as heard. If the stale report names a term below the request's, the member would vote a second time. The rule needs no binding of a report to the present run, because a stream is read in the order written:

1. The member's earlier run voted for candidate C in term t, so it read C's request vote for t.
2. Every frame C wrote on its stream to the member before that request was sent before it, since a stream is sent in order. Of those, a datagram still on its way at the member's start is dropped below the new image (`spec/udp.md`, "The receiver", steps 1 and 2; `spec/cluster.md`, "What the second case rests on").
3. A frame held back to the new image is one C had not yet sent, so one written after the request, after C stored t. It names t or a later term.
4. The member counts C as heard only from a report it reads in its present election. Every such report names t or later, so the term condition refuses another candidate in t.

A voter other than C that names a lower term in a stale report names what it held then. The case asks only that no voter but the candidate has named the request's term, and C has. The lost-log search draws this. It holds a consensus stream from one member to another, as a sender holds its frames, and delivers it in the order written to the receiver's next run (`Cluster::hold_stream`). Its default seeds draw at least one stream held across a wipe of its receiver, and the run asserts it.

## A vote held for the member's earlier run

A voter's sender can hold a vote it wrote to the member's earlier run, for that run's candidacy in a term t, and send it to the new run. A ballot keeps a vote by its candidate id and term alone, and a vote names no run. The vote is counted by a ballot of the new run only if the earlier run never led t:
- **If the earlier run led t**, it read a vote for t from another voter W, since a quorum of two or more voters needs one. W stored t before it wrote that vote. The earlier run read it, so the new run's image of W's stream joins past it, and every frame of W the new run reads names t or a later term. The new run stands only once every voter has reported, W among them, and then above every term named, so above t. The held vote matches no ballot of it, however the held frames of the voter that holds it are cut into parts.
- **If the earlier run never led t**, every vote counted for this member id in t was given to it. The member's own votes in t are for itself, since a run that voted for another candidate in t stores t and stands above it. A second leader of t would need a quorum that shares a voter with this one, a voter that voted twice in t, so it needs a second member that lost its log.

So the case is held for a cluster of two or more voters, while one member at a time loses its log. A vote that named the run it answers, as a floor answer names the session of the run it answered (ADR 0163), would make this argument unneeded. That is a simplification for a version of its own, not an open case (work item 500).

## The schedule the term above each term named is needed for

Without it, one term is led by two runs of one member. In a new cluster of members 0, 1 and 2:
1. Member 0 stands in term 0, and member 1 votes for it. Member 2 reads nothing of member 0, and member 1's stream to it is held once it has named −1.
2. Member 1's wait for its candidate ends, and it canvasses again: its log empty, naming term 0, on a stream to member 0 that is held. Member 0 leads term 0, and member 1 takes it.
3. Member 0 loses its log directory. Its new run reads member 1's held canvass positions alone, as frames past one receiver window are sent in parts: an empty log, term 0. Member 2 reports an empty log and names −1.
4. In candidate term + 1 the new run stands in term 0. Member 2 votes for it, and member 0 leads term 0 a second time.

It stands in term 1 instead. `a_member_that_holds_nothing_stands_above_the_term_a_stale_report_names` holds the schedule in the in-memory cluster. With the term above each term named taken out, the check of the terms fails: "term 0 led by member 0 in its run 0 and by member 0 in its run 1". No entry is lost in that schedule, since a leader appends its term's first entry only once a quorum has reported at its position. A member whose report the earlier run read then names a log in every frame of it the new run reads, and the new run withholds. So the term above each term named keeps one leader to a term, which the page's safety rests on, where the logs alone would not show the break.

## What the in-memory check sees

`Safety::check` runs beside every step of the searches and the cluster tests. Before this change it asserted that no two members lead one term at one moment, and what was committed. It saw no second vote, and no term led by two runs of one member id across a wipe. Now it records every vote as it is written (`Cluster::votes_cast`): a vote for a candidate, and a candidate's vote for itself, which its request vote carries. Across the whole run and across wipes, it fails:
- on one term led by two runs, of one member or of two;
- on a member id's second vote in a term, for another run, while the run it first voted for can still lead that term, in that term's ballot or leading it.

A second vote whose first candidate can no longer lead the term is not a fault: that candidate was wiped, started again, or left its ballot without leading. The check passes it, since what safety needs is one leader of a term, and not one vote. What it does not see:
- a vote a ballot counted that did not answer that ballot's request, unless it elects a second leader of a term;
- the transport itself, beyond what the in-memory cluster draws of it: held frames, held streams, loss and delay.

The test `a_quorum_that_lost_its_logs_leads_a_term_a_second_time_and_the_check_fails` shows the term check failing, on a cluster whose three members all lost their logs.

## Evidence

- **Seed 804 of the lost-log search passes, and with this rule taken out seeds 762, 804, 937 and 2228 of 1 to 3,000 fail.** Each fails on a second vote of one member id in one term while the run it first voted for could still lead it, the check described above. Seeds 804 and 937 were the first to fail before that check saw votes, on two members leading term 0, and both are among `just test`'s default seeds. The draws include held streams.
- **The searches under the rule**, one seed a run in a release build, with the checks of votes and terms:
  - lost-log 1 to 3,000: none fails the check of every step;
  - faulted-truncation 1 to 300: none fails;
  - timer 1 to 200: none fails, with 0 members whose election had not closed and 0 seeds with no settled quorum.

  The timer search settles every seed with every member running, so elections end with every member up.
- **The term above each term named alone taken out, and the check at the nomination delay's end alone taken out:** no seed of 1 to 3,000 fails either way. The search holds one stream at a time, and the schedule the term above each term named is needed for holds three. The evidence for each is its test, below.
- **The regression seeds of the other rules, taken again:**
  - ADR 0145's term condition out: the first six to fail are 408, 780, 1765, 1961, 2459 and 2829. 408 and 780 fail on one term led by two members, which the check of terms now sees.
  - ADR 0148's rule out: the first six to fail of 1 to 1,500 are 226, 411, 516, 609, 697 and 1145.

  The default seeds are now lost-log 1, 804, 937, 1765, 1961 and faulted-truncation 1, 226, 411, 609.
- **On one core**, in `tests/vote_in_one_term.rs`:
  - a member of a new cluster that lost its log withholds while the candidate it may have voted for is unheard;
  - it refuses a second vote in that term once that candidate names it;
  - it votes in the next term;
  - one that has heard every voter votes;
  - one that voted holds a candidate term, and votes in a later term without the report of the leader that stopped.
- **The term a member that holds nothing stands in**, in `tests/vote_in_one_term.rs` and `tests/lost_log_directory.rs`:
  - the manager's schedule on one core: the member stands in term 1, where with candidate term + 1 it stored term 0, a second vote of its own in the term it had voted in for member 1;
  - a report that holds a log, read during the nomination delay, sends the member back to canvass with nothing stored; with the check at the delay's end taken out, it stood;
  - the schedule above in the in-memory cluster.
- **A new cluster's first leader that stops unheard**, in `tests/elected_replication.rs`. When both other members voted in its election and its new leadership term reached neither, the two elect a later term within 5 s, about 2 s at seeds 1 to 5, and commit. When one of them holds nothing and has not heard the leader in its election, it waits: no leader in 120 s and `cluster.voters_unheard` 1, where with this rule taken out the two elect within 2 s. The three elect once the leader runs again.
- **ADR 0145's accepted case, held:** `two_members_that_lost_their_logs_while_the_third_is_stopped_wait_for_it`. The two elect nothing in 120 s, then the third leads and the two hold everything committed.
- **The cost:** a new cluster of three in memory, every member up, time to its first settled leader over seeds 1 to 1,000. Before and after, identical: minimum 14 ms, median 121 ms, p99 433 ms, maximum 757 ms (seed 868). With every member up the rule costs nothing. The whole cost is a member that never starts, and then the others wait with no bound.
- **Eleven tests that held the old start**, the startup canvass timeout electing two of three among them, now hold the new one or keep their point with every voter heard.
- **The twenty runs** of the election's driver tests on both runners: TWENTY_RUNS.

## Consequences

- A new cluster elects its first leader only once each of its members has run. Before its first vote, every member of a new cluster holds nothing. Two that run while the third has never started wait for it, with no bound. `cavalry doctor`'s `cluster-no-leader` says so, and the how-to says to start every member. In Aeron, a cluster elects at its start with a quorum after the startup canvass timeout. That rests on a member's state being kept. This page keeps the timeout for a member whose log is not empty.
- A new cluster whose first leader stops before its new leadership term reaches the other two still elects again when both voted in its election. Each holds a candidate term, so neither is held back, and each heard every voter in that election, so the two elect a later term within seconds, as before. A member that took the leader holds a vote floor and waits for it, as under ADR 0161. An agent review of the first form of this rule found that case: the form held back every member whose log was empty, and two running members then waited for the stopped leader with no bound. Narrowing the rule to a member that holds no candidate term ends it. The double vote needs a member that forgot its vote, and only a member with no candidate term can have.
- What the narrowed rule still costs a new cluster whose first leader stops early: a member that holds nothing and has not heard that leader in its present election waits for it with no bound, where before it elected with the other within seconds. The leader's frames to it were lost on the way, or it started again after the leader stopped. It cannot tell that it never voted from a vote a lost log directory took with it. A first leader that stops after its word arrived and before its followers joined its live log was already waited for, by the vote floor (ADR 0161). Starting the leader again ends the wait, and `cavalry doctor`'s `cluster-no-leader` says to.
- The waits this rule adds have no bound in time, as a cluster without a quorum's has none, and nothing grows while they last. A waiting member canvasses every election status interval, with nothing queued and no table filling, so invariant 3 holds. What it gives up is availability, which invariant 1 comes before: a vote that could lose a committed entry is not cast to end a wait. `cluster.voters_unheard` and `cluster-no-leader` say what ends it.
- Elections still end once every member that has run runs again and reaches the others. A member that holds nothing then hears every voter, and the guarded case lets it vote. The timer search settles every seed with every member up; the tests above end each wait by starting the member waited for.
- ADR 0145's two accepted cases are now held. Each is two members of three whose logs are empty while the third, which holds the log, is stopped: both lost their logs, or one lost it and the other never caught up. The two used to elect between them, as a new cluster does, and start an empty history. Now each waits for every voter's report, and once the third runs its report holds a log, so they withhold and it leads. If the third never comes back, they wait for it with no bound, where before they lost the cluster's history.
- Revisit if a member gains a way to know it never ran before: an identity the cluster records (work item 470), for instance.

## References

- Work item 476; work item 467's search and its seed 804; work item 470 (its rule does not join this one, since a new cluster's first ballot has no records); work item 500 (a vote that names the run it answers).
- ADR 0145, ADR 0161, ADR 0163.
- `spec/cluster.md`, "A member that lost its log", "What the second case rests on", and the version 3 paragraph.
- `docs/how-to/run-a-cluster.md`, step 3 and "When it does not work".
