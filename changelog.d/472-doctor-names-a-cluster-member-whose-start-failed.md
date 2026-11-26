- `cavalry doctor` names a cluster member whose start failed under `cavalry run`:
  `run.member_refused` counts it in the driver's counters file before `cavalry run`
  closes the driver, and `doctor` reports `WARN run.member_refused` from the directory
  the closed driver left, with where the reason was printed and what to do.
  [Run a cluster of three members](docs/how-to/run-a-cluster.md), "When it does not
  work".
