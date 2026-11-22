# Ask a cluster for a snapshot

A how-to page: one task, have a running cluster take a snapshot
([glossary](../glossary.md)) now. A leader asks for one by itself each time its log
has grown by 256 MiB. `cavalry snapshot` asks for one at once: before an upgrade,
before a purge of the log, or in a test. What the
command writes and what the leader does with it are
[`spec/cluster.md`](../reference/cluster.md), "Snapshots", "The control toggle"; why
it is a counter that the command owns is
[ADR 0147](../decisions/0147-ask-a-member-for-a-snapshot-with-a-control-toggle-that-the-asking-command-owns.md).
In Aeron, this is `ClusterTool snapshot` ([glossary](../glossary.md)).

## Prerequisites

- A cluster that runs, started as [Run a cluster of three members](run-a-cluster.md)
  starts one. This page uses that page's three members on one host, with their
  directories `/tmp/cavalry-member-0`, `-1` and `-2`.
- A terminal on the host of the member that leads. The command asks the member on
  the driver it is given, and a member that does not lead takes no request.
- The `cavalry` binary of the members' own build: `target/release/cavalry` after
  `just build`.

## Steps

1. Find the member that leads. On any member's host, with that member's two
   variables set as [Run a cluster of three members](run-a-cluster.md), step 4, sets
   them:

   ```sh
   target/release/cavalry doctor --dir /tmp/cavalry-member-0 --log-dir /tmp/cavalry-member-0-log
   ```

   You will see one `note cluster-member` line. It says `the leader` when member 0
   leads, and `it follows member 1` when member 1 does.

2. On the leader's host, ask. Here member 1 leads:

   ```sh
   target/release/cavalry snapshot --dir /tmp/cavalry-member-1
   ```

   You will see one line, within a second or two, and the command exits 0:

   ```text
   member 1 lists a snapshot of log position 192: every entry below that position is in it. Every member takes a snapshot where its leader asks for one, and lists it in the `snapshots` file of its log directory
   ```

   The position is 192 on a cluster that has committed nothing else: the term's
   first entry and the leader's request, 96 bytes each.

## How to verify it worked

- The command printed the line of step 2 and exited 0. It prints that line only
  once the member it asked lists a snapshot that the member asked for after the
  command began, so the leader's snapshot counts from then on. Where the member gave
  that snapshot up and took a later one, the line names the later position and says
  so.
- The other members take theirs when their own read of the log reaches the
  leader's request, some milliseconds later. `ls /tmp/cavalry-member-0-log` lists a
  file `snapshots` on each member: the list of the snapshots a start may begin
  from.
- A service beside a member takes a snapshot of its own state at the same position
  when its program gives the service that state. The command does not wait for it.

## When it does not work

Every message that the command writes itself ends with this section's name. A
message of the driver's, such as one that no driver runs in the directory, names its
own page. The command exits 1 with one of them, and has removed its request before
it ends.

- **`member 0 does not lead, so nothing was asked: member 1 leads.`** Run the
  command on member 1's host, with member 1's driver directory.
- **`... the cluster is holding an election.`** No member leads now. Run `cavalry
  doctor` until its `cluster-member` note says `election closed` and names the
  leader, then ask that member.
- **`the driver in ... hosts no cluster member`.** The directory is a driver's that
  was started without the cluster's two variables, or the wrong directory. Give
  `--dir` the driver directory of the member that leads.
- **`member 1 is of a build that takes no request for a snapshot`.** The member has
  no `cluster.control_toggle_taken` counter a second after the command found it: it
  was built before the command existed. Upgrade it
  ([Upgrade a cluster one member at a time](upgrade-a-cluster.md)), or wait for the
  snapshot its leader asks for each 256 MiB of log.
- **`member 1 is starting and has not added all its counters yet`.** The member's
  process began a moment ago. Run the command again once `cavalry doctor` says its
  election closed.
- **`member 1 went while the command read its counters`.** The member's process
  stopped. Run `cavalry doctor` on its host, start the member again, and ask the
  member that leads then.
- **`member 1 stopped leading while the request waited, and the request was
  removed`.** Another member leads now, or the cluster is holding an election: the
  message says which, and names the leader when there is one. Ask that member.
- **`member 1 took the request, asked every member for a snapshot of log position
  192, and stopped leading before it listed its own`.** The request is in the old
  leader's log. Where the cluster committed it, every member takes the snapshot.
  Where the next leader's log does not hold it, no member does. To be sure of one,
  ask the member that leads now.
- **`member 1 leads and did not take the request within 10 s`.** A leader takes a
  request once it takes no snapshot of its own and its followers have committed the
  last request it made. So a snapshot of its own did not end, its followers do not
  answer, or its process has stopped and left its counters for the driver to free.
  `cavalry doctor` on the leader's host says which: its `cluster-member` note gives
  the member's role and positions, and a `cluster.snapshots_failed` or
  `cluster-commit-waits` line gives the cause. Then ask again.
- **`... did not list its own within 10 s`.** The leader appended the request and
  its own snapshot did not finish: its recorder did not write, or its log did not
  sync to the snapshot's position. Nothing committed is lost. `cavalry doctor` on
  the leader's host says which, and the next request asks again.
- **`the driver of member 1 went, or closed this command's client`.** The driver
  stopped while the command waited, or did not hear from the command for the client
  liveness timeout. What the member took is not known. Run `cavalry doctor` on the
  host, and ask again when the driver and its member run.
- **`member 1's cluster.control_toggle_taken is ..., at or above this command's
  client id`.** A program other than this command added a request with an id that
  is no client's of the driver, and the member takes no request below it. Stop the
  member and start it again with `cavalry run`, which clears it.
- Two people ask one member at the same moment. The leader may take one request
  for both: each command prints the same position, of a snapshot that the leader
  asked for after both began.
