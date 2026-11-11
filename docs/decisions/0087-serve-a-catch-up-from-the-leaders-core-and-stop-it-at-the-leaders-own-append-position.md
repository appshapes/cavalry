# 0087. Serve a catch-up from the leader's core, and stop it at the leader's own append position

- **Status:** Accepted
- **Date:** 2026-11-11
- **Invariants affected:** 1, 3, 4, 6
- **Supersedes:** none. Adds three clauses to `spec/cluster.md`, "Messages outside an
  election": a catch-up position above the leader's own append position is answered
  by nothing; the leader's log position that ends a catch-up is its own append
  position, and the follower's report must be for the leader's term; and stop
  catch-up is sent for each such report. The page's version stays 0: no message
  changes, and no member over a driver takes a vote yet (ADR 0081).

## Context

ADR 0081 listed the steps of the electing path. The fifth is the catch-up within a
leader's term, states 11 to 13: a member that has joined its leader's term, and is
behind the leader's log, is brought the log from the leader's live recording until
it can join the live log. As with a replication (ADR 0085, ADR 0086), it has two
halves, and the leader's is built first.

`spec/cluster.md`, "Messages outside an election", says what the leader does. A
catch-up position at or above its term base log position starts a replay that
follows its live recording, counted in `cluster.catchups`. The leader stops the
replay, with `stop replay`, and sends stop catch-up, when a follower whose position
was appended from the replay reports an append position at or past the leader's log
position.

Building it found three things the page did not settle.

**Which position is "the leader's log position".** A member's append position is
what its recordings have synced (ADR 0060), and a leader's is its own, the same
way. A leader's log publication is ahead of that by whatever it has appended and
not yet synced. Aeron ends a catch-up at the publication's position:
`ConsensusModuleAgent.trackCatchupCompletion` compares the follower's log position
with `logPublisher.position()`. The page's new leadership term calls its field `log
position` "the leader's append position". The core knows its own append position
and not its publication's.

**What a position above the leader's own asks for.** A member can report a log that
ends above its leader's: the page counts such a member in leader ready, and does
not say what a catch-up position from one starts.

**How often stop catch-up is sent.** The page says the leader sends it "when" the
follower reports. A consensus message can be lost, and every other one is sent
again at an interval.

## Decision

The leader's core serves a catch-up through the table and the operations it serves
a replication with, and ends it at its own append position.

**One replay a member, of either kind.** `Serving::Running` holds a replication's
end, or none for a catch-up, whose replay follows the leader's live recording until
it is stopped. `Io::start_replay(member, position, end)` takes that end as an
option. A catch-up position, a canvass position and init do with a catch-up's
replay what they do with a replication's (ADR 0085). A catch-up's replay that ended
is followed by nothing, however it ended.

**The position tells the two apart, and bounds both.** Below the leader's term base
log position it is a replication. From the base to the leader's own append position
it is a catch-up. Above that the leader holds nothing to replay: it starts nothing,
and the member's own timer counts it. A request the leader does not serve, above
its append position or below 0, still says that the member left what it was in: the
leader ends what it held for that member, the replay stopped and the one owed
dropped, as a canvass position ends it. The review of pull request 226 noted that
the page has every new catch-up position do so.

**One member's fault does not starve another.** The leader serves every member in
every duty cycle, and returns the first fault after all have had their turn. Before,
the first fault ended the cycle's serving, so a start that faulted for one member in
every cycle kept the members after it in the table from their replays; the same
review noted it.

**The catch-up ends at the leader's own append position.** When a member reports,
with the catch-up flag and for the leader's term, an append position at or past the
leader's own, the leader asks the replay it runs to that member to stop, once, and
sends stop catch-up. Both sides of the comparison are then positions that
recordings have synced. Under a steady load a follower's synced position trails the
leader's publication by the follower's own sync, and might never be reported at or
past it; it reaches the leader's synced position whenever the follower syncs as
fast as the leader does.

**Stop catch-up is sent for every such report.** The member reports with the flag
until it has read stop catch-up and left the catch-up. A stop catch-up that was
lost is sent again on the member's next report, at the latest a leader heartbeat
interval later. The replay is asked to stop once. A stop that fails is a fault,
returned from the report, and the next report asks again; stop catch-up is not sent
before the stop is asked.

**Counted once a replay.** `cluster.catchups`, id 137, rises by one for each replay
a leader starts for a catch-up.

**What does not serve yet.** The in-memory cluster starts no catch-up replay, and
the consensus module refuses, as for a replication (ADR 0085). The member's half
brings the in-memory one.

## Alternatives considered

- **End the catch-up at the publication's position, as Aeron does.** Lost, for the
  reason above, and because it needs an operation of `Io` that reads a position no
  other rule of the core uses. Its merit is real: a leader whose own sync stalls can
  end a catch-up before the follower holds the bytes up to where its live image
  joined, since the replay reads what the recording wrote and not what it synced.
  That follower's catch-up then stops moving, fails after the leader heartbeat
  timeout and begins again. The member's half shows whether that happens over
  drivers.
- **Send stop catch-up once.** Lost. One lost message would leave the member in the
  catch-up until its timer failed its election.
- **A second table for catch-ups.** Lost. A member is in one of the two, and the
  page gives it one replay at a time.
- **Start a replay from the leader's append position for a member above it.** Lost.
  The member holds bytes the leader does not, which is the truncation's or the
  election's to settle, not a replay's.

## Evidence

- `crates/cavalry-raft/tests/catchups.rs`, ten tests on one core through an `Io`
  that remembers what it was asked. The leader is member 0 of three, leading term 2
  from 96 with its append position at 160.
  - A catch-up position at 96 and one at 160 each start a replay with no end,
    counted in `cluster.catchups`; one at 95 starts a replication to 96; one at 192
    starts nothing.
  - A report from the replay at 128, and a report at 160 without the flag, stop
    nothing. A report from the replay at 160 stops the replay and then sends stop
    catch-up; a second such report sends stop catch-up again and stops nothing.
  - A report from the replay for term 1 stops nothing.
  - A catch-up's replay that ended is followed by nothing, and a new request is
    served at once.
  - A canvass position stops a catch-up's replay; a member that asks three times
    while one runs has one stop asked and its newest request started.
  - A leader that lost its quorum stops the replay in init and sends no stop
    catch-up as a follower.
  - A stop that fails is returned, stop catch-up is not sent, and the next report
    asks again and sends it.
  - A catch-up position above the leader's append position stops the replay the
    leader ran to that member, and one below 0 drops the replay it owed another.
  - With a start to member 1 faulting in every cycle, member 2, after it in the
    table, has the next recording's replay started in the same cycles, and the fault
    is returned from each.
  - An appointed leader serves no catch-up.
- `crates/cavalry-raft/tests/replays.rs` passes with one case moved: a position at
  the leader's term base is a catch-up now, and no longer starts nothing.
- Every other test of the crate passes unchanged. No member sends a catch-up
  position at or above its leader's base yet, so no cluster test can move.

## Consequences

- `Io::start_replay` takes an end that may be none. The module's I/O for the
  election replays the recording of the term it leads with no end for a catch-up.
- `Counts` has twenty-two counts, and a member's module adds twenty-two client
  counters; eleven of them stay 0 while the module's leader is appointed.
- A request whose start faulted is kept as a replay owed until the member reports
  the position (ADR 0085). A member that waits for a catch-up's image reports no
  position, so such a request is in practice dropped by the canvass position the
  member sends when its own wait runs out.
- Revisit: with the member's half and the tests over drivers, whether a catch-up
  can end short of the live image's join position; then `Io` gains the
  publication's position and this decision is superseded.

## References

- `spec/cluster.md`, "Messages outside an election", rows 11 to 13 of the election's
  states, "Append position, template 54", "Stop catch-up, template 57" and
  "Counters".
- [ADR 0085](0085-serve-a-replication-from-the-leaders-core-to-the-endpoint-the-members-string-names.md),
  the table and the operations a replication is served with.
- [ADR 0081](0081-build-the-election-beside-the-appointed-leader-and-move-the-module-to-it-once.md),
  the electing path and its steps.
- [ADR 0060](0060-replicate-the-log-with-an-appointed-leader.md), a member's append
  position as what its recordings have synced.
- Aeron's `ConsensusModuleAgent.trackCatchupCompletion`, read on 2026-11-11:
  <https://github.com/aeron-io/aeron/blob/master/aeron-cluster/src/main/java/io/aeron/cluster/ConsensusModuleAgent.java>
