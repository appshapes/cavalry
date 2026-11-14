- A .NET program holds a session with a cluster and runs a cluster's service.
  `client.ConnectCluster(members, responseChannel)` returns a `ClusterSession`:
  `Offer` sends the leader a message, and `Poll` hands a `ReplyHandler` each reply with
  its leadership term and the cluster's time. `client.ClusterService()` returns a
  `ClusterService`: `Poll` hands a `ServiceHandler` each committed entry with its
  values, the handler answers a session with `service.Reply`, and what it returns,
  `Abi.ACTION_CONTINUE`, `Abi.ACTION_BREAK` or `Abi.ACTION_ABORT`, says what becomes of
  the entry. A service's handler that throws, or returns any other value, has its entry
  aborted: the poll ends at it, throws `HandlerException`, and the next poll hands the
  entry over again. A service's `Poll` is a call on its client and takes no lock, so
  that client is one thread's from `ClusterService()` to the service's `Close()`.
  `just conformance` runs the cluster round through the .NET client, the last of the
  five: every generated client now has a session object and a service object
  ([ADR 0124](docs/decisions/0124-give-each-client-a-session-object-and-a-service-object-whose-handlers-failure-aborts-its-entry.md);
  [the .NET client](clients/dotnet/README.md); work item 229).
