- The cluster tutorial's service, `adder-service`, now ends with one line when its
  member's driver stops, the line the tutorial shows, with the total and the
  position it read to. Before, a stop that the service met inside a poll ended it
  with the client's own error, which says neither, and `just quickstart` failed for
  it now and then. `ClusterService::poll` and `cavalry_cluster_service_poll` now say
  when a close of the client is an error of a poll and when it is not, and that a
  program's loop treats the two as one close.
