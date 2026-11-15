- The latency gate's baselines for the two bench hosts are re-recorded on the harness
  that reads a claim row with its subscriber held behind the publisher: the profiles
  `bench-linux`, `bench-linux-unchecked`, `grip-linux` and `grip-linux-unchecked` in
  `bench/baseline.json`, each from the eight processes its host ran at one tree on
  2026-11-14 and 2026-11-15. The gate compares a row's p99.99 with the slowest run in
  its baseline, and on `grip` that run is now 95 ns and 117 ns on the checked
  `claim_commit_32_client` and `claim_commit_32_recorded` rows, where 139 ns and
  160 ns stood since that profile was widened for one pull request, and 5,827 ns on
  `one_way_32_udp`, where 20,639 ns stood. Of the 48 gated rows, 19 are held to a
  slowest run more than 5 percent below the one that stood and three, all on
  `bench`'s checked profile, to one more than 5 percent above it, each named with its
  process in the commit. Each profile also holds the five `_at_tail` rows, which are
  reported and never gated. The link's profile, `bench-linux-link`, is as it was
  ([Run the bench](docs/how-to/run-the-bench.md), step 4;
  [ADR 0127](docs/decisions/0127-measure-a-claim-row-with-its-subscriber-held-behind-the-publisher.md);
  work item 316).
