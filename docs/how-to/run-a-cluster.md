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
  others, has no record of them. It takes a leader only once it has heard every
  other member, so it waits, and `cluster.leaders_withheld` rises, while another
  member is stopped. Then it replicates the log, and it withholds its vote and seeks no
  leadership until it has caught up, joined its leader's live log, and reached the
  position its leader answered it with, which clears the vote floor
  `cluster.vote_floor` holds and counts it in `cluster.vote_floors_cleared`
  ([ADR 0145](../decisions/0145-withhold-the-vote-of-a-member-that-lost-its-log-until-it-holds-what-the-cluster-held.md),
  [ADR 0161](../decisions/0161-hold-a-members-vote-floor-until-it-has-joined-the-live-log-and-then-clear-it.md),
  [ADR 0163](../decisions/0163-clear-a-vote-floor-only-on-a-leaders-answer-to-the-present-run.md)).
  Until it has caught up, the cluster tolerates one failed member fewer. That is
  safe while each other member holds the log it kept, a new leadership term that the
  leader's sender held back by flow control included, and a report of its earlier run
  that reaches the leader late included. Beside another member that holds no log
  either, because it started late and has not yet caught up, while the member that
  holds the log is stopped, the two elect nothing: the member on the empty directory
  waits for every member's report, and `cluster.voters_unheard` counts the members it
  waits for. Once the stopped member runs again, it leads
  ([ADR 0174](../decisions/0174-have-a-member-that-holds-nothing-vote-only-once-every-voter-has-reported.md)).
  A volume spares the wait.
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
   member <id> started`. Start all three, from one build of `cavalry`: a build from
   before work item 476 votes at a new cluster's start as this one does not, and
   nothing tells the two apart. A new cluster elects its first leader only
   once every member has started: three members that have heard one another elect a
   leader within a second, and two that run while the third has not started wait
   for it, for as long as it takes. Before its first vote a member of a new cluster
   holds nothing, its log empty and no
   [candidate term](../glossary.md) stored, and it votes only once it has heard
   every member (work item 476).
   Once every member has followed the first leader, a member that stops or starts
   late no longer holds the others back: two of three elect while the third is
   stopped, and the third catches up from the leader when it starts. If the first
   leader stops before every member has followed it, the others can wait for it:
   start it again.

4. Ask each member what it is:

   ```sh
   CAVALRY_CLUSTER_MEMBER_ID=0 target/release/cavalry doctor --dir /tmp/cavalry-member-0 --log-dir /tmp/cavalry-member-0-log
   ```

   You will see a line that begins `note cluster-member`. Run the command for members
   1 and 2 too, with their own id and directories. The member that leads also prints
   a line that begins `note cluster-no-service`: no program runs beside it yet, and
   there is nothing to do. One of the three lines says `the
   leader`: `this driver is cluster member 2, the leader in term 0, election closed:
   append position 96, commit position 96; its last election began because the member
   started`, when member 2 was elected. The other two say `a follower in term 0,
   election closed`, with the same two positions once their recorders have synced,
   which each member asks its recorder for as soon as an entry is written, and name
   the leader: `; it follows member 2; its last election began because the member
   started`. Every line then names the members that the member's own string gives a
   vote to, `; its members string gives a vote to members 0, 1 and 2`, the same on
   the three, and the version of the cluster protocol that the member runs, `; it
   runs version 3 of the cluster protocol, 0.3.0`. The term is 1 or more where two
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

   Do the same for a member that was stopped, whose process was ended, or whose host
   lost power or whose operating system crashed while it ran. After such a crash its
   driver cuts each recording that was being made to the position the recording had
   synced, and counts each cut in `recording.cut_to_synced`. While that driver runs,
   `cavalry doctor` on it prints a `WARN` line for the counter, which explains the
   cut and asks nothing of you unless the count climbs at every start: then the
   line says to find what stops the driver without `close`. A host that lost power
   starts again, and the directories of this page are under `/tmp`, which macOS and
   many Linux systems empty when a host starts. When this page's one host empties
   it, the three members' directories go together, and no member is left with the
   cluster's log: start the three again as a new cluster, with the commands of step
   3, as step 6 does. What the cluster had committed is gone with them. On three
   hosts, keep each log directory on storage that outlives its host, as the
   prerequisites say. A member whose log directory is gone all the same starts as
   one whose directory has no `log-recordings` file, under "When it does not work",
   while the other two run. That section, under "The member's host lost power", also
   names the one member that starts on a new log directory instead: one that ran a
   driver of an earlier version when the power was lost.

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
  the leader was offered, in order (`scripts/replay-check.sh`). Its last step holds
  a service's timers the same way: three members with a program beside each that
  schedules 200 timers, one program started again from its snapshot, the leader
  stopped while half of them are pending, and a program that reads the log from
  position 0, and the gate fails unless the two members left and that replay were
  handed the same 200 timers, each once, at the same positions and times.
- A program beside a member reads the entries as the cluster commits them:
  [Read a cluster's committed log from a service](read-the-committed-log.md).
- To have the cluster take a snapshot now, and not at the next 256 MiB of its log:
  [Ask a cluster for a snapshot](ask-for-a-snapshot.md).

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
  member while the other two run. To have three members again, start this member
  on a new log directory while the other two run. It takes a leader once it has
  heard both, joins them and replicates the log, and it withholds its vote and seeks no
  leadership until it has caught up and joined its leader's live log: then
  `cluster.vote_floor` on its host reads −1 and `cluster.vote_floors_cleared` reads 1.
  While one of the other two is stopped it takes no leader,
  and `cluster.leaders_withheld` on its host rises: start the stopped member on the
  log directory it kept. It catches up only while
  the leader's log begins at position 0. Where the other members' recordings were
  purged below a snapshot, start the cluster again as a new cluster: stop every
  member, give every member a new log directory, and start them together.
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
- `member refused (log-directory-held)`: another process holds the lock a
  consensus module takes on the member's log directory. Normally a member runs on
  it, started by `cavalry run` or by a program that embeds a member as a client of
  the host's driver, with this member id or another. A log directory holds one member's files, so this member did not start,
  and it wrote nothing. `cavalry doctor --log-dir <path>` names the holder's pid on
  Linux (`log-directory-held`), and `lsof <path>` names it on any host. Stop that
  process before you start this member on the directory, or start this member with a
  driver and a log directory of its own. A member whose process ended, however it
  ended, holds nothing, so a killed member starts again at once.
- `member refused (log-directory-lock)`: the member's log directory could not be
  opened, or its filesystem refused the lock by which a member holds it. Nothing was
  written, and the member does not start without the lock, as a driver does not
  start without its own. A network filesystem refuses it: on NFS an exclusive lock
  of `flock(2)` needs a descriptor open for writing, which a directory never is, so
  a member that ran on NFS before ADR 0152 meets this at its first start under it.
  `cavalry doctor --log-dir <path>` warns of such a directory in
  `log-directory-backing`. Give the member a log directory on a disk of this host:
  stop its driver, copy the whole directory there, and start the driver and the
  member on the copy. In an upgrade one member at a time, do this for each member
  before you upgrade it.
- `cavalry run: member refused (listed-recording)`: the `catalog` in the member's
  log directory does not hold the recordings that its `log-recordings` file names.
  The message names the recording and what the catalog holds in its place. The
  directory is not as the member left it: files were removed from it, or it was put
  together from two directories. Start the member with the log directory it had,
  whole. If that directory is lost, do as for a directory with no file, above.
  One of its messages can come from a directory that nobody changed: `the
  recordings before it ending at` a position, `below where it begins`. A
  truncation of this member's log stopped on a fault part way, and the member then
  led a term or joined one, so its file names a recording that begins past the cut.
  A member of a build since ADR 0148 does neither until the truncation finishes
  (`WARN cluster.truncation_owed`, below), so this comes from a directory an older
  build ran on. Start this member on a new log directory, as for a directory that
  is lost.
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
  ran. Start the member again on its log directory, as after any stop. When its
  driver opens the log directory, it cuts each recording that was being made to the
  position the recording had synced, and drops the frames past it, which the crash
  may have left with no body. `recording.cut_to_synced` on that driver counts each
  recording it cut, and `cavalry doctor` explains it. The member's log then holds
  every entry the member reported and nothing past it, and the member takes what it
  lacks from its leader. The same holds when every host lost power together
  ([ADR 0144](../decisions/0144-keep-a-recordings-synced-position-in-its-time-index-and-cut-a-recording-a-crash-left-live-to-it.md)).
  This holds for a recording that this version's driver was making. A driver of an
  earlier version, version 4 of the durable log or before, kept no synced position,
  so a recording it was making when the power was lost cannot be cut. No release of
  Cavalry carries such a driver: it is a build of `master` that does not hold commit
  dfbbe930, `287: Cut a recording a crash left live to the position it had synced`,
  of 2026-11-22. The date alone does not tell: `master` has earlier commits of that
  day. In the checkout the driver was built from, at the commit it was built at,
  `git merge-base --is-ancestor dfbbe930 HEAD` exits with 0 when the build holds
  that commit. It exits with 1, or prints a `fatal:` line and exits with 128, for
  such a build. A shallow checkout lacks old commits and prints `fatal:` for any
  build: run `git fetch --unshallow` in it first.
  When the member ran such a driver at the loss, do not start it on that log
  directory again, even under a newer driver: start it on a new log directory, as
  for a directory with no `log-recordings` file, above.
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
- `WARN run.member_refused` in `doctor`, beside `FAIL driver-heartbeat`: the driver
  that made this directory closed because its cluster member's start failed. The
  member never ran a duty cycle, so none of its own counters is in the directory:
  this count, which `cavalry run` adds to the driver's file before it closes the
  driver, is the only sign there. Read the line the process printed after the driver
  started; under systemd, `journalctl -u` with the unit reads it again. A line
  beginning `cavalry run: member refused (` or `cavalry run: member stopped (` names
  the reason in its brackets, which an entry above explains. A line beginning
  `cavalry run: the cluster member's thread could not start` means the host had no
  thread to give the member: free one. Correct what the line names, then start
  `cavalry run` again with the same directories.
- `WARN cluster-no-leader`: the member is in an election and has no leader to follow.
  The message says which state its election is in and why it began, from
  `cluster.election_cause`. A leader needs the votes of two of three members. A
  member that holds nothing, its log empty and no candidate term stored, as a new
  cluster's members do before their first vote, waits until it has heard every other
  member, for as long as that takes: start the member it has not heard, a first
  leader that stopped among them. At a start, a member that holds a log or a
  candidate term waits 60 s, the startup canvass timeout, for a member it has not
  heard, before two of them elect. Run `doctor` on each member's
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
  version that member runs, in its `note cluster-member` line: `it runs version 3 of
  the cluster protocol, 0.3.0`, where the major is the first number of `0.3.0`. Of
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
  entry says.
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
- `WARN cluster.vote_floor`: this member took a leader with its log empty, and since
  it started it has not caught up past its vote floor and its leader's answer to its
  present run, and joined its leader's live log. The line says one of five things:
  - how many bytes its `cluster.append_position` is short of its vote floor;
  - that it runs a build before ADR 0161, which never clears its floor: start it on
    the last build of version 2 of the cluster protocol before an upgrade to version 3,
    since a build of version 3 withholds on that floor until a leader of version 3
    answers it;
  - that no leader has answered it yet: a leader of a version of the cluster protocol
    before 3 answers none, so it waits for a leader of version 3 (see
    [Upgrade a cluster](upgrade-a-cluster.md));
  - how many bytes it is short of its leader's answer, which it reaches as it
    catches up;
  - or that it has not yet joined the live log.

  It lost its log directory, it joined a new cluster after the first term began,
  or it is a new cluster's follower that has not been answered yet. Until its floor
  clears, it withholds its vote and seeks no leadership, so the cluster elects a
  leader only while both other members run. A member at or past its floor withholds still
  after a start that came before it joined: it waits for the live log too, not for
  the position alone. It catches up and joins from its leader by itself, and the line goes once
  it has. If its leader is lost for good before it joins, no member that prints this
  line can stand in the election that follows. First start every stopped voter on
  the log directory it kept: a quorum of voters that hold the log elects without this
  member. Then:
  - When every voter but the lost leader runs and prints this line with a
    `cluster.append_position` of 0, as a new cluster's followers do whose first leader
    was lost, no voter that runs holds an entry: whatever the cluster committed is on
    the lost leader alone. Start the cluster again as a new cluster, every member on a
    new log directory, which gives up what that leader held.
  - When the members were upgraded all at once from a build before ADR 0161, each
    holds the floor that build stored and never cleared. With every voter running,
    start again on that build before ADR 0161 the member whose
    `cluster.append_position` is at least every other's: it votes and stands once its
    log is at or past its floor. Start the others on the last build of version 2 of
    the cluster protocol, a build of ADR 0161, which votes for it by the one voting
    case and clears its floor once it has joined that leader's live log, with no
    answer needed. Then start that member on the same build of version 2, and once
    every member's line has gone, upgrade to version 3 one member at a time
    ([Upgrade a cluster](upgrade-a-cluster.md)). A member of version 3 started beside
    a leader before version 3 would wait for an answer that never comes.

  If the bytes short stay the same while the cluster appends, its leader's log
  no longer begins at position 0, and it cannot catch up: start the cluster again as a
  new cluster, every member on a new log directory
  ([ADR 0145](../decisions/0145-withhold-the-vote-of-a-member-that-lost-its-log-until-it-holds-what-the-cluster-held.md),
  [ADR 0161](../decisions/0161-hold-a-members-vote-floor-until-it-has-joined-the-live-log-and-then-clear-it.md),
  [ADR 0163](../decisions/0163-clear-a-vote-floor-only-on-a-leaders-answer-to-the-present-run.md)).
  A standby that starts on an empty log directory stores a vote floor too, and
  `cavalry doctor` prints no line for it: a standby has no vote to withhold. Once
  it is promoted to a voter ([Promote a standby to a voter](promote-a-standby.md)),
  the line applies to it as written.
- `WARN cluster.votes_withheld`: this member, holding a vote floor or with its log
  empty, answered a candidate's request with no vote, and the count says how many
  times. A vote from it could have elected a leader that lacks an entry the cluster
  committed with its acknowledgement before it lost its log. An election succeeds
  without it while a quorum of the other members votes. If no leader is elected
  while the count rises, a member that holds the log is stopped: start it again on
  the log directory it kept, and the cluster elects once it runs. The count stays
  until this member starts again. When this member's `note cluster-member` line
  says `election closed` and `cavalry doctor` prints no `WARN cluster.vote_floor`
  for it, the count is history: the member has caught up and votes again.
- `WARN cluster.leaders_withheld`: this member started on an empty log directory,
  a leader answered it, and it has not taken that leader yet; the count says how
  many times it passed one over. It takes a leader only once it has heard every
  other voter, since only a voter it has not heard could show that a later term was
  elected without that leader. Run `cavalry doctor` on each other member's host:
  the member whose driver is stopped, or whose host cannot reach this one, is the
  one it waits for. Start that member on the log directory it kept, or mend the path
  between the two, and this member takes its leader within 100 ms. A member of a
  build before ADR 0145 does not answer it until that member canvasses in an
  election or is upgraded. `cavalry doctor` prints the line only while this
  member's log is empty and it has no leader. The count stays until this member
  starts again, so in a later election it says nothing of why there is no leader.
- `WARN cluster.truncation_owed`: a truncation of this member's log, which a new
  leader asked for, faulted part way, and the member owes it; the line names the
  position it owes and the end it reports. The driver refused a command, did not
  answer in time, or a write to the log directory failed: `cavalry run` printed that
  error for the member. Until the truncation finishes the member neither leads nor
  joins a log, so the cluster elects a leader only while both other members run.
  The member asks for the truncation again at each duty cycle, and the line goes
  once it finishes. While it keeps failing the member reads and sends nothing, and
  `cavalry run` gives it up after 10 s of failed cycles: mend what the error names,
  then start the member again on the log directory it kept. Its start makes that
  directory agree with itself, and the next leader asks for whatever the fault left
  uncut
  ([ADR 0148](../decisions/0148-keep-a-member-whose-truncation-faulted-out-of-the-log-until-it-finishes-the-truncation.md)).
- `WARN cluster.truncations_faulted`: how many truncations of this member's log
  faulted part way. With no `WARN cluster.truncation_owed` line for the member,
  each one finished, and the count is history. It stays until this member starts
  again. A count that rises often names a driver or a disk to look at: the line
  `cavalry run` printed for each fault says which.
- `WARN cluster-no-service`: this member leads and holds timers, which a service
  told it of, and no service runs beside it now. The program beside this member
  has stopped. A member is told of timers only by the service beside it, and only
  a leader appends a timer's event, so where the program runs beside the other
  members, its timers wait while this member leads. Nothing answers what is
  appended in the term meanwhile either: a reply is sent by the service beside
  the leader. Start the program beside this member. A client whose message got no
  reply sends it again.
  `cavalry doctor` reads this from the driver's counters: a service adds counters
  of its own there, keyed by its member's id, `cluster.replies_dropped` when it
  first opens its member's log, and holds them until it closes. A program that
  only reads the committed log, as that of [Read a cluster's committed log from a
  service](read-the-committed-log.md) does, adds none of them and is not seen as
  one.
  A program that was killed, with no close, is seen about 10 s later, when the
  driver has timed its client out, and one that started a moment ago has not added
  them yet
  ([ADR 0167](../decisions/0167-have-doctor-name-a-leader-beside-which-no-service-runs.md)).
- `note cluster-no-service`: this member leads, holds no timer, and no service
  runs beside it. In a cluster that runs no program yet, that is how it is, and
  there is nothing to do. Where a program runs beside the other members, start
  it beside this member too: until then its timers wait and nothing answers
  what is appended in the term, as for the warning above. The leader's host
  cannot tell the two cases apart, which is why this is a note
  ([ADR 0167](../decisions/0167-have-doctor-name-a-leader-beside-which-no-service-runs.md)).
- `WARN cluster.timers`: this member leads and holds no timer, while the service
  beside it has timers pending; the line names how many. A service sends its member
  every pending timer again each second, and tells it nothing while it reads its
  history after a start, so a first run of `cavalry doctor` may catch the second
  after a member started or a service that is still reading. When a second run,
  after the service has read its log to its end, says the same, the
  member is not told: an application's timers wait while this member leads, since
  only a leader appends a timer's event. Check that the service beside this member
  runs and reads its log, and that `cluster.messages_rejected` on this host does
  not rise, which is a service of another build than its member
  ([ADR 0151](../decisions/0151-give-a-clusters-service-timers-that-every-service-decides-from-the-log.md)).
- `WARN cluster.timers_cleared`: this member's table of timers was full, 2,048,
  with no timer in it whose event the member had appended. That is more than the
  service beside it can have pending, so some of them were pending nowhere, and
  the member could not tell which: it removed them all and kept the one it was
  sent. No timer is lost: a service sends every pending timer again each second,
  and a timer may be late by that much. A count that keeps rising is a service
  whose cancels do not reach this member: check that the service beside it runs
  and reads its log, and look for `WARN cluster.service_timer_cancels_dropped` on
  this host
  ([ADR 0151](../decisions/0151-give-a-clusters-service-timers-that-every-service-decides-from-the-log.md)).
- `WARN cluster.service_timer_cancels_dropped`: the service beside this member
  held 1,024 cancels that its member had not read, and dropped the oldest for the
  next. No timer is handed to a program wrongly: the member keeps each such timer
  until it appends an event for it, which every service passes over. Check that
  the member runs.
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
