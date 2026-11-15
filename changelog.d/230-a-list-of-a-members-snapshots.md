- The durable log's format is version 4: the snapshots file is new, and no other
  file changes (`spec/durable-log.md`, "Snapshots").
- `cavalry-log` reads and writes the snapshots file, `snapshots` in a cluster
  member's log directory: the list of the snapshots a start may begin from, each
  with its recording, its log position, its term and whose state it is, kept in two
  slots so that a write a crash tore reads as the list before it
  (`snapshots::SnapshotsFile`). No member takes a snapshot yet or writes the file:
  the changes that follow in work item 230 do
  ([ADR 0128](docs/decisions/0128-list-a-members-snapshots-in-a-file-of-their-own-in-its-log-directory.md)).
