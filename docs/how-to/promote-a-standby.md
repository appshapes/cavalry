# Promote a standby to a voter

A how-to page: one task, change which members of a running cluster have a vote. You
give a standby a vote, or you make a voter a standby, by a change of the members
string, and you start the members again with it one at a time. A standby is a member
that follows the log and has no vote ([glossary](../glossary.md)). What a standby
does and the rules of the change are
[`spec/cluster.md`](../reference/cluster.md), "Standbys" and "Changing a standby's
mark"; why they are these rules is
[ADR 0134](../decisions/0134-mark-a-standby-in-the-members-string-and-change-one-mark-at-a-time.md).
In Aeron, this is Cluster Standby ([glossary](../glossary.md)).

The page runs four members on one host: voters 0, 1 and 2, and member 3, a standby.
It promotes member 3. Then it shows the two changes that replace a lost voter.

## Prerequisites

- A cluster that runs, started as [Run a cluster of three members](run-a-cluster.md)
  starts one, with a fourth member whose entry in the members string ends in
  `,standby`:

  ```sh
  export CAVALRY_CLUSTER_MEMBERS='0,127.0.0.1:9000,127.0.0.1:9001,127.0.0.1:9002,127.0.0.1:9003,127.0.0.1:9004|1,127.0.0.1:9010,127.0.0.1:9011,127.0.0.1:9012,127.0.0.1:9013,127.0.0.1:9014|2,127.0.0.1:9020,127.0.0.1:9021,127.0.0.1:9022,127.0.0.1:9023,127.0.0.1:9024|3,127.0.0.1:9030,127.0.0.1:9031,127.0.0.1:9032,127.0.0.1:9033,127.0.0.1:9034,standby'
  CAVALRY_CLUSTER_MEMBER_ID=3 target/release/cavalry run --dir /tmp/cavalry-member-3 --log-dir /tmp/cavalry-member-3-log
  ```

  The three voters elect a leader as three members do. The standby follows it.
- Every member on a version of Cavalry that knows the mark. A member of a version
  before it refuses the string, `members-grammar`, and does not start.
- `cavalry doctor` on every member that runs prints one `note cluster-member` line,
  with `election closed`, and no `WARN` or `FAIL` line whose check begins `cluster-`.
  The standby's line says `a standby, which follows the log and has no vote,` and the
  same `commit position` as the leader's line: it has caught up.
- One place that holds the members string for every host, such as the file your
  deployment sets `CAVALRY_CLUSTER_MEMBERS` from. After this page, no member may
  start with the older string again.

## Steps

1. Write the new string. It is the string the cluster runs with, with one member's
   mark changed and nothing else. To promote member 3, take `,standby` off its entry:

   ```sh
   export CAVALRY_CLUSTER_MEMBERS='0,127.0.0.1:9000,127.0.0.1:9001,127.0.0.1:9002,127.0.0.1:9003,127.0.0.1:9004|1,127.0.0.1:9010,127.0.0.1:9011,127.0.0.1:9012,127.0.0.1:9013,127.0.0.1:9014|2,127.0.0.1:9020,127.0.0.1:9021,127.0.0.1:9022,127.0.0.1:9023,127.0.0.1:9024|3,127.0.0.1:9030,127.0.0.1:9031,127.0.0.1:9032,127.0.0.1:9033,127.0.0.1:9034'
   ```

   Change one mark and no more. Do not add a member, remove one, or change an id or
   an endpoint: a cluster keeps its members for as long as it keeps its log.

2. Check the string, and see which members run with the older one:

   ```sh
   CAVALRY_CLUSTER_MEMBER_ID=0 target/release/cavalry doctor --dir /tmp/cavalry-member-0 --log-dir /tmp/cavalry-member-0-log
   ```

   You will see no line that begins `FAIL cluster-members`, and one that begins `WARN
   cluster-voters`: `member 0 runs with a members string that gives a vote to members
   0, 1 and 2, and CAVALRY_CLUSTER_MEMBERS on this host gives one to members 0, 1, 2
   and 3`. That line is this page's progress. A member has it until it is started
   with the new string.

3. Find the leader. Run the command of step 2 for each member, with its own id and
   directories. One `note cluster-member` line says `the leader`. Write down its term,
   the number after `in term`.

4. Start each member that is not the leader again with the new string, one at a
   time, the standby first. Stop it with Ctrl-C or SIGTERM, wait for `cavalry run:
   closed`, and start it with the same id and the same two directories:

   ```sh
   CAVALRY_CLUSTER_MEMBER_ID=3 target/release/cavalry run --dir /tmp/cavalry-member-3 --log-dir /tmp/cavalry-member-3-log
   ```

   It prints `cavalry run: cluster member 3 started`. Within a second, `doctor` on it
   says `election closed` and `it follows member`, with the leader's id, and prints no
   `WARN cluster-voters`. Then do the same for the next member. Do not stop two
   members at once: with one voter stopped, the others are still a quorum.

5. Start the leader again with the new string, last. The other members hear nothing
   from it for 10 s, the leader heartbeat timeout, and the members elect a leader.
   About 11 s after you stopped it, `doctor` on one member says `the leader`, with a
   term above the one you wrote down in step 3.

6. Wait until the change is complete, and make no other change before it is. It is
   complete when `doctor` on every member says `election closed`, exactly one says
   `the leader`, and none prints `WARN cluster-voters`. The member you promoted now
   says `a follower` or `the leader`, and no longer `a standby`.

## Replace a lost voter with a standby

A voter whose host is gone for good still has its vote. A cluster of three voters
that has lost one needs both of the others for every commit and every election. To
give the standby that vote, make two changes, each by steps 1 to 6, the first
complete before the second begins:

1. Make the lost voter a standby: add `,standby` to its entry. The cluster then has
   two voters, and needs both, as it does now. The lost member does not run, so
   steps 4 and 5 start the three that do. While the one voter that follows is
   stopped in step 4, the leader has no quorum: start that voter again at once. If
   more than 10 s pass, the leader stops leading, and the two voters elect a leader
   when both run again. Nothing committed is lost.
2. Promote the standby: take `,standby` off its entry. The cluster then has three
   voters and needs two.

Do not make both changes in one string. Two strings that differ by two marks can
each elect a leader with no voter in common, and the second leader then lacks what
the first committed. `spec/cluster.md`, "Changing a standby's mark", has the
example, and a test of `cavalry-raft` runs it.

## How to verify it worked

- `cavalry doctor` on every member, with the new string set, prints one `note
  cluster-member` line with `election closed`, and no `WARN cluster-voters`.
- Exactly one member says `the leader`. Its term is above the term the cluster had
  before the change.
- Every member reports the same `commit position` once its recorder has synced.
- The promoted member votes. To see it, stop the leader: about 11 s later one of the
  members that still run says `the leader`, in a higher term. Of four voters, three
  must run for that, so the promoted member's vote is one of them.

## When it does not work

- `WARN cluster-voters`: this member runs with another string than the one in
  `CAVALRY_CLUSTER_MEMBERS` on its host. During steps 4 and 5 that is a member you
  have not started again yet. After step 6 it is a member that was started with an
  older string, or a host whose variable is not the cluster's string: correct the
  variable, and start the member again.
- `WARN cluster.standby_vote_requests`: a candidate asked this member for a vote,
  and this member is a standby by its own string. Two members run with strings that
  differ. Between step 4 and step 6 that is expected, and the election took 1 s
  longer for it. With no change under way, find the member that prints `WARN
  cluster-voters` and start it with the cluster's string.
- `FAIL cluster-members`, with `members-voters`: every member of the string is
  marked `standby`, so no member can lead. Take `,standby` off the members that
  vote.
- `WARN cluster-no-leader` on every member after step 5, for longer than a minute:
  fewer than a quorum of the new string's voters run or reach each other. The line
  names the quorum, such as `3 of 4`. A cluster of four voters needs three. Start the
  members that are stopped, with the new string.
- A member that was away during the change starts with the string it had. Give it
  the new string before you start it. It then follows the leader and replicates
  what it missed.
- Anything else a member or `doctor` prints:
  [Run a cluster of three members](run-a-cluster.md#when-it-does-not-work).
