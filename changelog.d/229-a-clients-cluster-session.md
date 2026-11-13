- A Rust client holds a session with a cluster: `Cavalry::connect_cluster(members,
  response_channel)` asks the members in turn, follows a redirect to the leader and
  returns a `ClusterSession`, or a `ClusterError` that says what to do.
  `ClusterSession::offer` sends the leader a message of one frame, and
  `ClusterSession::poll` hands over the cluster's replies with the cluster's time,
  sends the session's keepalive every 500 ms, follows the cluster to a new leader and
  says when the cluster closed the session, or when the session's leader was lost
  and no member took its place in 20 s. Call `poll` at least every 500 ms. The
  session's calls for the C ABI and the five other languages come with the next
  change ([ADR 0117](docs/decisions/0117-open-a-cluster-session-from-the-client-with-a-publication-to-every-member.md);
  work item 229).
