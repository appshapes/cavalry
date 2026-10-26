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
a multiple of 10 or one below it (the baseline recorded at 377a190 holds 159 beside 160
and 150 in one process: the spans are truncated, and the step is not a whole number of
nanoseconds once calibrated), and the `claim_commit_*` rows now sit at 150 to 170 ns,
where one step is 6 to 7 percent. The run that recorded the baselines (#56, run 40379532297) read
`claim_commit_32_client` at 160 to 170 ns across its 21 runs against a baseline slowest
run of 160, and `claim_commit_32_exclusive` at 170 to 180 against 170: an unchanged
commit whose median lands one step up fails the gate at +6.3 percent, and a gate that
fails an unchanged commit teaches people to re-run it until it passes. The second bench
host's counter resolves to a nanosecond, so the same rows there read 117 to 223 and the
5 percent rule is a measured 5 percent. On Apple silicon the step is the machine's own
counter's tick: `quanta` reads `cntvct_el0` on every aarch64 build (its detection names
the counter as always present outside iOS, so there is no fallback there; ADR 0028), and
the macOS CI runner read one tick of its 24 MHz counter, 42 ns, while the laptop this was
first run on read 1 ns. Neither has a baseline.

## Decision

A gated row fails when its median p99.99 is more than 5 percent above the baseline's
slowest run **and** more than one step of the harness's clock above it. The harness
measures the step once at start, on the publisher's CPU once it is pinned, with its
counts on the stack: a million pairs of consecutive readings, the counter's advances
between them counted, the advances that occur in fewer than one pair in a thousand left
out as interrupts or migrations, those below half the most frequent one left out as the
counter's own residual, and the greatest common divisor of the rest converted to
nanoseconds, rounded to the nearest. On the bench host the counter advances by 37 ticks,
10 ns, at a time, reads 37 or 74 between two readings and a single tick in 2 percent of
them, which a divisor over everything turned into a step of 1, as the first runs of #57
showed; on kip the advances are 26 to 54 ticks in even numbers and divide to 2, a third of
a nanosecond each, so its step is 1 ns. It prints the step in ticks and nanoseconds in its
first line and writes
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
- **The greatest common divisor of every frequent advance.** Lost: the bench host's
  one-tick residual, in 2 percent of its pairs, divides 37 and 74 to 1, and CI read
  `clock step 1 ns` there on both passes of runs 40386774358 and 40387901949, the
  hazard ADR 0017 named for the OS clock's residuals; a floor of 1 ns is no floor.
- **The smallest frequent advance.** Lost: on the bench host that is the residual
  itself, 1 tick.
- **Counting the advances in a heap vector.** Lost: a 16 KiB allocation at startup
  moved the raw rows' `one_way` p99.99 by 60 ns on the bench host, in every run of an
  interleaved comparison (#57); the counts live on the stack.

## Evidence

- CI runs 40386774358 (047def1) and 40387901949 (e5e2752), job `bench (linux)`: the
  plain divisor read `clock step 1 ns` on both passes; the second run also read the raw
  rows' `one_way` p99.99 at 290 and 300 ns against baselines of 230 and 240, the heap
  vector's cost, and failed the gate.
- CI run 40389997682 (c281f1b), job `bench (linux)`: `clock step 10 ns (37 ticks)` on
  both passes, the raw rows back at 220 to 240, `bench-compare: ok`; its
  `gates (macos)` job: `clock step 42 ns (1 ticks)`, with the `timer` row's p50 at 0
  and its p99 at 41. The advances on the bench host at 5712454: 37 in 965,497 pairs of
  a million, 1 in 23,282, 74 in 11,203. The interleaved comparison of the heap vector
  against the stack array is in #57's body.
- `scripts/bench-compare-test.py` holds the rule on made-up results: one step up at
  160 ns passes and says so, two steps fail, a fine clock keeps the 5 percent rule, a
  probe row is reported, a result without the step is judged by the tolerance alone and
  `--update` writes no step from it.

## Consequences

- The result and the baseline carry `clock_step_ns`; the bench host's is 10, the second
  bench host's 1, the macOS CI runner's 42 and a laptop's 1, as the Context says.
- The measurement keeps its counts on the stack: a 16 KiB heap allocation at startup
  moved the raw rows' one_way tail on the bench host by 60 ns at p99.99 in every run of
  an interleaved comparison, which is a fragility of the harness with a card of its own.
- On the bench host a `claim_commit_*` row can move one step, 10 ns of 160, without
  failing; a `one_way_*` row at 290 is held at 5 percent, 15 ns, as before, since one
  step is less.
- ADR 0017's Revisit clause has fired on its first half, the counter; the probes still
  sit on a step and stay reported.
- Revisit: when a host's step is above 5 percent of a `one_way_*` row, or when the
  harness reports in finer units than the clock resolves.

## References

- Card 143; card 144, the harness's sensitivity to a heap allocation at startup;
  `scripts/bench-compare.py`, `scripts/bench-compare-test.py`; `bench/src/main.rs`,
  `clock_step`.
- [ADR 0016](0016-run-the-latency-gate-on-a-dedicated-host.md),
  [ADR 0017](0017-gate-the-message-rows-and-report-the-host-probes.md),
  [ADR 0028](0028-stamp-bench-samples-with-the-time-stamp-counter.md).
