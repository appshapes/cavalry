- The consensus module's I/O can truncate a member's log, for the election a member
  over a driver does not hold yet: it purges the recordings that begin above the
  position, truncates the ones that stopped above it, and takes the terms based
  above it out of the `terms` file. It was the last operation of the election the
  module refused. A member started with `cavalry run` is unchanged: its leader is
  appointed and asks for none of it
  ([ADR 0095](docs/decisions/0095-truncate-a-members-log-over-a-driver-recording-by-recording-then-its-record-of-terms.md)).
