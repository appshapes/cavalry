- A cluster member made with `cavalry_raft::Consensus::with_election` that
  replicates a term it missed now asks its leader for each replay: when a replay's
  image has gone and the member's log ends further on, it subscribes again and sends
  catch-up position from there, so that a recording is armed for every image. A
  leader starts no replay by itself after one has stopped. `cluster.log_replications`
  rises on the member once for each replay it asks for.
  `memory::Cluster::end_recording` ends a member's recording where its log stands,
  for a test of a term that lies in two recordings. No member started with
  `cavalry run` elects yet, so none replicates
  ([ADR 0092](docs/decisions/0092-have-the-member-ask-for-each-replay-of-a-replication-so-that-a-recording-is-armed-for-its-image.md)).
