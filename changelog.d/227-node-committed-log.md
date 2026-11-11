- The Node client reads a cluster's committed log. `client.committedLog(position)`
  returns a `CommittedLog`, or `null` while the driver hosts no cluster member with a
  log; `log.poll(handler, fragmentLimit)` calls `handler(body, header, position)` for
  each entry a quorum has committed, none past the commit position; `log.position()`
  is where a service opens the log again; and once `log.isEnded` is true,
  `log.failedAt()` and `log.withdrawnAt()` say whether the log ended short of the
  entries it was owed, and where. A handler's exception is thrown after the poll as
  `HandlerError`, as a subscription's is, and its entry is not delivered again. The
  Node runner starts a cluster of three members and reads its log
  ([ADR 0070](docs/decisions/0070-take-the-c-abi-to-version-2-with-the-committed-log.md);
  card 227).
