# 0163. Clear a vote floor only on a leader's answer to the present run

- **Status:** Accepted
- **Date:** 2026-11-25
- **Invariants affected:** 1, 3, 4
- **Supersedes:** ADR 0161 on one point, what clears a vote floor: its two conditions,
  the live join and the log at or past the floor, stay, and a third is added. It
  answers ADR 0161's Revisit, "when work item 463 lands".

## Context

ADR 0145 lets a member that lost its log directory come back on a new one, with a vote
floor: the log position of the new leadership term it took its leader on. A report that
the member sent before its loss can still reach the leader and count toward a commit,
as a datagram sent before a host's loss can. ADR 0145 meant the floor to keep such a
commit safe. ADR 0161 cleared the floor only once the member had also joined its
leader's live log, and named the one case none of its holders holds (its Decision,
"None of the three"). Work item 463 carried it, in eight steps:

1. M has lost its log once. L's answer to its report of an empty log, a new leadership
   term at L's append position p0, is held back by L's sender (work item 411).
2. M follows L on a later answer. L's live sender has no receiver and stands below an
   entry E that L appends, while L's recorder, a spy on L's own driver, records E.
3. M has E from a replay of L's recording, and acknowledges it. The acknowledgement is
   held on its way.
4. M loses its log again and starts again.
5. The held frame of step 1 reaches M's new run, and M takes L with a floor of p0, below E.
6. M replicates past p0 and joins L's live log at the sender's position, below E. ADR
   0161's two conditions hold, and M clears its floor and votes.
7. The held report of step 3 reaches L, which commits E with it.
8. L stops before it has sent E, and M and the third elect a leader without E.

Work item 463 asked first whether the two transport behaviours the case rests on can
happen. The pages say both can.

- **A live sender below what its recorder holds.** While a publication is connected,
  its publisher may claim up to the consumer position plus the publication window
  (`spec/udp.md`, "The sender"), and the consumer position is the lesser of the sender
  position and the recording's. Under the log stream's `fc=min` a receiver that went away
  holds the sender limit back for the flow control receiver timeout, and the limit never
  falls. So with no live receiver the leader appends past what its sender sends, for up
  to the connection timeout, and its recorder records it. What was appended stays above
  the sender position until a receiver joins, at the sender position ("The receiver").
- **A report of an earlier run reaching the leader after the member started again.** A
  closed publication lingers for the linger timeout, sending what flow control held
  (`spec/udp.md`, "The sender"), and a member's process can end while its driver runs on
  (`docs/how-to/run-a-cluster.md`, `start-stop-timeout`). The network can also deliver
  a datagram of the earlier run late.

**What was tried first.** A rule that a member reads a peer's consensus messages from
the newest image of that peer's stream alone, ordered by the registration ids its driver
gives images, so that the leader drops the late report. An independent read found that
image order is not always run order: an earlier run's image, still lingering with a
held tail, can be made again after the present run's, when the receiving member's module
starts again on a driver that keeps running, or after a long partition. The present
run's messages are then dropped until it starts again, and the earlier run's pass. It
was never pushed.

## Decision

**The floor answer.** The floor is made fresh: the echo that ADR 0145 and ADR 0161 named
first, for the floor alone and with no layout changed.

- **The ask.** From version 3 of `spec/cluster.md`, a member that holds a vote floor and
  has taken no floor answer for the term it follows, in this run, sets bit 1 of every
  append position's `flags`, the floor flag, in whatever state it sends one. Every
  version of major 0 reserves the bit and ignores it. It asks again under each term it
  holds no answer from, so that an answer of a leader that a later term deposed, which
  the later leader's log may never reach, as in an idle cluster of five whose later
  leader cut a tail only the deposed one held, holds it only until the later leader
  answers.
- **The answer.** A leader of version 3 answers an append position with the flag with a
  floor answer, template 58, to that member alone, in whatever state it reads one, at
  most once each leader heartbeat interval for each member, counted in
  `cluster.floor_answers_sent`. It carries the leader's term, its append position, the
  two member ids, and the session id of the data frame the report came in: the session
  of the member's consensus publication to the leader. No member of an earlier version
  sets the flag, so none is sent an answer.
- **The take.** A member takes an answer that names it, from the leader it follows, for
  that leader's term, with the session id of its own consensus publication to that
  leader. It ignores any other, counted in `cluster.floor_answers_ignored`; a member that
  holds no floor ignores one uncounted, as one still on its way when its floor cleared
  is. It keeps the lowest position of the answers it took since it stored the floor, in
  this run, in `cluster.floor_answer`, and stops asking until it follows another term.
  Any answer will do, since each was written after the leader read a report of the
  present run; a member that waited for the latest would chase a leader that goes on
  appending.
- **The clear.** A member clears its floor once three things hold, since it stored the
  floor and in this run: ADR 0161's two, the live join and the log at or past the floor,
  and its log at or past an answer it took. `clears_vote_floor` in `cavalry-proto` is
  the rule, pinned by the `clears-vote-floor` vectors.

**Why it holds.** Every entry an earlier run of the member held came to it in one of two
ways: from the leader's recordings, by a replay, or from the leader's sender, on the live
log. The answer holds the first, since the leader's recorder reads its publication's log
and never passes the leader's append position. ADR 0161's join holds the second. So
neither condition can go while the argument rests on both.

An answer's session id is the member's present run's. A member that held any of the log
and starts on a new log directory starts on a new driver (`spec/cluster.md`, "What the
second case rests on"), whose session ids begin at a random value
(`spec/client-protocol.md`, "Commands"). A member adds each of its consensus
publications once in a run, as an exclusive publication, so a run on a driver that ran
on has session ids of its own too. An answer to an earlier run, held back as in work
item 411 or late, names that run's session and is ignored.

So an answer the member takes was written after the leader read a report of the
present run, and its log position is at or past every entry the leader had appended
then. An append position counts toward a commit only for the term it names
(`spec/cluster.md`, "Roles and the election", the quorum position), so an earlier run's
acknowledgement commits only at the leader it was sent to, while that leader leads that
term. Any leader the present run follows before its first vote holds every entry such an
acknowledgement commits, by the vote rule: the member's own vote in any quorum that
elected it is the earlier run's, cast after its acknowledgement, or one not yet cast. A
member that has cleared its floor therefore holds every entry a report of its earlier
run can commit, however late the report reaches the leader. The leader counts late
reports as it did, as ADR 0145 meant, and the image rule tried first is not needed.

**Version 3, inside major 0.** "Versions and the upgrade" lets a version of a major add a
message, a record of a snapshot or a use for an entry, where no member, client or
service of an earlier version is sent what it rejects. Version 3 adds a message, sent
only to a member that asked, and to that list it adds a use for a reserved bit of a
`flags` field, which every version of the major ignores. No layout changes, and every
header's version stays 0. A member and a client of version 3 write 768.

- A member of version 3 that holds a floor clears it only once a leader of version 3
  leads, and withholds until then. A member of version 2 that holds one clears it by ADR
  0161's two conditions, as it did.
- A member of version 3 stepped back to version 2 can still be sent an answer its run of
  version 3 asked for, in flight at its stop. It rejects it with the reason `template`
  and counts it in `cluster.messages_rejected`. The step-back is against the rule, as a
  step-back from version 2 with a timer is.
- An upgrade to version 3 begins once no member holds a floor. A member that comes to
  hold one during it, and runs version 3 beside a leader of version 2, withholds: in a
  cluster of three voters no leader is elected while that leader is stopped for its
  upgrade, the cluster elects again once it runs version 3, and the floor clears under
  the first leader of version 3, which `cluster.vote_floor` reading −1 shows.

**What the code does.** `cavalry-proto` reads and writes template 58 and names the flag;
`VERSION` is 3. The core sets the flag, answers through `Consensus::on_message_in`, which
gives a message the session id of the frame it came in, takes an answer by
`Io::consensus_session`, and clears by the three conditions. The module reads each
consensus message with its frame's session id and stores the gauge. `cavalry doctor`
names a member of version 3 that holds a floor no leader has answered, and one whose
log is short of its leader's answer.

## Alternatives considered

- **Read a peer's messages from its newest image alone.** Tried first and dropped
  (Context): image order is not always run order, and the cost of the wrong order is a
  member whose present run is ignored, or an earlier run's report counted.
- **A value of the member's run in each consensus message.** It closes the case on no
  property of the transport, but a field changes the layouts, so `spec/cluster.md`
  would go to a new major, members of the two majors would form no cluster, and the
  upgrade gate would have to cross them. The session id is on the wire already, in
  every frame's header, and the member knows its own.
- **ADR 0145's echo, in canvass position and new leadership term.** It would also take
  "a voter heard" and "Taking a leader" off the stream's order. It changes two layouts,
  so it is a new major too. The floor answer is that echo for the floor alone; "heard"
  stays on the stream's order, as `spec/cluster.md`, "What the second case rests on",
  already says.
- **Answer only in follower log replication and follower catch-up.** A member that
  reaches follower ready with no catch-up, as one that starts on a log it kept with
  its floor still stored, would never be answered and would hold its floor for good.
  The leader answers in whatever state it reads a report.
- **Take only the latest answer.** A member would chase a leader that goes on
  appending. Any answer of the present run is fresh enough.
- **Ask once in a run.** The first build did so. An answer from a leader that a later
  term deposed can lie past what the later leader's log will ever hold, when the later
  leader cut a tail only the deposed one held: in an idle cluster the member would then
  withhold until client traffic grew the log past it. The independent read of the
  first build found it. Asking again under each term ends it at the later leader's
  answer, and keeping the lowest answer stays safe, since each answer alone is.
- **Count every ignored answer.** An answer still on its way when a member's floor
  cleared is routine, and would make `cluster.floor_answers_ignored`, which `doctor`
  reports, rise for nothing. It is ignored uncounted; one ignored while the member holds
  its floor is counted.
- **An answer that carries the leader's log publication position.** The recorder and
  the sender both stand at or below it, so the join would no longer be needed. The
  join stays, as ADR 0161's condition: it costs nothing more, and dropping it is a
  decision of its own on its own evidence.

## Evidence

All runs were on an Apple M-series laptop, macOS 26, in the in-memory cluster of
`cavalry-raft` unless named.

- **The two transport behaviours** are shown by the pages, as the Context says:
  `spec/udp.md`, "The sender", for a publisher past its sender while connected and for
  a closed publication that lingers, and `docs/how-to/run-a-cluster.md` for a driver
  that runs on after its member's process ended.
- **The eight steps, in memory**:
  `a_floor_answer_keeps_a_member_from_voting_before_it_holds_what_a_late_report_commits`
  in `crates/cavalry-raft/tests/lost_log_directory.rs`, under the check of every step.
  - The member loses its log, and the leader's answer to its empty log is held back.
    The leader's live sender is held at that answer's position (`Cluster::hold_sender`)
    while it appends forty entries; the third lacks them. The member's run has them
    from a replay, one entry a step, with its reports cut off once its catch-up runs,
    so the leader counts few of them and never stops the replay. One report of every
    entry is held on its way.
  - The member loses its log again, takes the leader on the held frame with a floor of
    1,152, and joins the live log at 1,344, below the report at 4,992. Then the report
    reaches the leader, which commits 4,992, and the leader stops.
  - With the answer condition taken out of `clears_vote_floor`, ADR 0161's rule alone,
    the check of every step fails: "member 2 leads with a log that differs from what was
    committed".
  - With it, the member's leader had answered it at 4,992, and no leader is elected
    while the leader is stopped. The member votes for no one, and once the leader comes
    back every member holds the entries and the floor clears.
- **No catch-up**: `a_member_that_holds_a_floor_and_needs_no_catch_up_is_answered_and_clears_it`.
  A new cluster's follower holds its floor of 0 with every answer lost
  (`Cluster::lose_floor_answers_to`) and its log at the leader's. Started again on the
  log it kept, it reaches follower ready with no catch-up, is answered and clears.
- **An answer to an earlier run**, in the eight steps too: the leader's first answer to
  the run that acknowledged the entries, written at the floor before they were
  appended, is held back and reaches the present run with the held frame. The member
  ignores it, counted in `cluster.floor_answers_ignored`. With the session comparison
  taken out of the take, the check of every step fails the test as above: "member 2
  leads with a log that differs from what was committed".
- **Asking under each term**: `a_member_asks_for_a_floor_answer_under_each_term_it_holds_none_from`
  in `crates/cavalry-raft/tests/floor_answer.rs`, one member through the stub. It asks
  while it holds a floor, ignores and counts answers for another session or member,
  stops asking once answered for its term, asks again under the next term, keeps the
  lower answer, and clears at it. With the ask tied to the run alone, as in the first
  build, it fails: "under a term it holds no answer from, it asks again".
- **A lost answer**: `a_floor_answer_lost_on_its_way_is_sent_again_in_the_next_interval`.
  A new cluster's followers hold their floors for two seconds while every answer is
  lost and the leader keeps answering each interval. Once the answers get through, the
  next ones clear both floors.
- **The rest of `cavalry-raft`**, its tests over three drivers in `tests/cluster.rs`
  among them, passes, in 394 s in a debug build. The tests that drive one core through a
  stub take their leader's answer where they follow a term with an empty log, as
  `tests/common/mod.rs`, `follow_term`, does.
- **ADR 0145's and ADR 0148's regression seeds, proven again** on this rule, each from a
  release binary built with its rule taken out and the source restored at once.
  - `lost_log_search`, with the term condition of ADR 0145's one voting case taken out:
    seeds 779, 1573 and 2983 find two members leading one term, `[4, 4]`, `[2, 2]` and
    `[4, 4]`; seed 1 passes.
  - `faulted_truncation_search`, with ADR 0148's rule taken out: FAULTED_SEEDS_PLACEHOLDER
- **Wide runs on this rule**, release builds: `lost_log_search` seeds 1 to 500 pass, in
  330 s; `faulted_truncation_search` seeds 1 to 300 pass, in 191 s; `timer_search` seeds
  1 to 200 pass, in 853 s, with every seed's quorum settled at its end.
- **The rule's other holders**: the `clears-vote-floor` vectors, now with the answer,
  and the encoding vectors of template 58 and of an append position with the floor flag,
  347 vectors in all, which the generator and the JSON agree on; and `doctor`'s floor
  test, which names a member of version 3 that no leader has answered and one short of
  its answer.
- **Gates:** GATES_PLACEHOLDER

## Consequences

- ADR 0161's last case is held: the eight steps end with no leader elected without the
  entry.
- ADR 0161's Revisit is answered: the floor's comparison and the join are kept, and the
  answer is added. Each holds entries of its own.
- A report of a member's earlier run still counts toward a commit when it reaches the
  leader late. A fresh floor keeps that safe. One effect of it is liveness alone: a late
  report with the catch-up flag can stop the replay the leader serves the member's
  present run, which then stalls until it asks again. Work item 466 carries it, in
  feature 4.
- Two drivers whose random session ids meet would name two runs of a member alike, and
  an answer to the earlier one could clear the later one's floor. That collision would
  also merge the two runs' frames into one session at the leader's receiver
  (`spec/udp.md`, "The receiver", step 1), and its chance is about 2⁻³² for each start.
- A member that holds a floor asks in every report, and its leader answers at most once
  each heartbeat interval: one consensus message more each interval for each such
  member, while it holds its floor.
- A new cluster's followers hold a floor of 0 until their first leader answers them,
  which it does in the same election.
- The in-memory cluster gains a live sender a test holds (`Cluster::hold_sender`), a
  held-back report and a held-back floor answer (`Cluster::hold_back_append_position`,
  `Cluster::hold_back_floor_answer`) under the one bound of the held-back frames,
  answers a test loses (`Cluster::lose_floor_answers_to`), and a session for each run
  of each member. A replay to a member that starts again now ends as stopped at once, so
  that its leader reads the stop; before, it was dropped, and its leader waited for a
  stop that never came. That is a shortcut: a driver ends such a replay at its leader's
  stop, or after the replay connect timeout with no subscriber, and until then its image
  can reach the new run's catch-up subscription, which costs time and no entry, and
  which the model leaves out.
- `Consensus::on_message` reads a message with no session, so a leader read only there
  answers no floor ask. The module reads every consensus message through
  `Consensus::on_message_in`, with its frame's session id.
- `cavalry doctor` names a member of a build before ADR 0161 that holds a floor at or
  past its log, which that build never clears and a later build withholds on, so that
  an upgrade to version 3 waits until it runs a build of ADR 0161 or later, which clears
  it at its next join.
- Revisit when a member's consensus publications may share a session id across its runs,
  as a driver that reused ids would make them; when a later major carries a value of
  the member's run, which would let the answer rest on no property of the driver; and
  when the join is weighed against an answer that carries the publication position.

## References

- Work item 463, which carried this decision; work item 411 and ADR 0161; work item 286
  and ADR 0145; work item 466, the late catch-up flag.
- `spec/cluster.md`, "Members and streams", "A member that lost its log", "The floor
  answer", "Floor answer, template 58", "Append position, template 54", "Versions and the
  upgrade"; `spec/udp.md`, "The sender" and "The receiver"; `spec/client-protocol.md`,
  "Commands".
