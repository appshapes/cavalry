- The latency gate's baseline for the link between the two bench hosts,
  `bench-linux-link`, is re-recorded from the three link processes at `master`'s tree
  on 2026-11-15, after `master`'s own run there failed `udp_round_trip_32` twice with
  no line of the link rows changed. The row's tail had risen on the hosts: its runs
  read 190,207 to 244,863 ns where the entry of 2026-11-04 held 188,671 to 190,591,
  with the median where it was. The gate now fails that row above 257,106 ns, where
  it failed it above 200,120 ns; what widened the tail is not found and is with the
  bench hosts' other effects. `scripts/bench-compare.py --update` now writes a row
  that the harness runs once a process, as the link pass's two `_udp_mdc` rows are,
  with no runs list, where it refused the link's result files
  ([Run the bench](docs/how-to/run-the-bench.md), steps 4 and 5;
  [ADR 0033](docs/decisions/0033-record-a-bench-baseline-from-every-process-at-one-tree.md);
  work items 316 and 198).
