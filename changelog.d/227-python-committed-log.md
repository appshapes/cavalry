- The Python client reads a cluster's committed log. `client.committed_log(position)`
  returns a `CommittedLog`, or `None` while the driver hosts no cluster member with a
  log; `log.poll(handler, fragment_limit)` calls `handler(body, header, position)` for
  each entry a quorum has committed, none past the commit position; `log.position()`
  is where a service opens the log again; and once `log.is_ended` is true,
  `log.failed_at()` and `log.withdrawn_at()` say whether the log ended short of the
  entries it was owed, and where. A handler's exception is raised after the poll, as
  a subscription's is, and its entry is not delivered again. The Python runner starts
  a cluster of three members and reads its log
  ([ADR 0070](docs/decisions/0070-take-the-c-abi-to-version-2-with-the-committed-log.md);
  card 227).
