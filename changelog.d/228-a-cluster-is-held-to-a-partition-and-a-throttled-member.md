- A cluster's fault injection runs with `just test`: a member cut off from the others
  through a relay that loses and reorders datagrams, and a member throttled to a
  twentieth of a processor (`crates/cavalry-raft/tests/partition.rs` and
  `tests/throttle.rs`). In both the cluster commits while a quorum remains and every
  member's log holds every committed entry afterwards. What they measured on a
  laptop: a member cut off for 10 s or more joins again about 10 s after the cut is
  mended, the image cool-down of [`spec/udp.md`](spec/udp.md); and one throttled
  follower, stopped for 95 ms of every 100 ms as the test throttles on macOS, slows
  the cluster's commits to about 12,000 entries of 96 bytes a second, one receiver
  window each 100 ms, because the leader's log is sent under `fc=min`
  ([ADR 0052](docs/decisions/0052-build-raft-on-the-durable-log-fifth.md);
  [ADR 0111](docs/decisions/0111-cut-a-cluster-member-off-through-a-relay-in-the-test-and-throttle-one-as-a-process.md)).
