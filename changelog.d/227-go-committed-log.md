- The Go client reads a cluster's committed log. `client.CommittedLog(position)`
  returns a `*CommittedLog`, or `nil` and no error while the driver hosts no cluster
  member with a log; `log.Poll(handler, fragmentLimit)` calls `handler(body, header,
  position)` for each entry a quorum has committed, none past the commit position;
  `log.Position()` is where a service opens the log again; and once `log.IsEnded()`
  is true, `log.FailedAt()` and `log.WithdrawnAt()` say whether the log ended short of
  the entries it was owed, and where. A handler's panic is returned as a
  `*HandlerError` after the poll, as a subscription's is, and its entry is not
  delivered again. An open log holds one slot of the table of 4,096. The Go runner
  starts a cluster of three members and reads its log
  ([ADR 0070](docs/decisions/0070-take-the-c-abi-to-version-2-with-the-committed-log.md);
  card 227).
