- The durable log is at version 5: a recording's time index keeps the position its
  recorder last synced, and when the driver starts after a crash of any kind, a power
  loss included, it cuts each recording that was being made to that position and
  zeroes what lay past it, so that no frame a power loss left without its body is read
  back or replicated. `recording.cut_to_synced` counts each recording cut, and
  `cavalry doctor` explains it. A cluster member whose host lost power starts again on
  its log directory. A directory an earlier version wrote opens in place
  ([ADR 0144](docs/decisions/0144-keep-a-recordings-synced-position-in-its-time-index-and-cut-a-recording-a-crash-left-live-to-it.md)).
