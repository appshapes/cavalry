- A Rust program runs a cluster's service: `Cavalry::cluster_service()` returns a
  `ClusterService` beside the cluster member on the client's driver, and
  `ClusterService::poll` hands over each committed entry, a session's message, a
  session's open or close, or a term's first entry, with its position, its term and
  the cluster's time. The handler answers a session with `Replies::reply`, from the
  session's own open event on: a reply that reaches a client before its
  `connect_cluster` returns is kept, and the session's polls hand it over. Every
  member's service runs the same handler, and the one beside the member that led the
  entry's term sends, while that member leads the term still: a member that has
  stopped leading it has a service that sends nothing for it, so a reply of an
  earlier term does not come long after the next leader's. A reply a client does not take is tried again for 500 ms and
  then dropped, counted in the new counter `cluster.replies_dropped`, so one client
  that does not read delays the others once and never stops the service. The
  service's calls for the C ABI and the five other languages come with the next
  change ([ADR 0118](docs/decisions/0118-reply-to-a-session-from-the-service-beside-the-entrys-leader-and-wait-for-no-client-past-the-keepalive-interval.md);
  work item 229).
