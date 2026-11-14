- A Java program holds a session with a cluster and runs a cluster's service.
  `client.connectCluster(members, responseChannel)` returns a `ClusterSession`:
  `offer` sends the leader a message, and `poll` hands a `ReplyHandler` each reply with
  its leadership term and the cluster's time. `client.clusterService()` returns a
  `ClusterService`: `poll` hands a `ServiceHandler` each committed entry with its
  values, the handler answers a session with `service.reply`, and what it returns,
  `Abi.ACTION_CONTINUE`, `Abi.ACTION_BREAK` or `Abi.ACTION_ABORT`, says what becomes of
  the entry. A service's handler that throws, or returns any other value, has its entry
  aborted: the poll ends at it, throws `HandlerException`, and the next poll hands the
  entry over again. A service's `poll` is a call on its client and takes no lock, so
  that client is one thread's from `clusterService()` to the service's `close()`.
  `just conformance` runs the cluster round through the JVM client. The .NET client
  binds the same functions and has no session object or service object yet
  ([ADR 0124](docs/decisions/0124-give-each-client-a-session-object-and-a-service-object-whose-handlers-failure-aborts-its-entry.md);
  [the JVM client](clients/jvm/README.md); work item 229).
