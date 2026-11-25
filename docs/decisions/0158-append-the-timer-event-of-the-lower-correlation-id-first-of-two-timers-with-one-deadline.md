# 0158. Append the timer event of the lower correlation id first, of two timers with one deadline

- **Status:** Accepted
- **Date:** 2026-11-24
- **Invariants affected:** 6
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

Of two timers that are due with one deadline, a leader appends the event of the
lower correlation id first. The page says so in the sentence that gives the order,
and the member's table is held to it by its unit test. The rule orders what a
leader holds, unmarked and due, at one look. It says nothing of a timer the leader
is told of after the other's event is appended, nor of one it holds marked.

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

- A program may rely on the order of two timers of one deadline only where the
  leader was told of both before the deadline came and holds neither marked, as it
  is of timers scheduled for a time more than a refresh ahead beside a leader that
  has appended no event for either: the lower id's event is then first in the
  log. A leader that holds one of them marked, by an event that its log no longer
  holds, appends the other alone until its service's refresh takes the mark off.
  Two timers scheduled for a time already past, in two entries, come in no order a
  program can rely on. In each duty cycle the leader reads up to 10 messages of the
  consensus module stream before it looks for a due timer. Two that it reads in one
  duty cycle are appended the lower id first, whatever their entries. So are two of
  which it still holds the first unmarked when it reads the second: its log refused
  the first's event, it had appended 10 events in that duty cycle, or it did not yet
  lead with its election closed. Two whose first event it appended before it read the
  second are appended in the order it read them, whatever their ids. A leader that is
  told of one late appends it late.
- It is an order of appending, and not a promise that two such events are next to
  each other in the log: a leader appends at most 10 timer events in a duty cycle,
  and other entries may lie between two of them.
- A second implementation of a member has one more rule to hold.
- A reason to revisit: a program that needs the order of its schedules kept for
  timers of one deadline whenever they were scheduled. That is a field in the
  schedule timer, and a change of the wire.

## References

- Work item 419 on the Brigade Board, which this decision was made in; pull request
  348's review, which named the gap.
- ADR 0151, which this adds one rule to.
- `spec/cluster.md`, "The service", "Timers", "The leader's timer events".
