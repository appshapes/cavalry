- A cluster member serves sessions: `cavalry run` in a member's role, and
  `ConsensusModule` in a program, read the ingress stream on the member's `ingress`
  endpoint. A follower answers a session connect with a redirect to its leader; the
  leader opens the session, appends its messages to the log with its own term and
  the cluster's time, closes it when its client is silent for 10 s, and a member
  that replaces a lost leader has the session from its log and sends the client the
  new leader event. The member's `cluster.sessions` gauge and its seven session
  counters are in its driver's counters file. A message of a session is one frame,
  1,344 bytes of payload at most. A members string whose ingress endpoints are
  longer than 1,320 bytes as a redirect lists them is refused when the member
  starts. The member's consensus module reads its own log for its sessions on a
  stream of its own, the log replay stream, 103: `Cavalry::committed_log_on(stream_id,
  position)` is `Cavalry::committed_log` on a stream the caller names, and
  `LOG_REPLAY_STREAM_ID` names that one. No client of Cavalry's own speaks the
  protocol yet, and a service does not
  reply to a session yet. `spec/cluster.md` stays at version 0: no field of a
  message changes
  ([ADR 0116](docs/decisions/0116-read-a-members-sessions-from-its-committed-log-and-serve-them-from-its-terms-base.md);
  work item 229).
