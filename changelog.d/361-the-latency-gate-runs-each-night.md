- The latency gate no longer paces a change. No pull request and no push takes a bench
  host: the gate runs each night at 07:17 UTC on `master`, and on demand for a branch
  with `gh workflow run bench.yml -f branch=<name>`. It fails a row whose p99.99 is
  more than 50 percent, and more than one step of the clock, above the slowest run
  recorded, where it failed at 5 percent; it is at 5 percent again when the slice that
  measures Cavalry against Aeron arms it. The allocation tests, the harness's own
  refusal of an allocation in a measured loop, and the lints still run for every pull
  request. A commit message and a pull request body carry no histograms: a change that
  adds work per message says what and why in one sentence. `scripts/bench-needed.py`,
  the `changes` job and `ci.yml`'s bench jobs are gone, and branch protection on
  `master` requires `gates (linux)` alone. A run of the workflow by hand is a report:
  it prints its comparisons and does not fail on them. A result names the commit it
  measured, which the workflow gives the harness in `CAVALRY_BENCH_GIT`. `/next-task`
  reads the scheduled runs, where two red nights in a row on one row are a task, and
  takes the three chains of ADR 0138 as the slice's order
  ([ADR 0139](docs/decisions/0139-run-the-latency-gate-each-night-as-a-coarse-check.md);
  [Run the bench](docs/how-to/run-the-bench.md); work item 361).
