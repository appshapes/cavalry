# Read a cluster's committed log from a service

A how-to page: one task, run a program beside a cluster member that reads every entry
the cluster has committed, in order, each once. What a service reads and how it is
bounded is [`spec/cluster.md`, "The service"](../reference/cluster.md#the-service); why
it reads a replay of its member's recording is
[ADR 0066](../decisions/0066-feed-the-committed-log-to-a-service-through-a-replay-of-the-members-recording.md).
In Aeron, this is a clustered service ([glossary](../glossary.md)).

A service is a program that applies the cluster's log: every member has one beside it,
each applies the same entries at the same positions, and so each reaches the same
state. An entry is committed once a quorum of members, two of three, has it on disk. A
service is given an entry only then, and only once its own member has it on disk too.

In this version nothing outside a member appends to the log: a cluster started with
`cavalry run` holds one entry, the first of its leader's term. The steps below read
that entry. A program that embeds a member appends with `ConsensusModule::offer`, and
its services read what it appends in the same way.

## Prerequisites

- A cluster of three members running on this host, started as
  [Run a cluster of three members](run-a-cluster.md) says, with member 1's driver
  directory at `/tmp/cavalry-member-1` and member 0's at `/tmp/cavalry-member-0`.
- The example service built: `cargo build --release -p cavalry --example cluster-service`.
  Its source is `crates/cavalry/examples/cluster-service.rs`.

## Steps

1. Read member 1's committed log, one entry:

   ```sh
   target/release/examples/cluster-service --dir /tmp/cavalry-member-1 --entries 1
   ```

   You will see three lines:

   ```text
   cluster-service: reading member 1's log, recording 0, from position 0
   0 template 24 56 bytes
   cluster-service: 1 entries, next position 96, commit position 96
   ```

   The second line is the entry: its position in the log, 0, its template id, 24, which
   is the new leadership term event, and its length. The first and third lines are on
   stderr. `next position` is where the entry after it begins.

2. Read the leader's, member 0's:

   ```sh
   target/release/examples/cluster-service --dir /tmp/cavalry-member-0 --entries 1
   ```

   You will see the same second line. The leader's service and a follower's read the
   log the same way and see the same entries.

3. Start again where a service stopped. Give the next position the last run printed:

   ```sh
   target/release/examples/cluster-service --dir /tmp/cavalry-member-1 --from 96
   ```

   You will see `cluster-service: reading member 1's log, recording 0, from position
   96` and no entry: nothing has been committed past position 96. The program waits
   for the next entry. Stop it with Ctrl-C.

4. In your own program, do what the example does. `Cavalry::connect_to(dir)` connects
   to the member's driver. `client.committed_log(position)` opens the log at a
   position, 0 or the next position of an earlier run, and returns `None` while the
   driver has no member with a log yet, so a service that starts before its member
   calls it again after a pause. `log.poll(handler, fragment_limit)` hands each
   committed entry to the handler with its position and returns how many it read.
   `log.position()` is the next position, the one to keep for the next start.
   `log.is_ended()` says that the member closed or left its log: when a poll then
   returns 0, open the log again from `log.position()`.

## How to verify it worked

- Steps 1 and 2 print the same entry line on every member.
- `target/release/cavalry doctor --dir /tmp/cavalry-member-1 --log-dir
  /tmp/cavalry-member-1-log`, with the member's two variables set as
  [Run a cluster of three members](run-a-cluster.md) sets them, prints the member's
  `commit position`. A service's `commit position` line never shows a larger one, and
  its `next position` is never past it.
- While a service runs, the same `doctor` lists one replay on the driver. After the
  service ends, the `replay.stopped` counter has grown by one, or `replay.ended` when
  the member's log ended first.

## When it does not work

- `cluster-service: waiting for a cluster member with a log on this driver`, and
  nothing more: the driver in `--dir` runs no cluster member, or the member has no log
  yet. Run `doctor` on that directory with the member's two variables set. A member
  whose line says `election closed` has a log. `WARN cluster-no-leader` and `WARN
  cluster-joined-late` are explained in
  [Run a cluster of three members](run-a-cluster.md#when-it-does-not-work).
- `error 23 (position out of range)`: `--from` is not where an entry begins, or it is
  past what this member has recorded. Use a `next position` that a run on this member
  printed, or 0.
- `error 10 (resource temporarily unavailable)`: the driver already plays 64 replays.
  Each service holds one while it runs. Stop the services that are not needed.
- `cluster-service: the member's log ended`: the member closed or left its log while
  the service read it. The service has read every committed entry up to the position
  in the message. Start it again with `--from` that position once the member runs.
- A service is given entries later than the cluster commits them, by up to a second:
  an entry reaches a service once the member's recorder has synced it, and the
  recorder syncs once a second. No entry is lost or reordered by the wait.
