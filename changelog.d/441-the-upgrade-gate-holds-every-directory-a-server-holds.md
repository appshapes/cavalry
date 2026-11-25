- `just conformance` holds every log directory that Brigade's servers hold or may hold,
  since both servers moved from Brigade's server 0.9.0 to 0.10.0 on 2026-11-24.
  `conformance/upgrade/log-directory.sh` builds both trees the servers have run,
  620a2e29 and d265af82, where it built 620a2e29 alone. It has them write three
  directories, each in its server's shape: 620a2e29's alone, 620a2e29's and then
  d265af82's in place, and d265af82's alone. It writes each with its last run stopped
  and again killed, and the one written in turn a third time, with 620a2e29's last run
  killed. This tree then opens each in place and reads back every record left
  ([ADR 0156](docs/decisions/0156-hold-in-the-upgrade-gate-every-directory-a-server-holds-each-trees-alone-and-the-trees-in-turn.md);
  [conformance/README.md](conformance/README.md), "The log directory's upgrade gate";
  work item 441). On CI's macOS runner alone, the gate writes server 0.9.0's directory
  alone, as before: with all seven, that runner was lost in 2 of 8 jobs, and with one
  tree in 2 of 39
  ([ADR 0160](docs/decisions/0160-keep-the-log-directorys-gate-on-one-tree-on-cis-macos-runner-and-track-the-lost-runner-as-a-fault-of-its-own.md);
  work item 456).
