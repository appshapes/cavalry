# 0158. Append the timer event of the lower correlation id first, of two timers with one deadline

- **Status:** Accepted
- **Date:** 2026-11-24
- **Invariants affected:** 1, 6
- **Supersedes:** none. Amends one sentence of `spec/cluster.md`, "The service",
  "Timers", "The leader's timer events", within version 2. Nothing on the wire
  changes.

## Context

ADR 0151 gave a cluster's service timers. A member that leads appends a timer event
for each timer it holds whose deadline its clock has reached, and `spec/cluster.md`
says in which order: "earliest deadline first". It does not say which of two timers
with one deadline comes first.

The order is in the log, so it is the same beside every member and at every replay:
invariant 1 does not depend on the answer. A program can see it, though. An
application that schedules two timers for one millisecond, a timeout and a retry,
say, is handed them in the order their events were appended, and what the page
leaves open a second implementation of a member may choose otherwise. The review of
pull request 348, which added the member's table, named the gap twice.

The table that work item 419 built, `MemberTimers`, keeps its timers in the rising
order of their correlation ids and gives the first of those that are due earliest,
so it has always appended the lower id first.

## Decision

Of two timers with one deadline, a leader appends the event of the lower correlation
id first. The page says so in the sentence that gives the order, and the member's
table is held to it by its unit test.

## Alternatives considered

**Leave the order open, and say on the page that it is open.** A program that
depends on the order would then work beside one leader and not beside another, and
would find out at a change of leader. An order that costs nothing to fix is fixed.

**The order in which the member was told of the timers.** It is the order a program
scheduled them in only while one service tells one member, with nothing lost: a
member that starts holds no timer and is told of every pending one again by its
service's refresh, in the order that sends them, and a new leader was told by
another service. The correlation id is the one thing every member holds of a timer
besides its deadline.

## Evidence

- `crates/cavalry-raft/src/timers.rs`,
  `a_timer_is_due_once_and_again_when_it_is_scheduled_again`: two timers of one
  deadline, ids 7 and 3, scheduled in that order, and one earlier; the earlier is due
  first, then 3, then 7.
- `crates/cavalry-raft/tests/timer_search.rs` runs the same table under every seed,
  and checks what a service is handed against a reading of the rule that knows
  nothing of the order: the order changes no check of it.

## Consequences

- A program may rely on the order of two timers it scheduled for one deadline: the
  lower id is handed over first, where both events are in the log and both timers
  are pending when their events are read.
- It is an order of appending, and not a promise that two such events are next to
  each other in the log: a leader appends at most 10 timer events in a duty cycle,
  and other entries may lie between two of them.
- A second implementation of a member has one more rule to hold.

## References

- Work item 419 on the Brigade Board, which this decision was made in; pull request
  348's review, which named the gap.
- ADR 0151, which this adds one rule to.
- `spec/cluster.md`, "The service", "Timers", "The leader's timer events".
