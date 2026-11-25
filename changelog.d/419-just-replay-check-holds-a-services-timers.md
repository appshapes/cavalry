- `just replay-check` holds a cluster service's timers. After the cluster it already
  checked, three more members on the loopback each run a program beside them that
  schedules a timer at each message, 200 in all; one program starts again from its
  snapshot, and the leader stops with its service while half the timers are pending.
  The gate fails unless the two members left and a program that reads one member's
  log from position 0, as a replay does, were each handed the same 200 timers, each
  once, at the same positions and times. It prints a `timers <program> <count>
  <hash>` line for each of the three. `examples/cluster-timers-check.rs` in `cavalry`
  is its program
  ([ADR 0151](docs/decisions/0151-give-a-clusters-service-timers-that-every-service-decides-from-the-log.md);
  [Run a cluster](docs/how-to/run-a-cluster.md); work item 419).
