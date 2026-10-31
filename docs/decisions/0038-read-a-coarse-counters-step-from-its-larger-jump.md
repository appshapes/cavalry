# 0038. Read a coarse counter's step from the larger of the two sizes its jump comes in

- **Status:** Accepted
- **Date:** 2026-10-31
- **Invariants affected:** 2, 6
- **Supersedes:** 0037, whose rule stands that a counter unmoved on most pairs is read
  by its jump and not by a divisor, and whose choice of the smaller jump this reverses;
  through it, the measurement rule of ADR 0029 for such a counter, as 0037 did. ADR
  0029's rule and the gate it sets stand for every counter that advances on most
  readings

## Context

ADR 0037 read a coarse counter's step from the jump it makes: when most of the million
pairs of consecutive readings `clock_step` takes show no advance, the counter moves
less often than the harness reads it, and ADR 0029's divisor reads 1 where the clock
resolves tens of nanoseconds. Such a counter's period is seldom a whole number of the
reading's ticks, so its jump comes in two adjacent sizes: on the Apple silicon laptop
pull request 58 was written on, 3,318 pairs of a million advanced by 41 ticks and 8,942
by 42, two jumps in three the larger. ADR 0037 took the smaller size, 41, reasoning that
a floor may under-read the step and may not over-read it, and cited `bench-compare`'s
choice of the smaller of the result's and the baseline's steps as the same reason. The
review of pull request 78 found that reasoning inverted; the pull request had merged
before the review could be applied on its branch, and an Accepted ADR changes only its
Status line (ADR 0001), so the correction is this record.

`bench-compare` fails a gated row when its p99.99 is more than 5 percent above the
baseline's slowest run and more than the floor above it, `now - slowest > floor`
(`scripts/bench-compare.py`), and the floor exists so that an unchanged commit whose
tail lands one step up cannot fail, since "a gate that fails an unchanged commit
teaches people to re-run it until it passes" (ADR 0029). With a floor of 41 ns a row
that moved by one jump of 42 ns, and by more than 5 percent, is a `REGRESSION`; with 42
it is `within a step`. A floor of 41 under a counter that jumps 42 two times in three
is the floor of 9 under a clock that moves 10 that `clock_step`'s rounding to the
nearest nanosecond exists to avoid. The smaller of two steps in `bench-compare` answers
another question: it keeps a change to the harness that over-reads its step from
widening the gate against an older baseline (`floor_step`'s docstring), and says
nothing about which of a counter's two jump sizes is its step.

## Decision

When most pairs advanced by none, `step_ticks` reads the step as the largest frequent
advance, at least one tick: the floor must admit every one-jump move an unchanged
commit can make, and only the larger of a counter's two jump sizes does. The over-read
is below one tick, a third of a nanosecond on the laptop, and admits nothing an
unchanged commit could not produce, since every one-jump move there is 41 or 42.
Everything else in ADR 0037 stands: the jump rule fires only when the unmoved pairs
are a majority, an advance in fewer than one pair in a thousand is left out, and a
counter that advances on most readings takes ADR 0029's divisor. The laptop's
histogram reads 42 ticks, 42 ns at the 1 GHz `quanta` reads that counter at, where ADR
0037 read 41; the bench host, kip, grip and the macOS CI runner read what they read,
10 ns (37 ticks), 1 ns (2 ticks), 1 ns (1 tick) and 42 ns (1 tick).

## Alternatives considered

- **The smaller jump, 41 on the laptop, as ADR 0037 chose.** Lost: two jumps in three
  on that counter are 42, so a floor of 41 fails two of every three one-jump moves of
  an unchanged commit, which is the failure ADR 0029 built the floor to prevent; the
  third of a nanosecond it saves passes nothing.
- **The most frequent jump.** Lost: it reads 42 on the laptop, but on a counter whose
  period's fraction is below a half the smaller size is the more frequent one and the
  mode under-reads; the floor has to admit every frequent one-jump move, which only
  the larger size does whatever the fraction.
- **Edit ADR 0037 in place, as the review asked.** Lost: it was Accepted on `master`
  when the review landed, and an Accepted ADR changes only its Status line (ADR 0001;
  `just lint-docs` refuses more).

## Evidence

- `bench/src/main.rs`, `mod tests`: the laptop's histogram, 987,728 pairs unmoved,
  3,318 at 41 ticks, 8,942 at 42 and 18, 58 and 59 a few times each, reads 42 under
  this rule; one pair more than half unmoved with jumps of 41 and 42 reads 42; exactly
  half still takes the divisor and reads 1; the 24 MHz runner's shape reads 1; the
  bench host's and kip's histograms read 37 and 2 as before.
- `scripts/bench-compare.py`, the gate's test: `ratio > TOLERANCE and now[GATED] -
  slowest > floor`. On the laptop's counter a one-jump move is 41 or 42 ns; a floor of
  41 passes the first and fails the second wherever 42 ns is over 5 percent of the row,
  every gated row below 840 ns.
- The laptop has not been re-run with this rule. Its first line follows from the
  histogram and from the 1 GHz the counter is read at, `clock step 42 ns (42 ticks)`,
  and the author's next `just bench` there confirms it. The gated hosts' counters
  advance on every pair, so their steps do not move and `bench/baseline.json` is
  untouched.

## Consequences

Easier: a coarse counter's floor admits every one-jump move, as the bench host's does.
Harder: nothing on the bench hosts. Revisit: when a host's counter shows three or more
jump sizes, which this rule reads as the largest frequent one and may over-read by
more than a tick; when a counter moves in fewer than a thousand of the million pairs,
or in two sizes of which neither reaches a thousand, so that no advance is frequent
and the step reads 1 tick as it did before ADR 0037; and when a counter is read
between once and twice per move, so that fewer than half its pairs are unmoved, which
takes the divisor and reads 1 over two adjacent jump sizes.

## References

- `bench/src/main.rs`, `clock_step`, `step_ticks` and `mod tests`;
  `scripts/bench-compare.py`, `floor_step`; `docs/how-to/run-the-bench.md`
- [ADR 0001](0001-record-decisions-as-adrs.md),
  [ADR 0029](0029-fail-a-gated-row-only-beyond-one-clock-step.md),
  [ADR 0037](0037-read-a-coarse-counters-step-from-its-jump.md); card 154, pull
  requests 58 and 78 and the review of 78 that found the inversion
