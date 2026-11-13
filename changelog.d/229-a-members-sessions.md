- `cavalry-raft` has a cluster member's sessions, the member's side of the session
  protocol: `Sessions` answers a session connect with a redirect on a follower, and on
  the leader refuses it or accepts it, appends a session's messages to the log, closes
  a session that is silent for 10 s, and tells every open session of a new term. A
  session is opened and closed by committed entries of the log on every member, so a
  new leader has the sessions of the leader before it. Eight counters are new in
  `counters.toml`: the gauge `cluster.sessions`, and `cluster.sessions_accepted`,
  `cluster.sessions_opened`, `cluster.sessions_closed`, `cluster.sessions_timed_out`,
  `cluster.sessions_refused`, `cluster.connects_dropped` and `cluster.ingress_ignored`.
  No member reads the ingress stream yet: the consensus module drives `Sessions` in
  the next change, and a client has nothing to connect to until then.
  `spec/cluster.md` stays at version 0: no field of a message changes
  ([ADR 0115](docs/decisions/0115-keep-a-clusters-sessions-on-every-member-opened-and-closed-by-the-log.md);
  work item 229).
