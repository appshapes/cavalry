- The cluster protocol is at version 2, which says how a cluster's service schedules
  and cancels a timer and how the timer's event reaches every member's service once,
  at one position. A member of this build writes version 2, which `cavalry doctor`
  names as `it runs version 2 of the cluster protocol, 0.2.0`, and forms one cluster
  with members of version 1. No service has the calls yet
  ([the cluster protocol](docs/reference/cluster.md);
  [ADR 0151](docs/decisions/0151-give-a-clusters-service-timers-that-every-service-decides-from-the-log.md)).
