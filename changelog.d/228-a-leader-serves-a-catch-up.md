- A cluster leader made with `cavalry_raft::Consensus::with_election` answers a
  member that is behind within the leader's own term: a catch-up position from the
  leader's term base to its append position starts a replay of the leader's live
  recording, which follows the recording until the leader stops it. The leader stops
  it, and sends stop catch-up, when the member reports from the replay an append
  position at or past the leader's own. One counter follows, `cluster.catchups`, and
  `cavalry_raft::Io::start_replay` takes an end that may be none. No member asks for
  a catch-up yet, and a member started with `cavalry run` is unchanged: its leader
  is appointed, and the counter stays 0
  ([ADR 0087](docs/decisions/0087-serve-a-catch-up-from-the-leaders-core-and-stop-it-at-the-leaders-own-append-position.md)).
