- The Python client reads a cluster's committed log. `client.committed_log(position)`
  returns a `CommittedLog`, or `None` while the driver hosts no cluster member with a
  log; `log.poll(handler, fragment_limit)` calls `handler(body, header, position)` for
  each entry a quorum has committed, none past the commit position, and returns the
  count, or `-1` when the poll stopped on a fault, which `log.fault()` reads until
  the next poll; `log.position()` is where a service opens the log again. Once
  `log.is_ended` is true, the log delivered every entry it was owed only when
  `log.failed_at()` and `log.withdrawn_at()` are both `None` and no poll returned
  `-1` with the fault of the log's own image, `CAVALRY_ERR_FAULT_IMAGE` or
  `CAVALRY_ERR_FAULT_IMAGES_FULL`: that fault is reported once, so a service keeps
  it. A handler's exception is raised after the poll, as a subscription's is, and its
  entry is not delivered again. The Python runner starts a cluster of three members
  and reads its log
  ([ADR 0070](docs/decisions/0070-take-the-c-abi-to-version-2-with-the-committed-log.md);
  card 227).
