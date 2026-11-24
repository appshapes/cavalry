- A cluster member's consensus module holds its log directory for its life, and a
  second module on it, of any member id, is refused with `log-directory-held` and
  writes nothing. A member whose process was killed holds nothing and starts again at
  once. On Linux, `cavalry doctor --log-dir` names the pid of the module that holds
  the directory. A member's `terms` file now syncs the log directory when it is
  created, as its other two files do
  ([ADR 0152](docs/decisions/0152-sync-the-log-directory-when-a-members-terms-file-is-created-and-hold-the-directory-for-the-consensus-modules-life.md)).
