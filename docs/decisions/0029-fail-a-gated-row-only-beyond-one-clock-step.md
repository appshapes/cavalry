# 0029. Fail a gated row only beyond one clock step as well as beyond 5 percent

- **Status:** Accepted
- **Date:** 2026-10-26
- **Invariants affected:** 2
- **Supersedes:** none; it adds a floor to the rule of ADR 0016 and ADR 0017

## Context

Card 143. ADR 0017 gates the rows a message passes through, `claim_commit_*` and
`one_way_*`, by the rule of ADR 0016: a median p99.99 over the repeats more than 5
percent above the slowest of the baseline's runs fails. It held the rule safe because
those rows sat at 380 to 670 ns, where one step of the clock was 1.5 to 2.6 percent, and
named in its Revisit clause the day the harness would have a cycle counter and the probes
would resolve below a clock step.

ADR 0028 gave the harness that counter, and the bench host's rows moved to where the
step matters. Its counter advances in steps of 10 ns, so every percentile it reports is
a multiple of 10, and the `claim_commit_*` rows now sit at 150 to 170 ns, where one step
is 6 to 7 percent. The run that recorded the baselines (#56, run 40379532297) read
`claim_commit_32_client` at 160 to 170 ns across its 21 runs against a baseline slowest
run of 160, and `claim_commit_32_exclusive` at 170 to 180 against 170: an unchanged
commit whose median lands one step up fails the gate at +6.3 percent, and a gate that
fails an unchanged commit teaches people to re-run it until it passes. The second bench
host's counter resolves to a nanosecond, so the same rows there read 117 to 223 and the
5 percent rule is a measured 5 percent; on Apple silicon `quanta` reads the monotonic
clock in nanoseconds with a 41.67 ns tick, and a laptop has no baseline.

## Decision

A gated row fails when its median p99.99 is more than 5 percent above the baseline's
slowest run **and** more than one step of the harness's clock above it. The harness
measures the step once at start, the greatest common divisor of the counter's own
non-zero advances between a million pairs of consecutive readings, converted to
nanoseconds, prints it in its first line and writes
it into the result as `clock_step_ns`; `bench-compare` reads it from the result under
judgement, prints it, stores it in the baseline with `--update`, and marks a row that is
over 5 percent but within one step "within a step". A result from before the field is
judged by the 5 percent rule alone. The probe rows stay reported. The rule's floor is
the clock's, not a number chosen per host.

## Alternatives considered

- **Raise the tolerance to 10 percent.** Lost: the `one_way_*` rows at 220 to 290 ns
  would then move 20 to 30 ns before failing, three steps, where the gate has caught
  regressions of 5 percent before (ADR 0022 found a 100 ns cost at 600 ns); the floor
  costs nothing where the clock resolves.
- **A floor in nanoseconds chosen per host.** Lost: a number in a file is a knob to
  make a benchmark pass, and the step is a fact the harness can measure.
- **Compare medians with the baseline's median plus the step.** Lost: ADR 0016 chose the
  slowest run because every row's 21 runs spread 10 to 20 percent inside one process;
  the floor adds to that rule, it does not replace it.
- **Report the percentiles with more digits.** Lost: the counter's step is the
  measurement's; printing a finer number would print a lie.

## Evidence

The runs in Context, on card 143 and in #56's body. `scripts/bench-compare-test.py` holds
the rule on made-up results: one step up at 160 ns passes and says so, two steps fail,
a fine clock keeps the 5 percent rule, a probe row is reported.

## Consequences

- The result and the baseline carry `clock_step_ns`; the bench host's is 10, the second
  bench host's 1. An Apple silicon laptop's reads 1 too, since its clock's advances of
  41 and 42 ns divide to 1; its percentiles step by 41 or 42 and it has no baseline.
- On the bench host a `claim_commit_*` row can move one step, 10 ns of 160, without
  failing; a `one_way_*` row at 290 is held at 5 percent, 15 ns, as before, since one
  step is less.
- ADR 0017's Revisit clause has fired on its first half, the counter; the probes still
  sit on a step and stay reported.
- Revisit: when a host's step is above 5 percent of a `one_way_*` row, or when the
  harness reports in finer units than the clock resolves.

## References

- Card 143; `scripts/bench-compare.py`, `scripts/bench-compare-test.py`;
  `bench/src/main.rs`, `clock_step_ns`.
- [ADR 0016](0016-run-the-latency-gate-on-a-dedicated-host.md),
  [ADR 0017](0017-gate-the-message-rows-and-report-the-host-probes.md),
  [ADR 0028](0028-stamp-bench-samples-with-the-time-stamp-counter.md).
