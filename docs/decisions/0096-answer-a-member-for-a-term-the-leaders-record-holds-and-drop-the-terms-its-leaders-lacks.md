# 0096. Answer a member for a term the leader's record holds, and drop from a member's record the terms its leader's lacks

- **Status:** Accepted
- **Date:** 2026-11-12
- **Invariants affected:** 1, 3, 4, 6
- **Supersedes:** none. Changes `spec/cluster.md`: the first field of "New leadership
  term, template 53"; in "Roles and the election", the leader's answer and the
  reader's rule in "Messages during an election", and the answer of a leader whose
  election is closed; and three rows of "Counters": a new one for
  `cluster.terms_dropped`; what `cluster.ballots_lost` counts for a message that
  is not for the reader's log term; and what `cluster.log_truncations` counts,
  which is also the cut to the base of the lowest term a drop takes out of the
  record, besides the cut to the next term base log position of ADR 0083; and, in
  the same paragraph of "Messages during an election", one clause of ADR 0095's
  truncation, which the review of pull
  request 246 noted: `truncate recording` is sent for each recording that begins at
  or below the position and ends above it, two that overlap after a catch-up, where
  the page said "the recording". Changes one sentence of `spec/durable-log.md`,
  "The driver": what the module writes state 3 over. No message's layout changes and
  no file's format changes; both pages stay at their versions, since no member over
  a driver elects yet (ADR 0081). The appointed leader answers by the same rule, and
  over a driver every member starts on an empty log directory, so there the term
  answered is the term reported, as before. Adds the `answered-term` vectors and
  five `new-leadership-term` vectors. Amends ADR 0081 in one clause: what
  `cluster.ballots_lost` counts on a new leadership term that is not for the
  reader's log term. Amends ADR 0083 in one clause: what
  `cluster.log_truncations` counts, which now includes the cut a drop makes first.

## Context

A leader answered a member's report with a new leadership term "for the reported
log leadership term id", and named as next the lowest term its own record holds
above that one. It did not check that its own record holds the term the member
reported (card 267).

**A term can be recorded by fewer than a quorum.** A member X wins a ballot of term
R with a log that ends at b, and records R at b. Its new leadership term reaches
one member M, which records R at b too, and no other. X never has a quorum that
recorded R, so it appends nothing: R has no byte anywhere. A later leader Y is
elected by members that never recorded R. In Y's log an earlier term runs on past
b, and Y's record has no R.

**What followed.** M reported a log of term R that ends at b. Y named its own term
as the next, with a base above b. M replicated "the rest of its own term R" from Y.
The bytes were Y's bytes of the earlier term, so no byte was wrong, but M's record
kept R at b, inside that term of Y's. When M later led, it told a member whose log
was of the earlier term and ended above b that R was next, at b. That member had to
truncate to b. If it knew a commit position above b, the guard of ADR 0083 refused,
at every election, and the member never joined. With two of five members down, the
third one stuck leaves no quorum. A test of five members in memory made it: 295
truncations refused in 30 s, and a leader whose election never closed.

**The trap in the plain fix.** A reader acts on a new leadership term only when the
message's log leadership term id is its own log term. That equality also protects
it from a late copy of an answer to an older report. A reader that instead dropped
terms whenever a message named a term below its own would cut, on a late copy from
a leader it has since joined, bytes that leader has committed with it.

## Decision

**A leader answers for a term its own record holds.** The log leadership term id of
a new leadership term is the highest term in the leader's record at or below the
term the member reported, and −1 when the record holds none. The next fields are as
before, the lowest term in the leader's record above the reported term, which is
also the lowest above the term answered, since the record holds none between.

**A reader that is told a term below its own drops the terms above it, when the
message answers the log it holds.** The message says that the leader's record holds
none of the reader's terms above the one named. The reader acts on it when the
message's next leadership term id is above the reader's log term:

1. Its append position above the base of the lowest of those terms, it truncates
   its log to that base, under the rule and the counters of a truncation (ADR
   0083): a reader that knows a commit position above that base keeps its log and
   its record and fails its election.
2. It takes every term above the one named out of its record of terms, durably,
   counted in `cluster.terms_dropped`. They are named by term and not by base: the
   lowest of them is based where the log now ends, so a truncation "above that
   position" leaves it, and a term at or below the one named can share that base.
3. It reads the message again with the log it then holds, whose term is at or
   below the one named. With the term named, it replicates or joins as the page has
   it. With a lower one, it never recorded the term named, which fewer than a
   quorum recorded too: the message is then for a log term that is not its own, and
   it canvasses, a ballot it was in counted in `cluster.ballots_lost`, and reports
   the lower term, which the leader answers.

A message that names a term below the reader's own with a next term at or below the
reader's log term drops nothing, and the reader canvasses on, as it does for any
other term that is not its own.

**Why the next term is the condition.** The two fields together are a statement
about the leader's record that stays true for as long as that leader's term lasts:
it holds the term named, and no term between that one and the next. A reader whose
log term lies between the two holds, above the term named, only terms the leader's
record lacks. A reader whose log term is the next term or above it learns nothing
of its own terms from the next term up: they may be the leader's own, replicated
from it since. That is the case of an answer to a report the reader made before it
recorded more, and of a copy from a leader it has since joined, whose own term is
then the reader's log term or below. So the rule needs no notion of how old a
message is. The leader's term is at least the next term, so the condition also says
that the leader's term is above the reader's log term: the reader has not joined
this leader.

**Why what is dropped holds nothing committed.** The leader of term T was elected
with every entry committed before its election, and no entry of a lower term is
committed after it, since the members that voted in T count for no leader of a
lower term. A member holds bytes of a term only after it recorded the term. So a
term below T that the leader's record lacks has no committed entry, and the bytes
the reader holds at or above the base of the lowest term it drops are of the terms
it drops. The guard of ADR 0083 stays in front of the cut all the same.

**Records converge.** A member that joins a leader then holds the leader's record
up to the leader's term: what the leader lacks is dropped before the member
replicates, and what the member lacks it replicates term by term.

**Two operations of `Io`, one of the terms file.** `Io::term_at_or_below(term)`
reads the leader's record from memory. `Io::drop_terms_above(term)` deletes the
terms and returns the highest left. Over a driver it is
`TermsFile::drop_above_term`, which writes state 3 over each valid entry above the
term, from the last backwards, each write synced, as a truncation does.

**The reader reads the message twice at most.** After the drop its log term is at
or below the term named. A record that still held a term above it would be a fault
of the `Io`: the member fails its election and reads nothing again. A drop that
faults after its truncation leaves the log cut and the record with some of those
terms: the member reports its log term again, the leader answers again, and the
drop runs again, changing nothing that is already dropped.

**A departure from Aeron.** Aeron's leader names the term the member reported,
whatever its own record holds, and looks up in its record only the term after it;
a reader whose log term is not the one named canvasses, and drops nothing. So a
term that fewer than a quorum recorded stays in a member's record there.

## Alternatives considered

- **Drop whenever a message names a term below the reader's own.** Lost
  (invariant 1). It is the trap above: a late copy from a leader the reader has
  joined would cut what that leader committed with it.
- **Act when the leader's term is above the reader's log term, and no more.** Lost.
  It is safe, by the argument above, but an answer to an older report from the
  same leader, read between two terms the reader replicates from it, would cut
  what the reader has just replicated and ask for it again.
- **Carry the term the reader reported in the message.** Lost. It needs a field,
  which is a change of layout, and the next leadership term id already tells the
  two apart.
- **Have the leader refuse to answer a term it lacks.** Lost. The member would
  canvass for as long as that leader leads.
- **Leave the record and fix what a leader names as next when it has such a term
  itself.** Lost. The term would stay in records for good, and every rule that
  reads a record would need the exception.
- **Cut the record by base, with the truncation.** Not possible: the lowest term
  dropped is based where the log is cut.

## Evidence

- `spec/vectors/cluster.json`, which `cavalry-proto`'s conformance test runs
  against the arithmetic. Five `answered-term` vectors: the reported term when the
  record holds it; the term below one the record lacks; −1 when the record holds
  none at or below it, and for a member that reported no term; the leader's own.
  Five `new-leadership-term` vectors: a term the leader lacks is dropped, with no
  cut when the reader holds nothing above its base and with a cut when it does, in
  canvass and in a ballot; an answer to an older report canvasses; a leader that
  holds no term of the reader's drops them all.
- `crates/cavalry-raft/tests/dropped_terms.rs`, one core driven by hand, seven
  tests. A member whose record is (0, 0), (1, 64), told term 0 with term 3 next at
  128: with its log ending at 64 it cuts nothing, drops term 1, counted, and goes to
  follower log replication under that leader; with its log ending at 96 it truncates
  to 64 first, counted as a truncation, and then drops; knowing a commit position
  of 96 it refuses, counted, fails its election, and makes no call. A message whose
  next term is 1, and a late copy from the leader of term 1, change nothing. A
  leader whose record is (0, 0), (2, 96) answers a member of term 1 for term 0, with
  its own term 2 at 96 as the next; with term 1 in its record it answers for term 1.
  A member whose record is (0, 0), (2, 64), told term 1 with term 3 next, never
  recorded the term named: it drops term 2, once, its log term is 0, and it
  canvasses with no election failed; as a candidate of term 3 it drops the term and
  its ballot is lost, both counted.
- `crates/cavalry-raft/tests/election.rs`, in memory, with the check after every
  step that no committed entry is lost or replaced.
  - The history of the card, five members. The member that recorded term 1 with
    one other drops it when the next leader answers, cuts nothing, and replicates
    the tail as the rest of term 0: its record is then its leader's, byte for byte
    its log too. When it leads, the member that knows a commit position inside the
    tail replicates the entry it lacks and refuses no truncation; the member that
    led term 1 drops it; the three records are one; the election closes and an
    entry is committed by the three. With the leader answering for the reported
    term, as before, the test fails where the first member should have dropped the
    term.
  - Three members at eight seeds, every consensus message up to two seconds late
    and out of order, three leaders stopped and run again in turn: the check holds
    at every step, no member refuses a truncation, and the three settle on one
    log. The rule runs under those messages: at five of the eight seeds members
    drop terms, twelve drops in all, three of them after a cut, and the test fails
    if none does.
- `crates/cavalry-log`, `terms_above_a_term_are_dropped_by_term_whatever_their_base`:
  of two terms at one base, `drop_above_term` deletes the higher and keeps the
  lower, and deletes the term above them at another base; a term appended after
  reads back from a file opened again; and dropping above −1 empties the record.
- Aeron's `Election`, read on 2026-11-12: `onCanvassPosition` passes the member's
  log leadership term id on to `publishNewLeadershipTerm`, which sends it as it is
  and finds the next term with `findTermEntry(logLeadershipTermId + 1)`;
  `onNewLeadershipTerm` moves a reader whose log leadership term id differs to
  canvass.
- Not shown: the argument that a term a later leader lacks holds nothing committed
  is made above and rests on the election's own rules (ADR 0058, ADR 0081); the
  in-memory check would catch a counter-example in the histories the tests make,
  and makes none.

## Consequences

- A member that recorded a term no quorum recorded joins the next leader with one
  more step and no failed election, where it could strand another member later.
- `cluster.terms_dropped` says when a member's log term went down. Its `doctor`
  text says that nothing was lost. It also says that a member whose cut would go
  below the commit position it knows drops nothing: `cluster.truncations_refused`
  and `cluster.elections_failed` rise on it at each election while
  `cluster.terms_dropped` stays where it was, and the text names what to read then.
- A member's log term can fall. Every rule that compares log terms reads it at the
  time: a member that dropped a term reports the lower one from then on, and votes
  as a member of no term it no longer holds, as after a truncation.
- Bytes replicated under a term no quorum recorded, by a member of a version before
  this one, are cut and replicated again when a leader's record lacks the term,
  unless the member knows a commit position among them: then it fails its election
  as before. No such version has run over a driver.
- Revisit with the fault-injection tests of ADR 0077, over drivers: a history that
  makes a term no quorum recorded there, and a late answer.

## References

- `spec/cluster.md`, "New leadership term, template 53"; "Messages during an
  election"; "Counters". `spec/durable-log.md`, "The driver".
- [ADR 0058](0058-specify-the-cluster-protocol-from-aerons-codecs.md), the election
  and its departures from Aeron.
- [ADR 0081](0081-build-the-election-beside-the-appointed-leader-and-move-the-module-to-it-once.md),
  what `cluster.ballots_lost` counted.
- Aeron's `Election`:
  <https://github.com/aeron-io/aeron/blob/master/aeron-cluster/src/main/java/io/aeron/cluster/Election.java>
- [ADR 0083](0083-truncate-to-the-next-terms-base-and-never-below-the-commit-position.md),
  the truncation and its guard.
- [ADR 0086](0086-record-a-term-when-its-replication-begins-and-check-each-replays-image-against-the-position-last-sent.md),
  whose Consequences name this card.
- Card 267, with the analysis and the history.
