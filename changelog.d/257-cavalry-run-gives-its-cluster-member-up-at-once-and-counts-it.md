- `cavalry run` now stops at once when the thread of its cluster member panics.
  Before, the member was gone and the process ran on as a driver in no cluster until
  a signal. It says so in the line `cavalry run: the cluster member stopped, so the
  driver stops`, closes the driver and exits 1, as it does after 10 s of duty cycles
  that fail ([Run a cluster](docs/how-to/run-a-cluster.md#when-it-does-not-work); card 257).
- `cavalry doctor` now says why such a driver closed. The new counter
  `run.member_given_up` counts each time `cavalry run` gives its cluster member up. It
  is one of the driver's own counters, so it is in the directory the closed driver
  leaves, and `doctor` reports it there as `WARN run.member_given_up` with what to do
  ([Counters](docs/reference/counters.md)). Its id is 148, and a static counter is in
  the record of its id, so `cavalry_shm::counters::CountersFile::create` now refuses a
  capacity of 64 or 128 with `full`; a driver's own file has 16,384 records.
- `cavalry doctor` now looks up each host name of `CAVALRY_CLUSTER_MEMBERS` once. A
  name that resolves to no address is one `FAIL cluster-members` finding that names
  every endpoint the host has, where each of a member's five endpoints was looked up
  and reported by itself.
- `cavalry::Idler` is new: an idle strategy with the state of its backoff, the one
  each thread of the driver holds, for a duty cycle a program runs beside the driver.
  The thread of `cavalry run`'s cluster member idles with it, so the backoff's figures
  are written once.
