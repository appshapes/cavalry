- `cavalry doctor` now names a cluster leader beside which no service reads the
  log. Where the leader holds timers, the program beside it has stopped, and doctor
  prints `WARN cluster.timers` with "no service reads its log on this host": the
  timers of the program beside every other member wait until that program runs
  again. Where it holds none, doctor prints `note cluster-no-service`, which a
  cluster that runs no program yet may pass over. `spec/cluster.md` had said that
  doctor reports this, and doctor had no such line
  ([ADR 0166](docs/decisions/0166-have-doctor-name-a-leader-beside-which-no-service-reads-the-log.md)).
