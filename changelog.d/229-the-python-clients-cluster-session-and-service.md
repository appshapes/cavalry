- A Python program holds a session with a cluster and runs a cluster's service.
  `client.connect_cluster(members, response_channel)` returns a `ClusterSession`:
  `offer` sends the leader a message, and `poll` hands a handler each reply with its
  leadership term and the cluster's time. `client.cluster_service()` returns a
  `ClusterService`: `poll` hands a handler each committed entry with its values, the
  handler answers a session with `service.reply`, and what it returns, nothing,
  `cavalry.CONTINUE`, `cavalry.BREAK` or `cavalry.ABORT`, says what becomes of the
  entry. A service's handler that raises has its entry aborted: the poll ends at it,
  raises `HandlerError`, and the next poll hands the entry over again, so a service
  never passes an entry it did not apply. `just conformance` runs the cluster round
  through the Python client. The Node, Go, JVM and .NET clients bind the same
  functions and have no session object or service object yet
  ([ADR 0124](docs/decisions/0124-give-each-client-a-session-object-and-a-service-object-whose-handlers-failure-aborts-its-entry.md);
  [the Python client](clients/python/README.md); work item 229).
