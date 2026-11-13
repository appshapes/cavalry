- The durable log's format is version 3: the log recordings file is new, and no
  other file changes (`spec/durable-log.md`, "Log recordings").
- `cavalry-log` reads and writes the log recordings file, `log-recordings` in a
  cluster member's log directory: the list of the recordings that hold the member's
  log, kept in two slots so that a write a crash tore reads as the list before it
  (`recordings::RecordingsFile`). No member writes the file yet, and `cavalry run`
  still refuses a log directory that holds a term: the change that follows starts a
  member on the log directory it kept
  ([ADR 0108](docs/decisions/0108-list-the-recordings-of-a-members-log-in-its-log-directory-and-start-a-member-on-the-log-it-kept.md)).
