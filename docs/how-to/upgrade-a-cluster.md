# Upgrade a cluster one member at a time

Move a running cluster of three members to a newer build of Cavalry without
stopping the cluster: one member at a time, each on the directories it has. The
cluster commits throughout, but for about 11 s when its leader is stopped, which
is how long the others take to elect the next. The
rule this page follows is in
[`spec/cluster.md`, "Roles and the election"](../reference/cluster.md#roles-and-the-election),
under "Versions and the upgrade", and
[ADR 0136](../decisions/0136-run-the-versions-of-one-major-as-one-cluster-and-upgrade-from-the-last-tree-of-the-version-before.md)
has why.

## Prerequisites

- A cluster that runs as [Run a cluster of three members](run-a-cluster.md) starts
  it. `cavalry doctor` on each member's directory, with the member's two variables
  set, prints one `note cluster-member` line that says `election closed`, and no
  `WARN` or `FAIL` line whose check begins `cluster-`. Every member runs: do not
  begin an upgrade with a member out.
- The newer build on every host: the `cavalry` binary, and the program of the
  service beside each member, built against the same tree. A member's driver, its
  consensus module and its service are of one build.
- Both builds of a version of the cluster protocol whose major is 0. A build's
  version is the number in the title of `spec/cluster.md` in the tree it was built
  from: version 0 and version 1 are both of major 0, and form one cluster.
- Each member's command of [Run a cluster of three members](run-a-cluster.md),
  step 3, unchanged: the same `CAVALRY_CLUSTER_MEMBERS`, the same
  `CAVALRY_CLUSTER_MEMBER_ID`, the same `--dir` and the same `--log-dir`. A member
  is started on the log directory it had. A member started on a new one has lost
  its log.

## Steps

1. Find the leader. Ask each member what it is:

   ```sh
   CAVALRY_CLUSTER_MEMBER_ID=0 target/release/cavalry doctor --dir /tmp/cavalry-member-0 --log-dir /tmp/cavalry-member-0-log
   ```

   One member's `note cluster-member` line says `the leader in term`, and the two
   others say `a follower in term` and name it: `it follows member 2`. Write down
   the leader's id, its term, and the `commit position` of its line.

2. Stop one follower, and the service beside it, with Ctrl-C or SIGTERM. The member
   prints `cavalry run: stopping` and `cavalry run: closed`. The two others are a
   quorum and go on.

3. Start that member from the newer build with its own command, and then its
   service from the newer build:

   ```sh
   CAVALRY_CLUSTER_MEMBER_ID=1 /opt/cavalry-new/cavalry run --dir /tmp/cavalry-member-1 --log-dir /tmp/cavalry-member-1-log
   ```

   Here `/opt/cavalry-new/cavalry` stands for the newer build's binary. The member
   prints `cavalry run: cluster member 1 started`. It reads back the log it kept
   and catches up with what the others committed while it was stopped.

4. Wait for it. Run step 1's `doctor` on that member until its line says `a
   follower in term`, the leader's term, `election closed`, with the leader's
   `commit position` or a later one. Do not stop another member before it does:
   two members that are out at once leave no quorum, and the cluster commits
   nothing until one is back.

5. Do steps 2 to 4 for the other follower.

6. Do steps 2 to 4 for the leader, last. When it stops, the two others wait the
   leader heartbeat timeout, 10 s, and then elect one of them. About 11 s after
   the stop, `doctor` on one of them says `the leader in term` with the next term.
   Nothing is committed in those seconds. A client's session moves to the new
   leader by itself. The member you stopped starts as a follower of that leader.
   Upgrading the leader last costs the cluster one election. In any other order it
   can cost up to three: each stop of a member that leads at that moment holds
   one, and the member that wins an election may be one you still have to stop.

To step back to the older build, do the same steps with the older build's binary.
No file that a member keeps differs between version 0 and version 1.

## How to verify it worked

- `cavalry doctor` on each member's directory prints one `note cluster-member`
  line with `election closed`: one says `the leader in term`, two say `a follower
  in term` with the same term, and all three give the same `commit position` once
  nothing is being appended.
- No line of `doctor` is a `WARN` or a `FAIL` whose check begins `cluster-`. The
  two members that elected at step 6 each print one warning for the leader you
  stopped, once: `WARN cluster.leader_heartbeat_timeouts`, or `WARN
  cluster.log_images_lost` from a member that noticed the loss as its image of the
  leader's log going. Both mean that the leader was gone, and that is the
  upgrade's one election.
- A client that held a session through the upgrade still has its answers: a
  message it sent before a step was answered once, and its session is open with
  the leader of the last term.
- Each `cavalry run` process is the newer build's: its command line names the
  newer binary.

## When it does not work

- A member does not start after its step, and prints `cavalry run: member refused`
  or `cavalry run: member stopped` with a reason. The reasons are those of any
  start: [Run a cluster of three members](run-a-cluster.md#when-it-does-not-work)
  has each, with what to do. Until that member runs, upgrade no other.
- A member's line stays `note cluster-joining` and never says `election closed`.
  It is catching up. The same page says how long that may take and what its
  counters show.
- After the leader's stop, no member says `the leader in term`, and `doctor`
  prints `WARN cluster-no-leader`. Fewer than two members run: check that the
  member of the step before came back, as step 4 asks, and start it.
- The two builds are not of one major. The newer member's messages are rejected by
  the older ones, counted in each older member's `cluster.messages_rejected`, and
  it never follows their leader. Such builds do not upgrade in place: stop the
  newer member, start the older build on its directories again, and bring the
  cluster to the newer build as a new cluster, every member on a new log
  directory.
