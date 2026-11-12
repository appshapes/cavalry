# Run a cluster of three members

A how-to page: one task, start three drivers as one cluster, see which member the
others elected to lead, stop the leader and see another take its place, start the
member that was stopped again, and read what `cavalry doctor` says when a member
cannot join. What the members say to each other is
[`spec/cluster.md`](../reference/cluster.md); how they elect a leader is its section
"Roles and the election", and why the consensus module elects is
[ADR 0104](../decisions/0104-have-the-consensus-module-elect-its-leader-and-take-the-appointed-leader-out.md).
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
  9020 to 9024. On several hosts, each member's five ports must also be reachable over
  UDP from every other member's host: open them in each host's firewall and in any
  cloud security group between the hosts. A member whose `log` port the leader's host
  cannot reach does not join, and `doctor` reports it as `cluster-joining`, with how
  many of its elections have failed.
- A log directory for each member. A new cluster starts on log directories that hold
  no cluster yet. A member that was stopped starts again on the log directory it had,
  and on no other (step 7).

## Steps

1. Write the members string, once, for every member. Each member is its id and five
   endpoints, `id,ingress,consensus,log,catchup,archive`, and members are separated by
   `|`:

   ```sh
   export CAVALRY_CLUSTER_MEMBERS='0,127.0.0.1:9000,127.0.0.1:9001,127.0.0.1:9002,127.0.0.1:9003,127.0.0.1:9004|1,127.0.0.1:9010,127.0.0.1:9011,127.0.0.1:9012,127.0.0.1:9013,127.0.0.1:9014|2,127.0.0.1:9020,127.0.0.1:9021,127.0.0.1:9022,127.0.0.1:9023,127.0.0.1:9024'
   ```

   Every member must be given the same string. The members elect one of them as the
   leader; which one is not the same at every start.

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
   member <id> started`. Start all three within a minute. Three members that have
   heard one another elect a leader within a second. Two that run while the third
   is not started wait 60 s for it, the startup canvass timeout, and then elect one
   of the two; the third joins when it starts and catches up from the leader.

4. Ask each member what it is:

   ```sh
   CAVALRY_CLUSTER_MEMBER_ID=0 target/release/cavalry doctor --dir /tmp/cavalry-member-0 --log-dir /tmp/cavalry-member-0-log
   ```

   You will see a line that begins `note cluster-member`. Run the command for members
   1 and 2 too, with their own id and directories. One of the three lines says `the
   leader`: `this driver is cluster member 2, the leader in term 0, election closed:
   append position 96, commit position 96; its last election began because the member
   started`, when member 2 was elected. The other two say `a follower in term 0,
   election closed`, with the same two positions once their recorders have synced,
   which each member asks its recorder for as soon as an entry is written, and name
   the leader: `; it follows member 2; its last election began because the member
   started`. The term is 1 where two members asked for votes at the same moment and
   a second ballot was taken.

5. Stop a follower with Ctrl-C or SIGTERM. It prints `cavalry run: stopping` and
   `cavalry run: closed`. The other two are a quorum and go on.

6. Start again from step 3 with new directories, and stop the leader this time. The
   two followers hear nothing from it for 10 s, the leader heartbeat timeout, and
   elect one of them. About 11 s after the stop, `doctor` on one of them says `the
   leader in term 1, election closed: append position 192, commit position 192; its
   last election began because its leader said nothing for the leader heartbeat
   timeout`, and on the other `a follower in term 1`, which `follows` the new leader.
   Both also print `WARN cluster.leader_heartbeat_timeouts`, once, for the leader
   they lost. A member can notice the loss the other way first: it then says `its
   last election began because its image of the leader's log was no longer
   available`, and prints `WARN cluster.log_images_lost` in place of that line. Both
   mean that the leader is gone.

7. Start the member you stopped again, with its command of step 3: the same id, the
   same driver directory and the log directory it had. When member 2 led and was
   stopped:

   ```sh
   CAVALRY_CLUSTER_MEMBER_ID=2 target/release/cavalry run --dir /tmp/cavalry-member-2 --log-dir /tmp/cavalry-member-2-log
   ```

   It prints `cavalry run: cluster member 2 started`. The member reads back the log
   its log directory holds, joins the leader the other two elected, and replicates
   from it what was committed while it was away. Within a second, `doctor` on it
   says `a follower in term 1, election closed: append position 192, commit position
   192; it follows member 0; its last election began because the member started`,
   with the id of the member that leads now in place of 0.

## How to verify it worked

- `cavalry doctor` on each member's directory, with the member's two variables set as
  in step 4, prints one `note cluster-member` line and no `WARN` or `FAIL` line whose
  check begins `cluster-` or `cluster.`.
- Exactly one member says `the leader`, and the other two name it after `it follows
  member`.
- Every member reports `election closed` and the same `commit position`.
- `ls /tmp/cavalry-member-1-log` lists `terms`, the member's record of the terms it
  has joined, and `log-recordings`, its record of which recordings hold its log,
  beside `catalog` and the recording of the leader's log, `0-0.rec` with its time
  index `0.idx`. A member that was started again holds more recordings: one for each
  time it joined a log or caught up.
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
- `cavalry run: member stopped (recordings): the log directory holds a term in its
  record of terms and no log recordings file`: the member's log directory holds
  `terms` and no `log-recordings`. A version of Cavalry before this one wrote the
  directory, or the file was removed. Without it the member cannot tell which
  recordings hold its log, and it does not start. The cluster goes on without this
  member while the other two run. To have three members again, start the cluster
  again as a new cluster: stop every member, give every member a new log directory,
  and start them together. Do not start this member alone on a new log directory
  beside the other two. It would join them and replicate the log, but it has lost
  the record of the votes it cast and the log it held, and with a member whose log
  is behind it could then elect a leader that lacks an entry the cluster committed.
- `cavalry run: member stopped (recordings)` with any other message: the
  `log-recordings` file is another member's, and the message names both members,
  or it is damaged, and the message names the check it failed. Start the member with
  its own log directory. If the file is damaged, do as for a directory with no
  file, above.
- `cavalry run: member refused (terms-lost)`: the member's log directory holds its
  log and a `terms` file that holds no term. The file was removed or replaced, and
  the member's votes and the terms of its log were in it. Start the member with the
  log directory it had, whole. If the file is lost, do as for a directory with no
  `log-recordings` file, above.
- `cavalry run: member refused (listed-recording)`: the `catalog` in the member's
  log directory does not hold the recordings that its `log-recordings` file names.
  The message names the recording and what the catalog holds in its place. The
  directory is not as the member left it: files were removed from it, or it was put
  together from two directories. Start the member with the log directory it had,
  whole. If that directory is lost, do as for a directory with no file, above.
- `WARN cluster-no-leader`: the member is in an election and has no leader to follow.
  The message says which state its election is in and why it began, from
  `cluster.election_cause`. A leader needs the votes of two of three members. At a
  start, members that have not heard every other member wait 60 s for it, the
  startup canvass timeout, before two of them elect. Run `doctor` on each member's
  host, and check that each host reaches the `consensus` endpoint of every other,
  the second port of each entry in step 1. In canvass the message has one more
  sentence, for a member that has replicated a term it missed: it canvasses again
  with no leader, in the same election, until the leader it replicated from answers
  it. A member that stays there waits for that one member and not for votes, so
  check the `consensus` endpoints between the two hosts. The message ends with how
  many of the member's elections have failed. Three counters each count one rule
  that fails an election: `cluster.join_position_mismatches`,
  `cluster.truncations_refused` and `cluster.log_replications_unavailable`. They do
  not hold every failure. When more elections failed than the three count, the
  message says how many and what else fails an election: a step that made no
  progress for 10 s, a log the member cannot join its leader with, or, on a member
  that led before its election closed, a term above its own or the loss of its
  quorum, which `cluster.leader_quorum_losses` counts.
- `note cluster-joining`: the member has taken its leader, which the message names
  from `cluster.leader_member_id`, and joins the leader's log. The message says what
  the state the member is in waits for: an image of the leader's log on the member's
  `log` endpoint, the fourth field of its entry in `CAVALRY_CLUSTER_MEMBERS`, or the
  leader's replay on its `catchup` endpoint, the fifth, when the member replicates a
  term it missed, or both when it is behind the leader's log and catches up. The
  check at the end of the message names the endpoints of that state. Each must
  arrive, and the member's append position
  must then reach the leader, within 10 s. Once they have, the election closes, and
  the next `doctor` shows `election closed` and no `cluster-joining`. An election
  that fails begins again by itself, so the message never says to start the cluster
  again; it ends with how many elections have failed, as `cluster-no-leader`
  does. A count that keeps rising is a path that stays closed:
  the leader's host must reach the member's `log` and `catchup` endpoints over UDP,
  9022 and 9023 for member 2 in step 1, and the member's host must reach the
  leader's `consensus` endpoint.
- `WARN cluster-quorum-lost`: the leader heard from fewer than two of three members
  within 10 s, and nothing was committed while that lasted. A leader that elects
  stopped leading each time and held an election, and the finding is on a member that
  leads again. Check that two members run and that each reaches the leader's consensus
  endpoint.
- `WARN cluster.messages_rejected`: the member rejected consensus messages it cannot
  read, and acted on none of them. Another member sends them from another version of
  Cavalry, as during an upgrade, and the member may canvass with no leader for as long
  as that lasts. Run `cavalry --version` on every member's host and start every member
  on the same version.
- `WARN cluster.leader_heartbeat_timeouts`: the follower heard nothing from its leader
  for 10 s and began its election again. Check that the leader runs, that `doctor` on
  the leader's host shows no `cluster-quorum-lost`, and that this host reaches the
  leader's consensus endpoint.
- `WARN cluster.log_images_lost`: the follower lost its image of the leader's log
  stream and began its election again. Check the network path from the leader's log
  endpoint to this host, and the image counters of stream 100 on this member's driver.
- `note cluster-commit-waits`: the leader has appended more than a quorum has
  recorded. Each member asks its recorder to sync an entry as soon as it is written,
  so what waits at any moment is what was appended since the syncs now running began.
  A request that a member's driver refused, its recorder's queue of requests full,
  leaves the entry to the recorder's own sync, within a second; `driver.registry_full`
  on that member's driver counts the refusals. If the commit position stays where it
  is while the append position moves, run `doctor` on each member's host: the leader's
  own disk is one of the quorum's.
