- A Node program holds a session with a cluster and runs a cluster's service.
  `client.connectCluster(members, responseChannel)` returns a `ClusterSession`:
  `offer` sends the leader a message, and `poll` hands a handler each reply with its
  leadership term and the cluster's time. `client.clusterService()` returns a
  `ClusterService`: `poll` hands a handler each committed entry with its values, the
  handler answers a session with `service.reply`, and what it returns, nothing,
  `cavalry.CONTINUE`, `cavalry.BREAK` or `cavalry.ABORT`, says what becomes of the
  entry. A service's handler that throws, or returns anything else, has its entry
  aborted: the poll ends at it, throws `HandlerError`, and the next poll hands the
  entry over again. An `async` handler returns a promise, which is no action, so write
  the handler as a plain function. `just conformance` runs the cluster round through
  the Node client
  ([ADR 0124](docs/decisions/0124-give-each-client-a-session-object-and-a-service-object-whose-handlers-failure-aborts-its-entry.md);
  [the Node client](clients/node/README.md); work item 229).
