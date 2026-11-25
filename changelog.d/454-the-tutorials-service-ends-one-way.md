- A cluster's service now ends one way when its member's driver stops.
  `ClusterService::poll` of a closed client returns one error,
  `ClusterError::Client(ClientError::Closed(reason))`, and
  `cavalry_cluster_service_poll` −1 with `CAVALRY_ERR_CLOSED`, at every poll. Before,
  the poll gave that error, a fault of the log's read (`CAVALRY_ERR_FAULT_CLOSED`),
  the command ring's refusal, or 0 for as long as it was called, by what the service
  held and when it polled. A command that tells its caller how it went and finds
  the driver gone on the command ring now closes the client and returns
  `ClientError::Closed`, where it returned `ClientError::CommandRing` until the
  client's conductor saw the same. A service's close at a client that is closed is
  no error, as a publication's is none. The tutorial's `adder-service` ends on the one
  error with the line the tutorial shows, where `just quickstart` failed now and
  then for the other endings, and the Python service of
  `docs/how-to/take-a-services-snapshot.md` ends on a poll that failed, where it
  polled a stopped driver for ever
  ([ADR 0155](docs/decisions/0155-give-a-poll-of-a-closed-client-one-error-and-close-the-client-at-a-command-that-finds-the-driver-gone.md)).
