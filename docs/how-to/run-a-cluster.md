# Run a cluster of three members

A how-to page: one task, start three drivers as one cluster, see which member leads
and that the others follow it, and read what `cavalry doctor` says when a member is
left out. What the members say to each other is
[`spec/cluster.md`](../reference/cluster.md); why the leader is appointed and not
elected in this version is
[ADR 0063](../decisions/0063-run-the-consensus-module-as-a-client-and-lead-by-appointment.md).
In Aeron, this is Cluster ([glossary](../glossary.md)).

A cluster is three or more drivers that keep one log between them. Each driver is a
member. One member, the leader, writes the log; every member records it; an entry is
committed once a quorum, two of three, has it on disk.

## Prerequisites

- Cavalry built on every host: `cargo build --release -p cavalry`. The steps below run
  all three members on one host, on the loopback, each with a driver directory and a
  log directory of its own. On three hosts the steps are the same with each host's
  address in place of `127.0.0.1`.
- Five free UDP ports for each member. The steps use 9000 to 9004, 9010 to 9014 and
  9020 to 9024.
- Log directories that hold no cluster yet. A member does not start again on a log
  directory that holds a term of an earlier run: this version has neither the election
  nor the catch-up that a restart needs.

## Steps

1. Write the members string, once, for every member. Each member is its id and five
   endpoints, `id,ingress,consensus,log,catchup,archive`, and members are separated by
   `|`:

   ```sh
   export CAVALRY_CLUSTER_MEMBERS='0,127.0.0.1:9000,127.0.0.1:9001,127.0.0.1:9002,127.0.0.1:9003,127.0.0.1:9004|1,127.0.0.1:9010,127.0.0.1:9011,127.0.0.1:9012,127.0.0.1:9013,127.0.0.1:9014|2,127.0.0.1:9020,127.0.0.1:9021,127.0.0.1:9022,127.0.0.1:9023,127.0.0.1:9024'
   ```

   Every member must be given the same string. The member with the lowest id, 0 here,
   is the one that leads: the appointed leader.

2. Check the string before anything starts:

   ```sh
   CAVALRY_CLUSTER_MEMBER_ID=0 target/release/cavalry doctor --dir /tmp/cavalry-member-0
   ```

   You will see `FAIL driver-directory`, since no driver runs there yet, and no line
   that begins `FAIL cluster-members` or `FAIL cluster-member-id`. One of those two
   names what is wrong with the string or the id.

3. Start the three members, each in a terminal of its own, with the same string and
   its own id, driver directory and log directory:

   ```sh
   CAVALRY_CLUSTER_MEMBER_ID=0 target/release/cavalry run --dir /tmp/cavalry-member-0 --log-dir /tmp/cavalry-member-0-log
   CAVALRY_CLUSTER_MEMBER_ID=1 target/release/cavalry run --dir /tmp/cavalry-member-1 --log-dir /tmp/cavalry-member-1-log
   CAVALRY_CLUSTER_MEMBER_ID=2 target/release/cavalry run --dir /tmp/cavalry-member-2 --log-dir /tmp/cavalry-member-2-log
   ```

   Each prints `cavalry run: driver started on ...` and then `cavalry run: cluster
   member <id> started`. Start all three within a minute. The leader waits up to 60 s
   for every member before it leads with two, and a member that starts after the
   leader's first entry cannot join.

4. Ask each member what it is:

   ```sh
   CAVALRY_CLUSTER_MEMBER_ID=0 target/release/cavalry doctor --dir /tmp/cavalry-member-0 --log-dir /tmp/cavalry-member-0-log
   ```

   You will see `note cluster-member  this driver is cluster member 0, the leader in
   term 0, election closed: append position 96, commit position 96`. For members 1 and
   2, with their own id and directories in the command, the line says `a follower in
   term 0, election closed`, with the same two positions once their recorders have
   synced, which they do once a second.

5. Stop a member with Ctrl-C or SIGTERM. It prints `cavalry run: stopping` and
   `cavalry run: closed`. The other two are a quorum and go on.

## How to verify it worked

- `cavalry doctor` on each member's directory, with the member's two variables set as
  in step 4, prints one `note cluster-member` line and no `WARN` or `FAIL` line whose
  check begins `cluster-`.
- Exactly one member says `the leader`, the member with the lowest id.
- Every member reports `election closed` and the same `commit position`.
- `ls /tmp/cavalry-member-1-log` lists `terms`, the member's record of the terms it
  has joined, beside `catalog` and the recording of the leader's log, `0-0.rec` with
  its time index `0.idx`.
- A member that started before another may show `WARN receiver.receive_failed`: the
  datagrams it sent before the other member listened were refused by this host's
  kernel. The count stops rising once every member runs.
- Every member's recording holds the same entries at the same positions. To see it,
  stop the three members. Start a driver on one member's directories, in a terminal
  where neither variable is set:

  ```sh
  target/release/cavalry run --dir /tmp/cavalry-member-1 --log-dir /tmp/cavalry-member-1-log
  ```

  In a second terminal, replay that member's recording, as
  [Replay a recording](replay-a-recording.md) says:

  ```sh
  target/release/cavalry replay --dir /tmp/cavalry-member-1 --log-dir /tmp/cavalry-member-1-log --recording 0 --from 0
  ```

  You will see one line, position `0` and a body in hex, and then `replay 3 of
  recording 0 ended at position 96: 1 messages from position 0`. The line is the
  term's first entry, which the leader appended when its election closed. Every
  member's recording gives the same line. `just replay-check` is that check as a gate:
  three members replicate and commit 100,000 entries whose payloads carry a hash
  chain, each member's recording is replayed through a driver of its own, and the gate
  fails unless the three members' lines hash the same and their payloads are the ones
  the leader was offered, in order (`scripts/replay-check.sh`).
- A program beside a member reads the entries as the cluster commits them:
  [Read a cluster's committed log from a service](read-the-committed-log.md).

## When it does not work

- `cavalry run: CAVALRY_CLUSTER_MEMBERS is set and CAVALRY_CLUSTER_MEMBER_ID is not`,
  or the other way round: a member needs both. Set the one that is missing. Nothing
  was started.
- `FAIL cluster-members` in `doctor`: the members string is one the cluster protocol
  refuses, the message says why, or an endpoint's host name resolves to no address on
  this host, and the message names the member and the endpoint. Correct the string on
  every member.
- `FAIL cluster-member-id` in `doctor`: the id is not one of the string's, and the
  message lists the ids it holds; or the string is set and the id is not.
- `cavalry run: member refused (restart)`: the log directory holds a term of an
  earlier run. A member of this version does not start again on its log. Start the
  cluster again as a new cluster: stop every member, give every member a new log
  directory, and start them together. A new log directory on one member alone joins
  nothing.
- `WARN cluster-no-leader`: the member is in canvass and has no leader to follow. The
  appointed leader is not running, its consensus endpoint is not reached from this
  host, or it still waits for a member. Run `doctor` on the leader's host.
- `WARN cluster-joined-late`: the member's elections fail one after another. It
  started after the cluster's first entry, or lost what it held, and this version
  cannot bring it up to the leader's log. The cluster runs without it. To have it
  back, start the cluster again as a new cluster.
- `FAIL cluster-leader-behind`: this member is the appointed leader and other members
  hold a log ahead of its own, so it does not lead. Its log directory was lost or
  replaced while the others kept theirs; a container whose log directory is not on a
  volume does this at every restart. Start the cluster again as a new cluster, and
  keep the old log directories for what they hold.
- `WARN cluster-quorum-lost`: the leader heard from fewer than two of three members
  within 10 s, and nothing was committed while that lasted. Check that two members run
  and that each reaches the leader's consensus endpoint.
- `note cluster-commit-waits`: the leader has appended more than a quorum has
  recorded. Up to a second of entries waits at any moment, since a recorder syncs once
  a second. If the commit position stays where it is while the append position moves,
  run `doctor` on each follower's host.
