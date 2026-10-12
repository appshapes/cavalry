# 0017. Gate the rows a message passes through, and report the host probes

- **Status:** Accepted
- **Date:** 2026-10-12
- **Invariants affected:** 2, 6
- **Supersedes:** one sentence of ADR 0016's decision, that `bench-compare` fails when
  any benchmark's median p99.99 is more than 5 percent above the baseline's slowest
  run; the rest of ADR 0016 stands.

## Context

ADR 0016 moved the latency gate to the dedicated bench host, recorded baselines of 21
runs per benchmark from the host's own run, and compared every row. Within the hour
the gate failed twice on pull requests whose diffs held a glossary row and the ring
buffer modules, neither of which the harness runs. CI run 38258448325 read
`slot_padding_0_load` at 70 ns in 17 of its 21 runs against a baseline whose 21 runs
all read 61, and failed on +14.8 percent; CI run 38258561548 read
`slot_padding_0_store` at 60 against a baseline slowest of 50, and failed on +20
percent, with `slot_padding_0_load` back at 60. In both runs every row a message
passes through was at or under its baseline: `claim_commit_32_concurrent` 430,
`claim_commit_32_exclusive` 380, `one_way_32_concurrent` 670, `one_way_32_exclusive`
660. Three runs since the baseline read the two unpadded slots at 61 and 31, 70 and 50,
and 60 and 60.

The host's clock steps in 10 ns (ADR 0016, Evidence), and the `timer` and
`slot_padding_*` rows sit at 30 to 60 ns, where one step is 15 to 33 percent. Those
rows run no Cavalry code: `timer` is two clock reads, and each `slot_padding` row is one
thread loading a word while another stores to a word 0, 64 or 128 bytes away, the
probe ADR 0011 was decided on. A baseline band cannot hold a value that falls on one
of two adjacent steps at random, and widening it to both steps would only wait for the
third.

## Decision

`bench-compare` fails the gate on the rows a message passes through, the
`claim_commit_*` and `one_way_*` rows, by the rule of ADR 0016: a median p99.99 more
than 5 percent above the slowest of the baseline's runs. The `timer` and
`slot_padding_*` rows stay in the harness, in the table, in the result and in the
baseline, and are printed with their change against the baseline marked "reported":
they are read by a person, as the host's and the clock's state, and never fail a
commit.

## Alternatives considered

- **A floor at the clock's step, under which no change is a regression.** Lost: the
  harness cannot measure the step robustly, since the kernel's conversion of the
  counter to nanoseconds leaves residuals of 1 ns beside steps of 10, and a floor wide
  enough for the probes is three times the 5 percent rule on the message rows, which
  do not need it: at 380 to 670 ns, one step is 1.5 to 2.6 percent.
- **Re-recording the baseline with the wider band.** Lost: the next run falls on the
  other step again; the band of 21 runs in one process does not contain the band
  between processes on these rows.
- **Dropping the probe rows from the harness.** Lost: ADR 0011's evidence and the
  harness's own overhead belong in every table, and a doubling of `timer` would tell
  a reader the host changed before any message row did.
- **Gating the probes on max rather than p99.99.** Lost: max is one interrupt away
  from anything, which is why ADR 0015 left it out of the gate.

## Evidence

- CI run 38258448325, pull request 6 at 248ef60, one glossary row over the green
  head 8dcaa68: `slot_padding_0_load` p99.99 70 in 17 of 21 runs and 61 in 4,
  baseline 61 in all 21, +14.8 percent, REGRESSION; every other row at or under its
  baseline.
- CI run 38258561548, pull request 7 at 4cf4dfb, the ring buffer modules beside the
  log buffer, no change under `bench/`: `slot_padding_0_store` p99.99 60, runs 50 to
  60, baseline slowest 50, +20.0 percent, REGRESSION; `slot_padding_0_load` 60; the
  message rows 430, 380, 670 and 660 against baselines of 450, 430, 670 and 660.
- The four green comparisons before them, runs 38257554788, 38257882466 and
  38257968927, read the message rows at 430, 370 to 400, 660 to 670 and 660, and the
  probes on one step or the other.

## Consequences

Easier: a pull request that does not touch the data plane passes the gate, and a
reader still sees the probes move. Harder: the gate no longer notices a host drift
that moves only the probes; `bench/host/check.sh` guards the tuning, and a drift large
enough to matter moves the message rows too.

Revisit: when the harness has a cycle counter and the probes resolve below a clock
step; when a probe row is wanted as a guard with a rule of its own, such as failing on
a doubling of `timer`.

## References

- `scripts/bench-compare.py`, `GATED_ROWS`; `docs/how-to/run-the-bench.md`
- ADR 0011, ADR 0015, ADR 0016
- AppShapes Trello card 83, "Build the log buffer, the ring buffers and the bench"
