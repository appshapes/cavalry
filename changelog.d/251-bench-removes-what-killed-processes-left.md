- `just bench` removes, before its first row, the directories that bench processes
  killed before their end left under `/dev/shm`, and says how many: a CI job cancelled
  by a newer push left its rows' files there, 11 G on one gate host and nearly half
  of the other's `/dev/shm`, and nothing removed them. An entry of a process that
  still runs, and an entry of another user's, is never removed.
  `bench/host/check.sh` prints how full `/dev/shm` is
  ([Run the bench](docs/how-to/run-the-bench.md#when-it-does-not-work); card 251).
