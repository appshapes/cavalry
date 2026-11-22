# Run a cluster of three members

A how-to page: one task, start three drivers as one cluster, see which member the
others elected to lead, stop the leader and see another take its place, start the
member that was stopped again, and read what `cavalry doctor` says when a member
cannot join. What the members say to each other is
[`spec/cluster.md`](../reference/cluster.md); how they elect a leader is its section
"Roles and the election", and why the consensus module elects is
[ADR 0104](../decisions/0104-have-the-consensus-module-elect-its-leader-and-take-the-appointed-leader-out.md).
In Aeron, this is Cluster ([glossary](../glossary.md)). To learn what a cluster is by
running one with a service and a client, take
[Run a cluster in ten minutes](../tutorials/run-a-cluster.md) first.

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
- Each member's log directory on storage that outlives the member's host or
  container: a volume, not the container's own file system. The directory holds the
  member's votes and its log. A member that comes back on an empty one, beside the
  others, votes with no record of them, and with a member whose log is behind it can
  elect a leader that lacks an entry the cluster committed. `cavalry run` cannot tell
  an empty directory from a new cluster's, so nothing stops it: never start one
  member alone on a new log directory.
- Every member on this version of Cavalry. A member of a version in which the member
  with the lowest id was the leader casts no vote, so the two are not one cluster,
  and a log directory that version wrote is refused here. To move a cluster from
  that version, stop every member and start the cluster again as a new one, every
  member on a new log directory.

## Steps

1. Write the members string, once, for every member. Each member is its id and five
   endpoints, `id,ingress,consensus,log,catchup,archive`, and members are separated by
   `|`:

   ```sh
   export CAVALRY_CLUSTER_MEMBERS='0,127.0.0.1:9000,127.0.0.1:9001,127.0.0.1:9002,127.0.0.1:9003,127.0.0.1:9004|1,127.0.0.1:9010,127.0.0.1:9011,127.0.0.1:9012,127.0.0.1:9013,127.0.0.1:9014|2,127.0.0.1:9020,127.0.0.1:9021,127.0.0.1:9022,127.0.0.1:9023,127.0.0.1:9024'
   ```

   Every member must be given the same string. The members elect one of them as the
   leader; which one is not the same at every start. A member whose entry ends in a
   seventh field, `,standby`, is a standby: it follows the log and has no vote, and
   [Promote a standby to a voter](promote-a-standby.md) gives it one.

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
   started`. Every line then names the members that the member's own string gives a
   vote to, `; its members string gives a vote to members 0, 1 and 2`, the same on
   the three, and the version of the cluster protocol that the member runs, `; it
   runs version 1 of the cluster protocol, 0.1.0`. The term is 1 or more where two
   members asked for votes at the same moment and another ballot was taken. Each
   line ends with `; see` and the page that explains it, as every finding does.

5. Stop a follower with Ctrl-C or SIGTERM. It prints `cavalry run: stopping` and
   `cavalry run: closed`. The other two are a quorum and go on.

6. Start a new cluster, and stop the leader this time. Stop the two members that
   still run, remove every member's directories, and start the three again with
   the commands of step 3:

   ```sh
   rm -rf /tmp/cavalry-member-0 /tmp/cavalry-member-0-log /tmp/cavalry-member-1 /tmp/cavalry-member-1-log /tmp/cavalry-member-2 /tmp/cavalry-member-2-log
   ```

   Without the `rm`, the members start again on the log they kept, as step 7
   shows, and the positions below are higher. Stop the leader that `doctor` names.
   The two followers hear nothing from it for 10 s, the leader heartbeat timeout, and
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

   Do this for a member that was stopped, or whose process was ended. Do not do it
   for a member whose host lost power, or whose operating system crashed, while the
   member ran: "When it does not work" says why, under "The member's host lost
   power".

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
  chain, each member's log is replayed through a driver of its own, across its
  recordings where it holds two, and the gate
  fails unless the three members' lines hash the same and their payloads are the ones
  the leader was offered, in order (`scripts/replay-check.sh`).
- A program beside a member reads the entries as the cluster commits them:
  [Read a cluster's committed log from a service](read-the-committed-log.md).

## When it does not work

- `cavalry run: CAVALRY_CLUSTER_MEMBERS is set and CAVALRY_CLUSTER_MEMBER_ID is not`,
  or the other way round: a member needs both. Set the one that is missing. Nothing
  was started.
- `FAIL cluster-members` in `doctor`: the members string is one the cluster protocol
  refuses, and the message says why. Or a host name in it resolves to no address on
  this host: `doctor` looks each host name up once, and one finding names the host
  and every member's endpoints that it is the host of. Correct the string on every
  member.
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
- `cavalry run: member stopped (recordings)` with any other message. Read the
  message, since three causes share the prefix:
  - It begins `could not` and names the file and the operating system's reason. The
    member could not open, read, write or sync `log-recordings`: the log directory
    is not writable by the member's user, or its file system is full. Correct the
    permissions or free space, and start the member again on the same log
    directory. A running member prints the same message after `cavalry run: cluster
    member <id>:` and is not stopped by it: it tries the write again in each duty
    cycle, and `cavalry run` gives the member up after 10 s of cycles that fail.
    Start it again on the same log directory once the directory can be written.
  - It says the log directory is another member's, and names both members. Start
    the member with its own log directory.
  - It says the file is damaged, and names the check it failed. Do as for a
    directory with no file, above.
- `cavalry run: member refused (start-stop-timeout)`: the member's driver was still
  making a recording of the member's log, which happens when the member's process
  was ended and its driver ran on. The start stopped the recording, and the driver
  did not say where it stopped within 1 s. Nothing else was changed. Start the
  member again. If the message repeats, run `cavalry doctor --dir` on the member's
  driver directory: the driver is starved of CPU or cannot write its log directory.
- `cavalry run: member refused (terms-lost)`: the member's log directory holds its
  log and no record of its terms. Either there is no `terms` file, or the file holds
  no term: it was removed or replaced, and the member's votes and the terms of its
  log were in it. Start the member with the log directory it had, whole. If the file
  is lost, do as for a directory with no `log-recordings` file, above. Do not put an
  older copy of the file back.
- `cavalry run: member refused (listed-recording)`: the `catalog` in the member's
  log directory does not hold the recordings that its `log-recordings` file names.
  The message names the recording and what the catalog holds in its place. The
  directory is not as the member left it: files were removed from it, or it was put
  together from two directories. Start the member with the log directory it had,
  whole. If that directory is lost, do as for a directory with no file, above.
  One of its messages can come from a directory that nobody changed: `the
  recordings before it ending at` a position, `below where it begins`. A
  truncation of this member's log stopped on a fault part way, and the member then
  led a term, so its file names a recording that begins past the cut. Leave this
  member stopped, as for a directory that is lost.
  Another names a position `below position` of `the newest snapshot the member can
  read`: the last recordings of the member's log are gone, and the log that is left
  ends below the position the member's start would read it from. A truncation
  never cuts that far, so a recording at or above the snapshot's position was
  removed, as by a `purge recording` of one recording too many. Purge only the
  recordings of a member's log that stopped at or below the position its log is
  needed from, which the member's gauge `cluster.log_purge_position` holds while
  it runs: the lower of the positions of its newest snapshot and of its service's.
  That position assumes a service whose program calls `ClusterService::restore`
  before its first poll. A service whose program takes snapshots and never calls
  it reads the member's log from position 0 at every start: beside it, purge
  nothing, whatever the gauge says. The member cannot tell the two apart. And the
  position is of the newest snapshots the member lists: purge by it only while
  this member's `cluster.snapshots_passed_over` and
  `cluster.service_snapshots_passed_over` are 0, since a start that passed a
  snapshot over read its log from an older one, or from position 0, and the next
  start does so again.
  Start the member with the log directory it had, whole. If that directory is
  lost, do as for a directory with no file, above.
- `cavalry run: member refused (log-short)`: the `catalog` in the member's log
  directory holds every recording that its `log-recordings` file names, and the
  log they hold ends below the position of the member's newest snapshot. The
  message names both positions. The member read its log to the snapshot's position
  before it took the snapshot, so the end of its log has gone since: a recording of
  the log was cut, as by a `truncate recording`, or the `log-recordings` file and
  the `catalog` were put back from a copy older than the `snapshots` file. Never
  truncate a recording of a member's log. Start the member with the log directory
  it had, whole. If that directory is lost, do as for a directory with no file,
  above.
- `cavalry run: cluster member <id>: member stopped (append)`: the member leads, and
  its log publication refused an entry for a reason that waiting does not mend. The
  message names the reason and says what to do:
  - `closed`: the member's client is closed, because its driver stopped answering it
    or was closed. Run `cavalry doctor --dir <the member's driver directory>` to see
    whether the driver runs, and start the member again with its command of step 3.
    It starts on the log directory it had.
  - `max-position-exceeded`: the cluster's log is at the largest position its term
    length allows, and no leader can append past it. Stop every member and start a
    new cluster, as step 6 does.
  - `too-long`: the consensus module appended an entry longer than its own bound.
    That is a fault of Cavalry: report it, with the message.

  `cavalry run` tries the member's cycle again every 100 ms and stops the member
  after 10 s of failures on end. The other two elect a leader meanwhile.
- `cavalry run: member refused (log-forgotten)`: the member's log no longer reaches
  back to where its start reads it from. A member that starts takes its sessions
  from its newest snapshot and reads its log from that snapshot's position, or from
  position 0 when it has no snapshot it can read. The refusal names where the log
  begins and which position the start would read from. A log begins above 0 for
  two reasons. A member makes one recording each time it joins a log, catches up or
  leads a term, and it keeps 1,024 of them; past that it forgets the oldest, which
  `cluster.log_recordings_forgotten` counts while it runs. Or the recordings were
  purged. A leader asks for a snapshot every 256 MiB of log, so the refusal means
  that this member's snapshots are gone or older than its log: the cluster goes on
  while the other two run. When the refusal says that the start passed snapshots
  over, start the member once more first: a snapshot that the driver did not play
  back within 5 s, as on a loaded host, is one the next start reads. When the
  member is refused again, or the refusal names no snapshot passed over, do as for
  a directory with no `log-recordings` file, above, to have three members again.
- The member's host lost power, or its operating system crashed, while the member
  ran. Do not start the member on that log directory again. Leave it stopped, and
  do as for a directory with no `log-recordings` file, above. Nothing refuses this
  start, and no counter tells this crash from a process that was ended, so the rule
  is yours to keep. A process that was ended is safe to start again. When two of
  the three hosts lost power together, do not start any member on its log
  directory: start the cluster again as a new cluster. After such a crash a
  member's log can end in entries that were not written whole, and a member that
  starts on them can hand them to the others
  ([ADR 0108](../decisions/0108-list-the-recordings-of-a-members-log-in-its-log-directory-and-start-a-member-on-the-log-it-kept.md),
  "What a start does not hold").
- `cavalry run: the cluster member stopped, so the driver stops`, and the process
  exits with 1. The member could not go on, so `cavalry run` gave it up and closed
  its driver: a driver that runs on with the cluster's keys set and no member is
  not the member you started. The rest of the line says why. Either the member's
  duty cycle failed for 10 s on end, and the line ends with the last error, which
  an entry above explains. Or the member's thread panicked, which is a fault of
  Cavalry: report it with the panic's message, which the process printed before
  that line. Correct what the line names, then start `cavalry run` again with the
  same directories. The other two members elect a leader meanwhile. The line
  `cavalry run: the cluster member's thread panicked as the member stopped` is the
  same fault, met while `cavalry run` was already stopping the member: for a signal,
  or after it gave the member up for duty cycles that failed. In the second case the
  process printed the line this entry begins with first. The driver closes all the
  same, the exit is 1, and the report is the same.
- `WARN run.member_given_up` in `doctor`, beside `FAIL driver-heartbeat`: the driver
  that made this directory closed because `cavalry run` gave its cluster member up,
  as the entry above says. The count is in the directory the closed driver left, so
  `doctor` says it after the process has gone, until the next `cavalry run` remakes
  the directory. Read the line the process printed that begins `cavalry run: the
  cluster member stopped`.
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
  term it missed or begins a catch-up, or both in `follower catch-up`, the last
  state of a catch-up. The two states of a catch-up before it wait on the `catchup`
  endpoint alone: the member subscribes to the live log only when the replay has
  brought its log near the leader's position. The check at the end of the message
  names the endpoints of that state. In `follower ready` the image has arrived, and
  the message names the append position alone and the leader's `consensus` endpoint
  as the one to check. Each must arrive, and the member's append position must then
  reach the leader, within 10 s. Once they have, the election closes, and
  the next `doctor` shows `election closed` and no `cluster-joining`. An election
  that fails begins again by itself, so the message never says to start the cluster
  again; it ends with how many elections have failed, as `cluster-no-leader`
  does. A count that keeps rising is a path that stays closed:
  the leader's host must reach the member's `log` and `catchup` endpoints over UDP,
  9022 and 9023 for member 2 in step 1, and the member's host must reach the
  leader's `consensus` endpoint.
- `WARN cluster-quorum-lost`: the leader heard from fewer than two of three members
  within 10 s, and nothing was committed while that lasted. The leader stopped
  leading each time and held an election, and the finding is on a member that leads
  again. Check that two members run and that each reaches the leader's consensus
  endpoint.
- `WARN cluster.messages_rejected`: the member rejected consensus messages it cannot
  read, and acted on none of them. Members of one major of the cluster protocol read
  each other, whatever their versions, as during an upgrade. So a rejected consensus
  message is usually from a member of another major, and the member may canvass with
  no leader for as long as that member runs. `cavalry doctor` on each member names the
  version that member runs, in its `note cluster-member` line: `it runs version 1 of
  the cluster protocol, 0.1.0`, where the major is the first number of `0.1.0`. Of
  a member that has no `cluster.protocol_version` counter, `doctor` says that `it
  runs a build older than that counter: version 0 of the cluster protocol, or the
  first build of version 1`. Both are of major 0. Start every member on one major:
  [Upgrade a cluster one member at a time](upgrade-a-cluster.md) has what to do when
  two builds are not of one major.
- `WARN cluster.leader_heartbeat_timeouts`: the follower heard nothing from its leader
  for 10 s and began its election again. Check that the leader runs, that `doctor` on
  the leader's host shows no `cluster-quorum-lost`, and that this host reaches the
  leader's consensus endpoint.
- `WARN cluster.log_images_lost`: the follower lost its image of the leader's log
  stream and began its election again. Check the network path from the leader's log
  endpoint to this host, and the image counters of stream 100 on this member's driver.
- `WARN cluster.log_recordings_unwritten`: the member could not write the file of
  its log's recordings in its driver directory, and no service beside it read the
  log while that lasted. The member's own message begins `member stopped
  (log-recordings)` and names the file and the cause. Check that the driver
  directory is writable by the member's user and that its file system has room. The
  member tries again every 100 ms, so the count rises by about ten a second. After
  10 s of cycles that fail, `cavalry run` stops the member: start it again once the
  directory can be written. A count that no longer rises is a fault that has passed.
- `WARN cluster.log_recordings_unsynced`: the member could not write or sync
  `log-recordings` in its log directory, and the count says how many times. While
  that lasted it reported no position from the recording it was making, so it could
  not follow or lead. Check that the log directory is writable by the member's user,
  that its file system has room and that its device reports no error. After 10 s of
  cycles that fail, `cavalry run` stops the member with `member stopped
  (recordings)` and a message that begins `could not`, above: start it again on the
  same log directory once the directory can be written. A count that no longer
  rises is a fault that has passed.
- `WARN cluster.log_recordings_forgotten`: the member's log is in more than 1,024
  recordings, and it has forgotten the oldest. It runs on, and there is nothing to
  do while it runs. Two requests fail meanwhile. A member that asks it for the log
  below the oldest recording it kept receives none, and counts
  `cluster.log_replications_unavailable` when its wait ends. A service that asks
  for that part of the log is refused by start replay with code 23. The member
  starts again on this log directory from its newest snapshot, when that snapshot
  is of a position at or above where its log now begins. When it is not, `cavalry
  run` refuses the member with `member refused (log-forgotten)`, above, and the
  cluster goes on with the other two. To have three members again then, do as that
  entry says, and never start this member alone on a new log directory.
- `WARN cluster.snapshots_failed`: the member gave up a snapshot of its sessions
  that the leader's log asked for, and the count says how many times. The member's
  log is whole, nothing committed is lost, and the member runs on. Read three
  counters of this member's driver, which the same `doctor` prints when they are
  above 0. `driver.registry_full`: the driver had no room for the snapshot's
  recording. `recording.start_failed`: the recording's files could not be created
  in the log directory. `recording.write_failed`: a write or a sync to the log
  directory failed. Check that the log directory is writable by the driver's user
  and that its file system has room. Where one of the three is above 0, this
  member took no snapshot where the others did: until it takes one, a start of
  this member reads more of its log than a start of the others does. If none of
  the three is above 0, one of three causes explains the count. A step took the
  driver longer than a second, as on a loaded host or a slow disk. Or the
  recorder took longer than 2 s to sync this member's log to the snapshot's
  position, as on a loaded host or a slow disk: a member lists a snapshot only
  once its own log has synced to the snapshot's position
  ([ADR 0143](../decisions/0143-list-a-members-snapshot-once-its-log-has-synced-to-the-snapshots-position.md)).
  Or the leader was asked for a second snapshot while the first was being taken:
  `cluster.snapshot_actions` on the leader then rose twice within a few seconds,
  every member counts one snapshot given up, and each still takes the first. For
  the first two, check the load of this host and the latency of the log
  directory's device. The leader asks for the next snapshot when its log has
  grown by 256 MiB, and the member takes that one by itself. A count that no
  longer rises is a fault that has passed.
- `WARN cluster.snapshots_passed_over`: when this member started, it could not
  read its newest snapshot, and the count says how many snapshots it passed over.
  It started from an older snapshot, or from none, and read more of its log.
  Nothing committed is lost. The member's counter `cluster.snapshot_start_position`
  holds the position it started from, or -1 when it started from none. A start
  passes a snapshot over
  for one of three reasons. The snapshot's recording is gone from the log
  directory, as after a `purge recording` of the snapshot itself. The driver found
  the recording damaged, which `recording.invalidated` on this member's driver
  counts: check the log directory's device. Or the driver did not play the
  recording back within 5 s: check the load of this host. A start waits those 5 s
  for each snapshot that the driver does not play back, and a member lists 64
  snapshots at most, so such a start takes up to 320 s before the member runs. When
  none of the three
  explains the count, the recording holds something that no member writes as a
  snapshot. That is a fault of Cavalry, or a file of the log directory that was
  changed by hand: report it, with the log directory. There is nothing to do for
  the member. Its next snapshot is listed after the ones it passed over, and its
  next start takes that one. The count stays until the member starts again.
- `WARN cluster.service_snapshots_failed`: the service beside this member gave up
  a snapshot of its program's state, and the count says how many times. Nothing
  committed is lost, and the service reads on. The service's own error names the
  step. Check first that the member runs: a service whose member is stopped writes
  its recording and has nobody to tell. Then read three counters of this member's
  driver, which the same `doctor` prints when they are above 0.
  `driver.registry_full`: the driver had no room for the snapshot's publication or
  its recording. `recording.start_failed`: the recording's files could not be
  created in the log directory. `recording.write_failed`: a write or a sync to the
  log directory failed. The service takes the next snapshot by itself, when the
  leader's log has grown by 256 MiB. A count that no longer rises is a fault that
  has passed.
- `WARN cluster-voters`: this member runs with a members string that gives a vote
  to other members than `CAVALRY_CLUSTER_MEMBERS` does on its host. The line names
  both sets. A member takes the string when it starts. While a standby is promoted,
  the members are started with the new string one at a time, and each has this line
  until its turn: [Promote a standby to a voter](promote-a-standby.md). With no
  change under way, one of the two strings is older. Read the voters in
  the other members' `note cluster-member` lines: with no change under way they
  are one set, those of the cluster's newest string. If the variable on this host
  is the older, correct the variable and start nothing. If the member runs the
  older, start the member again with the newest. Never start a member with the
  older one. Do not leave it: two members whose strings differ by more than one
  member's `standby` mark can elect a leader that lacks an entry the cluster
  committed.
- `WARN cluster.standby_vote_requests`: this member is a standby, and a member
  whose string gives it a vote asked it for one. It gave none. The two ran with
  strings that differ. Read the voters in every member's `note
  cluster-member` line, which are one set where the strings agree, and start the
  member whose line differs with the cluster's string. A member whose host holds
  an older string prints no `WARN cluster-voters`. The count stays until this
  member starts again: [Promote a standby to a voter](promote-a-standby.md).
- `WARN cluster.service_snapshots_passed_over`: the service beside this member was
  told of a snapshot when it started, and could not take it. It read the log from
  position 0. Nothing committed is lost. If this member's log no longer begins at
  0, the service's own error says that its replay was refused, and the service
  does not run. Start the service once more first: a snapshot that the driver did
  not play back within 5 s, as on a loaded host, is one the next start reads, and
  so is one whose replay another client stopped before the recording's end. When
  it is passed over again, the snapshot's recording is gone from the log
  directory, or the driver found it damaged, which `recording.invalidated` on this
  member's driver counts: check the log directory's device. The service takes its
  next snapshot by itself, and its next start reads that one. Only a refusal for
  the recording itself, error code 21 or 23, is counted here. A driver that had no
  room for the replay, error code 10, which `driver.registry_full` counts, or that
  refused it for another reason, made `ClusterService::restore` return the
  driver's error: the service did not start, and the program calls `restore`
  again.
- `WARN cluster.service_acks_rejected`: a service beside this member said that it
  took a snapshot, and the member listed nothing for it. Nothing committed is
  lost. A count that rises with every snapshot has one of two causes. More than
  one service beside this member takes snapshots, where one does in this version:
  run one. Or the service is of another version than the member: run
  `cavalry --version` on the host and start both on one version. A count that
  rose once is a service's snapshot whose recording the driver found damaged,
  which `recording.invalidated` on this member's driver counts: check the log
  directory's device.
- `note cluster-commit-waits`: the leader has appended more than a quorum has
  recorded. Each member asks its recorder to sync an entry as soon as it is written,
  so what waits at any moment is what was appended since the syncs now running began.
  A request that a member's driver refused, its recorder's queue of requests full,
  leaves the entry to the recorder's own sync, within a second; `driver.registry_full`
  on that member's driver counts the refusals. If the commit position stays where it
  is while the append position moves, run `doctor` on each member's host: the leader's
  own disk is one of the quorum's.
