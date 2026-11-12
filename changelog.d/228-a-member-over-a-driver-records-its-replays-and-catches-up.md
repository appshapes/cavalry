- The consensus module's I/O can record the replays a member asks its leader for
  and catch up, for the election a member over a driver does not hold yet: it
  subscribes on the member's `catchup` endpoint with a recording armed for each
  replay, sends catch-up position with the member's own endpoint, and counts the
  member's append position across the recordings it stopped, its recording of a
  replay and its recording of the live log. A member started with `cavalry run` is
  unchanged: its leader is appointed and asks for none of it
  ([ADR 0094](docs/decisions/0094-record-a-members-replays-over-a-driver-and-count-the-live-recording-once-the-log-reaches-it.md)).
