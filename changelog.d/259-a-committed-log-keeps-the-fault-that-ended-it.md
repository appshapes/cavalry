- A committed log keeps the fault that ended it. When a service's read of its replay
  stops on a fault of the replay's image, the poll that meets it still returns the
  fault once, and `CommittedLog::faulted()` now answers it for as long as the handle
  is held, so a service that polled on can tell this end from a whole one. The C ABI
  is at version 3 with `cavalry_committed_log_faulted`, and the Python, Node, Go,
  .NET and JVM clients each have the read on their committed log
  ([read the committed log](docs/how-to/read-the-committed-log.md);
  [ADR 0080](docs/decisions/0080-keep-the-fault-that-ended-a-committed-log-and-take-the-c-abi-to-version-3.md);
  card 259).
