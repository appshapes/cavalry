- `just bench` reads a shared-memory message benchmark's claim row with its subscriber
  held behind the publisher. Each such benchmark, the two log types, `client`,
  `recorded`, `recorded_purging` and `ffi`, now runs `--messages` twice in one loop:
  first with the subscriber at the publisher's tail, where the `one_way` row is read
  and the claim is reported as a new row, `claim_commit_<body>_<name>_at_tail`, never
  gated; then with the subscriber staying away from the log for as long as the
  publisher takes to claim 1,000 messages, where the gated `claim_commit` row is
  read. A subscriber at the tail reads the cache line the publisher writes next, and
  how many claims waited for that line followed a build's layout and a process's
  address space, so a claim row's p99.99 moved by a tenth or more with no instruction
  changed. The result carries each benchmark's meetings with the tail as `held_polls`
  ([ADR 0127](docs/decisions/0127-measure-a-claim-row-with-its-subscriber-held-behind-the-publisher.md); work item 316).
