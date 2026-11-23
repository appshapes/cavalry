- A cluster member whose truncation of its log faults part way, on a driver's refusal,
  a command not answered in time or a failed write, now owes that truncation. It asks
  for it again at each duty cycle, and until it finishes it neither leads nor joins a
  log, so it no longer stands in an election on an end it no longer holds or lists a
  recording past the cut that its next start would refuse (`listed-recording`).
  `cluster.truncations_faulted` counts such truncations, `cluster.truncation_owed`
  gives the position while one is owed, and `cavalry doctor` names the member
  ([ADR 0148](docs/decisions/0148-keep-a-member-whose-truncation-faulted-out-of-the-log-until-it-finishes-the-truncation.md);
  [Run a cluster](docs/how-to/run-a-cluster.md); work item 277).
