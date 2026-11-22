- `just conformance` holds that a log directory written by an older build opens under
  this one in place. `conformance/upgrade/log-directory.sh` builds the tree that
  Brigade's servers run, 620a2e29, has it write a log directory as a server writes its
  own, with two purges in it, once stopped and once killed, and then holds that this
  tree's `cavalry run` starts on it, that every record is read back at its position,
  that what was purged stays gone, and that the directory takes a new recording, in the
  server's shape and in the one it moves to. It is the rule for every change of the
  durable log's format: a driver opens a directory of the format before it with no step
  by an operator
  ([ADR 0141](docs/decisions/0141-open-in-place-a-log-directory-written-by-the-tree-that-servers-run-and-build-that-tree-on-every-run.md);
  [conformance/README.md](conformance/README.md), "The log directory's upgrade gate";
  work item 362).
