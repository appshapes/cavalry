- The .NET client reads a cluster's committed log. `client.CommittedLog(position)`
  returns a `CommittedLog`, or `null` while the driver hosts no cluster member with a
  log; `log.Poll(handler, fragmentLimit)` calls the `EntryHandler`, `handler(body,
  header, position)`, for each entry a quorum has committed, none past the commit
  position, and returns the count, or `-1` when the poll stopped on a fault, which
  `log.GetFault()` reads until the next poll; `log.Position()` is where a service
  opens the log again. Once `log.IsEnded()` is true, the log delivered every entry it
  was owed only when `log.FailedAt()` and `log.WithdrawnAt()` are both `null` and no
  poll returned `-1` with the fault of the log's own image, `Abi.ERR_FAULT_IMAGE` or
  `Abi.ERR_FAULT_IMAGES_FULL`: that fault is reported once, so a service keeps it. A
  handler's exception is thrown after the poll as `HandlerException`, as a
  subscription's is, and its entry is not delivered again. The .NET runner starts a
  cluster of three members and reads its log
  ([ADR 0070](docs/decisions/0070-take-the-c-abi-to-version-2-with-the-committed-log.md);
  card 227).
