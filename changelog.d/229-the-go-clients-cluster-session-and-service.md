- A Go program holds a session with a cluster and runs a cluster's service.
  `client.ConnectCluster(members, responseChannel)` returns a `*ClusterSession`:
  `Offer` sends the leader a message, and `Poll` hands a handler each reply with its
  leadership term and the cluster's time. `client.ClusterService()` returns a
  `*ClusterService`: `Poll` hands a handler each committed entry with its values, the
  handler answers a session with `service.Reply`, and what it returns,
  `ActionContinue`, `ActionBreak` or `ActionAbort`, says what becomes of the entry. A
  service's handler that panics, or returns any other value, has its entry aborted:
  the poll ends at it, returns a `*HandlerError`, and the next poll hands the entry
  over again. A service's `Poll` is a call on its client and takes no lock, so that
  client is one goroutine's from `ClusterService` to the service's `Close`. `just
  conformance` runs the cluster round through the Go client
  ([ADR 0124](docs/decisions/0124-give-each-client-a-session-object-and-a-service-object-whose-handlers-failure-aborts-its-entry.md);
  [the Go client](clients/go/README.md); work item 229).
