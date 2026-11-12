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
  9020 to 9024. On several hosts, each member's five ports must also be reachable over
  UDP from every other member's host: open them in each host's firewall and in any
  cloud security group between the hosts. A member whose `log` port the leader's host
  cannot reach is left out of the cluster, and `doctor` reports it as
  `cluster-joined-late`.
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

   You will see a line that begins `note cluster-member` and says `this driver is
   cluster member 0, the leader in term 0, election closed: append position 96,
   commit position 96`. For members 1 and 2, with their own id and directories in the
   command, the line says `a follower in term 0, election closed`, with the same two
   positions once their recorders have synced, which each member asks its recorder
   for as soon as an entry is written. The line ends with who the member follows and
   why its last election began: on members 1 and 2, `; it follows member 0; its last
   election began because the member started`. On member 0, which follows no member,
   it ends `; its last election began because the member started`.

5. Stop a member with Ctrl-C or SIGTERM. It prints `cavalry run: stopping` and
   `cavalry run: closed`. The other two are a quorum and go on.

## How to verify it worked

- `cavalry doctor` on each member's directory, with the member's two variables set as
  in step 4, prints one `note cluster-member` line and no `WARN` or `FAIL` line whose
  check begins `cluster-` or `cluster.`.
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
- `WARN cluster-no-leader` on a member that is not the appointed leader: the member is
  in canvass and has no leader to follow. The appointed leader is not running, its
  consensus endpoint is not reached from this host, or it still waits for a member.
  The message names the appointed leader's id; run `doctor` on that member's host.
- `WARN cluster-no-leader` on the appointed leader, whose message begins `member 0 is
  the appointed leader`: it does not lead yet. It leads once every other member's
  canvass position has reached it, or, after the 60 s startup canvass timeout, once
  two of three have, itself among them. A member it has not heard is not running, or
  cannot reach this member's consensus endpoint. Run `doctor` on each other member's
  host, and check that the other hosts reach this host's consensus port, 9001 in step
  1. `doctor` tells the two cases apart from `CAVALRY_CLUSTER_MEMBERS`; without it, the
  message describes both.
- `note cluster-joining`: the member has taken its leader and joins the leader's log.
  An image of the log must arrive on the member's `log` endpoint, and then its append
  position must reach the leader, each within 10 s. Once both have, the election
  closes, and the next `doctor` shows `election closed` and no `cluster-joining`. If
  either takes longer, the election fails, and `doctor` reports `cluster-joined-late`.
  The message names what is still to come in the state the member is in, and the
  network path that needs. In `follower replay` the member replays its own log
  first, which no timeout bounds; a log that then ends behind the leader's fails
  the election, which `cluster-joined-late` below reports with none on such an
  image, its second cause. Then, and in `follower log await`, both steps and their
  path, the same path as that entry's first cause, the network. In `follower ready`
  the image has arrived, and the message names the append position alone and the
  way to the leader's consensus endpoint.
- `WARN cluster-joined-late`: the member's elections fail one after another, and the
  cluster runs without it. The message says how many failed, and how many of them on
  an image of the leader's log that joined past the member's append position.
  - With none on such an image, there are two causes, and the counters do not tell
    them apart. The first is the network: the leader's log does not reach the member.
    No image of the log arrived on the member's `log` endpoint, the fourth field of
    its entry in `CAVALRY_CLUSTER_MEMBERS`, within 10 s. The leader then closed its
    election with the other two and appended, and every later election of this member
    fails. The second is a member that started after the cluster's first entry, or
    lost what it held. Check the network first, because a new cluster leaves the
    member out again while the path is closed. The leader's host must reach the
    member's `log` endpoint over UDP, 9022 for member 2 in step 1. The member's host
    must reach the leader's `consensus` endpoint, 9001 in step 1. With
    `CAVALRY_CLUSTER_MEMBERS` set, the message names both endpoints. Open the path,
    then start the cluster again as a new cluster.
  - With one or more on such an image, the leader's log reaches the member, and the
    member's log is behind it: it started late or lost what it held. This version
    cannot bring it up to the leader's log. To have it back, start the cluster again
    as a new cluster.
- `FAIL cluster-leader-behind`: this member is the appointed leader and other members
  hold a log ahead of its own. Found in canvass, it does not lead: the note says `a
  follower`. Found in leader ready, it leads and does not close its election: the note
  says `the leader` and `election leader ready`, and nothing of its term is appended
  or committed. Either way its log directory was lost or replaced while the others
  kept theirs; a container whose log directory is not on a volume does this at every
  restart. Start the cluster again as a new cluster, and keep the old log directories
  for what they hold.
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
- `WARN cluster-no-leader` or `note cluster-joining` whose message names no appointed
  leader: the member elects its leader. No member started with `cavalry run` does
  yet; `doctor` knows such a member by its counters, which hold
  `cluster.leader_member_id` and no `cluster.appointed_leader_behind`. With no leader,
  the message says which state the election is in and why it began, from
  `cluster.election_cause`. A leader needs the votes of two of three members, so run
  `doctor` on each member's host and check that each reaches the `consensus` endpoint
  of every other. In canvass the message has one more sentence, for a member that has
  replicated a term it missed: it canvasses again with no leader, in the same
  election, until the leader it replicated from answers it. A member that stays there
  waits for that one member and not for votes, so check the `consensus` endpoints
  between the two hosts. With a leader taken, the message names that member, from
  `cluster.leader_member_id`, and what the state waits for: an image of the leader's
  log on the member's `log` endpoint, the leader's replay on its `catchup` endpoint,
  the fifth field of its entry, or both during a catch-up. The check at the end of the
  message names the endpoints of that state. A member that elects begins again after
  an election that fails, so neither message says to start the cluster again. Both
  end with how many elections have failed. Three counters each count one rule that
  fails an election: `cluster.join_position_mismatches`,
  `cluster.truncations_refused` and `cluster.log_replications_unavailable`. They do
  not hold every failure. When more elections failed than the three count, the
  message says how many and what else fails an election: a step that made no progress
  for 10 s, a log the member cannot join its leader with, or, on a member that led
  before its election closed, a term above its own or the loss of its quorum, which
  `cluster.leader_quorum_losses` counts.
