- The latency gate's baselines for the two bench hosts are re-recorded on the harness
  that keeps a subscriber's samples out of the histogram until its loop is over: the
  profiles `bench-linux`, `bench-linux-unchecked`, `grip-linux` and
  `grip-linux-unchecked` in `bench/baseline.json`, each from the eight processes its
  host ran at one tree on 2026-11-12. The gate compares a row's p99.99 with the slowest
  run in its baseline, and on the one-way rows that run is now the path's and not a
  cache miss in the harness: 494 ns and 498 ns on `grip`'s `one_way_32_client` and
  `one_way_32_recorded` rows, where 1,117 ns and 1,152 ns stood, and 520 ns on
  `bench`'s `one_way_32_recorded` row, where 730 ns stood. Of the 48 gated rows, 21
  are held to a slowest run more than 5 percent below the one that stood and six to
  one more than 5 percent above it, each named with its process in the commit. The
  link's profile, `bench-linux-link`, is as it was
  ([Run the bench](docs/how-to/run-the-bench.md), step 4;
  [ADR 0090](docs/decisions/0090-keep-a-subscribers-samples-out-of-the-histogram-until-its-loop-is-over.md);
  card 198).
