# 0037. Read the clock step from the counter's jump when most readings show no advance

- **Status:** Accepted
- **Date:** 2026-10-31
- **Invariants affected:** 2, 6
- **Supersedes:** the measurement rule of ADR 0029 for a counter that moves less often
  than the harness reads it; ADR 0029's rule and the gate it sets stand for every
  counter that advances on most readings

## Context

ADR 0029 made the gate's floor the harness's clock step, measured at start from a
million pairs of consecutive readings: the advances that occur in fewer than one pair
in a thousand left out as interrupts, those below half the most frequent one left out
as the counter's residual, and the greatest common divisor of the rest. The divisor is
right for a counter that advances on every reading in multiples of its step, as the
bench host's does (37 or 74 ticks, with a one-tick residual) and kip's (26 to 34 ticks in
even numbers). It is wrong for a counter that moves less often than the harness reads
it: there the step is the jump the counter makes when it moves, and the jump's size is
whatever the reading's units make of the counter's period. On the Apple silicon laptop
pull request 58 was written on, `quanta` reads the machine's counter at 1 GHz, but the
counter moves 24 million times a second: in a million pairs 987,728 advanced by none,
3,318 by 41 ticks and 8,942 by 42, with 18, 58 and 59 a few times each, and 41 and 42
divide to 1, so the harness printed `clock step 1 ns (1 ticks)` where the clock resolves
about 42 ns. ADR 0029 recorded the laptop's 1 ns as an observation. The macOS CI runner's
counter ticks at 24 MHz itself and reads one tick, 42 ns, which is right by either rule.
Neither machine has a baseline, so no gate was wrong; the hazard was a host with a
coarse-moving counter and a baseline, whose gate would have no floor where it needs one
(card 154).

## Decision

`clock_step` counts the pairs that advanced by none, which it dropped before. When they
are the majority of the pairs, the counter moves less often than the harness reads it,
and `step_ticks` reads the step as the smallest frequent advance, at least one tick,
rather than the divisor of the frequent advances: a counter whose period is not a
whole number of the reading's ticks jumps by two adjacent sizes, and the smaller one
under-reads the step by less than a tick, which a floor may do, where the larger one
would over-read it, which a floor may not (`bench-compare` takes the smaller of the
result's and the baseline's steps for the same reason, ADR 0029). Otherwise the rule is
ADR 0029's. The laptop now prints `clock step 41 ns (41 ticks)`; the bench host and kip
print what they printed, 10 ns (37 ticks) and 1 ns (2 ticks); the macOS CI runner 42 ns
(1 tick). The harness's test module holds the laptop's histogram beside the two bench
hosts', and the half-unmoved boundary.

## Alternatives considered

- **The most frequent jump, 42 on the laptop.** Lost: it over-reads the step by a third
  of a nanosecond on this counter, and a floor that is too high passes a regression of
  up to that much; the smaller size errs the way a floor may.
- **The mean jump weighted by its counts, 41.72.** Lost: the step is an integer number of
  ticks everywhere the harness prints it and `bench-compare` compares it, and a
  fraction would be rounded to one of the two sizes anyway.
- **Read `quanta`'s own resolution instead of measuring.** Lost: the library reports
  the counter's calibration, not how often the counter moves, which is exactly what the
  laptop showed to differ; the measurement is the harness's own evidence and the
  histogram goes in the test module where a stranger can read it.
- **Leave the rule, since neither machine has a baseline.** Lost: the gate's floor
  exists so that a host's own resolution cannot be mistaken for a regression, and a
  future host with a coarse counter and a baseline would have printed a step of 1 and
  failed unchanged commits one tick apart.

## Evidence

- The laptop's histogram above, a million pairs, from pull request 58's session (card
  154): with this change `just bench --messages 100000` on the same machine prints
  `clock step 41 ns (41 ticks)`, and `master` prints `clock step 1 ns (1 ticks)`.
- `cargo test -p cavalry-bench`: the laptop's histogram reads 41, a 24 MHz counter read
  faster than it ticks reads 1, the bench host's and kip's histograms read 37 and 2 as
  before, and exactly half the pairs unmoved still takes the divisor.
- The gated hosts' steps are unchanged, so no baseline moves; the bench jobs on the
  pull request compare against their baselines as before.

## Consequences

Easier: a laptop's report says what its clock resolves, and a future host with a coarse
counter gets a floor. Harder: nothing on the bench hosts, whose counters advance on
every pair; the harness carries one more count on the stack. Revisit: when a host's
counter shows three or more jump sizes, which this rule reads as the smallest frequent
one and may under-read by more than a tick.

## References

- `bench/src/main.rs`, `clock_step`, `step_ticks` and `mod tests`; `scripts/bench-compare.py`
- `docs/how-to/run-the-bench.md`; ADR 0028, ADR 0029; card 154, pull request 58
