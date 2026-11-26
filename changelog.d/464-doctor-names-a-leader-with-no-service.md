- `cavalry doctor` now names a cluster leader beside which no service runs, as
  `cluster-no-service`. Where the leader holds timers, the program beside it has
  stopped, and the line is a warning: where the program runs beside the other
  members, its timers wait until the program beside the leader runs again. Where
  the leader holds none, the line is a note, which a cluster that runs no program
  yet may pass over. `spec/cluster.md` had said that doctor reports this, and doctor
  had no such line
  ([ADR 0167](docs/decisions/0167-have-doctor-name-a-leader-beside-which-no-service-runs.md)).
