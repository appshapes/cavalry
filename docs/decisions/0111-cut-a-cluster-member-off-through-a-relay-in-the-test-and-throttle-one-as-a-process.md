# 0111. Cut a cluster member off through a relay in the test, and throttle one as a process

- **Status:** Accepted
- **Date:** 2026-11-12
- **Invariants affected:** 1, 3, 6
- **Supersedes:** none. Decides what ADR 0100 left to its seventh change, how a
  test cuts a member of a cluster over drivers off, and how the throttle test of
  ADR 0052's fourth feature throttles a member on a runner that has no cgroup. It
  changes no page of `spec/` and no code outside
  `crates/cavalry-raft/tests/`.

## Context

Gate 3 names fault injection for loss, reordering and CPU throttling. For the
cluster, ADR 0052 asks for tests on the loopback that kill the leader, partition
a member "through the lossy shim" and throttle one "with the cgroup the driver
already degrades under", run by `just test` on both runners. ADR 0077 says what
they assert: nothing committed is lost, and the cluster is available while a
quorum remains. The first is `tests/cluster.rs`, which closes the leader's
driver. The other two need something the tree does not have.

**No driver can be asked to lose or cut a datagram.** ADR 0100's Consequences say
so, and leave the choice "between a relay in the test and a facility of the
driver" to this decision. The shim ADR 0052 names is in
`crates/cavalry-client/tests/udp_receive.rs`: one socket pair between one
publisher and one subscriber, in that test's file. A cluster of three sends on
three endpoints of each peer, in both directions: eighteen roads.

**A member sends where its members string says.** A peer's `consensus`, `log` and
`catchup` endpoints are fields of that string (`spec/cluster.md`, "Members and
streams"), and nothing else tells a member where a peer is.

**A CPU limit is a process's, and one runner has no cgroup.** Every other test
of the crate runs its three drivers in the test's own process. The macOS runner
has no cgroup at all. A test on Linux can set one on a process only with cgroup
v2, `systemd-run` and `sudo` without a password, and a host may refuse any of the
three.

## Decision

**A member is cut off by a relay that the test runs**, `tests/relay/mod.rs`. For
each pair of members, each direction and each of the three endpoints, the relay
binds a port of its own, a link. What a link receives it sends on to the peer's
own endpoint, and what the peer sends back to that socket, status messages and
NAKs, it sends back to the sender. Each member is given a members string of its
own: its five endpoints as they are, and each peer's three as the ports of the
links to that peer. The strings name the same members at the same ids and differ
only in the road, which only a test does.

**The relay loses and reorders for the whole of each test.** Of the data frames
a link carries, one in 23 is lost and one in 7 is held back until the next has
gone, or 2 ms have. Setup frames, heartbeats, status messages and NAKs pass, as
in the client's shim, so what a fault costs is mended by a NAK and a
retransmission. A cut is on top of that: while a member is cut, every datagram to
it and from it is lost on every link, which is a partition with that member
alone on its side.

**Two tests use it**, `tests/partition.rs`. A follower is cut off, canvasses
alone and unseats nobody, and when the cut is mended joins the log and holds what
was committed without it. A leader is cut off with five entries no follower
received; the other two elect one of them and commit a hundred entries; mended,
the member that led truncates the five, replicates what the two committed and
follows. In each, a service beside every member reads the same entries at the
same positions.

**A member is throttled as a process of its own**, `tests/throttle.rs`, a test
program with no harness. Each member is that program started again with its
member id in the environment: one driver and one consensus module. The test
reads the members as an operator does, their state from each driver's counters
and their logs through a service's handle.

**The throttle is the kernel's where the runner allows it, and stop and continue
elsewhere.** On Linux with cgroup v2, `systemd-run` and `sudo` without a
password, each member runs in a transient scope and the test sets `CPUQuota=5%`
on one follower's scope for five seconds; it reads the scope's `cpu.stat` and
fails when the kernel throttled no period. Elsewhere it stops that member's
process for 95 ms of every 100 ms with `SIGSTOP` and `SIGCONT`, sent to the
process by its id. The test prints which of the two ran, and why.

**The tests assert what must hold and print what it costs.** They assert that
entries are committed while a quorum remains, that a member cut off or throttled
unseats nobody, and that afterwards every member's service reads every committed
entry. Under the throttle, "while a quorum remains" is asserted where it can be
seen: with the member stopped outright, the leader's commit position must rise in
at least one stop, which a cluster that needed all three members would not do.
Under a cgroup's quota the member runs a part of every period and paces the
leader (`fc=min`), so no moment shows the two others alone, and there the test
asserts that commits continue. They print, with no bound asserted, how long a member cut off took to join
again and how many entries a second the cluster committed under the throttle.
Both are costs of rules the pages already have, and a bound on either would be a
guess at a runner's speed.

## Alternatives considered

- **A facility of the driver: a loss rate, or a list of peers to drop.** Lost. It
  is a branch for each datagram on the sender's or the receiver's path, there for
  tests alone, and a key that an operator can set wrong (invariant 5). It would
  also test the driver's account of a network, not a network. In Aeron, this is
  the driver's debug channel endpoints, `DebugSendChannelEndpoint` and
  `DebugReceiveChannelEndpoint` with a `LossGenerator`, set by properties of
  `DebugChannelEndpointConfiguration` (`io.aeron.driver.ext`, read on 2026-11-12).
  Cavalry departs from it for the two reasons above: a key that is there for
  tests is a key to set wrong, and a zero-configuration driver has no place for
  one.
- **The operating system's own tools**: a packet filter, or a traffic control
  queue. Lost. Each needs root on the runner and is a different tool on Linux and
  on macOS, and a rule on the loopback reaches every other test that runs beside
  this one.
- **The client's shim, as it is.** Lost. It is one link in one test's file. The
  relay keeps its rule, that only data frames are lost or held, and serves
  eighteen links from one thread.
- **The in-memory cluster's cuts alone** (`memory::Cluster::cut_between`). Lost.
  The election's tests use them, on a clock the test steps. They have no driver,
  so no image, no liveness timeout and no cool-down: the ten seconds in Evidence
  would not have been seen.
- **A cgroup or nothing.** Lost. The test would not run on the macOS runner, and
  the card asks for both runners.
- **Stop and continue on every runner.** Lost. It stops the member whether or
  not it had work, where a quota stops it once it has used its share; where the
  kernel's own throttle can be had, the test uses it.
- **Throttle a member's threads inside the test's process**, by sleeping in its
  duty cycle. Lost. That slows the consensus module and leaves the driver's
  sender, receiver and conductor running, which a CPU limit does not.

## Evidence

On an Apple M5 Pro laptop, macOS 26.6, twenty runs in a row of `tests/partition.rs`
and `tests/throttle.rs`, all passed.

- A leader cut off: the two that remained had a leader 10.18 s to 10.53 s after
  the cut, in term 1 or 2, which is the leader heartbeat timeout of 10 s and an
  election.
- A member cut off and mended, 40 values: it held the log the others had
  committed 9.78 s to 10.39 s after the mend. A member cut off for the image
  liveness timeout has lost its images of the others' streams, and a driver
  makes no second image of a session whose image it retired until the image
  cool-down of 10 s has passed (`spec/udp.md`, "The receiver", step 5). The
  tests mend the cut at the start of that cool-down, the longest the wait can
  be.
- A follower given a twentieth of a processor for 5 s, by stop and continue: the
  cluster committed 29,664 to 30,200 entries of 96 bytes a second before it,
  11,668 to 13,012 under it and 35,717 to 39,997 after it, with no election. The
  leader's log is sent under `fc=min`, so it goes no further than one receiver
  window of 128 KiB past the slower follower's position, about one window for
  each 100 ms the throttled member runs in (ADR 0052, "Harder").
- Not shown here: the cgroup's path, which a macOS laptop cannot run. The line
  the test prints in the Linux runner's log says which path ran there.

## Consequences

- Gate 3's cluster tests run with `just test` on both runners, about 40 s more of
  it.
- Two costs are now measured and in the crate's README: a member cut off for 10 s
  or more joins again about 10 s after the cut is mended, and one throttled
  follower slows the cluster's commits to about one receiver window each 100 ms,
  though the other two members are a quorum.
- The relay follows the members string's shape. A change to the endpoints of a
  member (`spec/cluster.md`, "Members and streams") is a change to the relay.
- The throttle test needs `sudo` without a password to use the kernel's
  throttle, and says so in its output when it falls back.
- Revisit when a leader's log can be sent under a flow control that does not wait
  for its slowest follower: the throttle test then asserts the rate it prints
  today.
- Revisit when the driver's image cool-down changes: the time to join again in
  Evidence is that value.

## References

- `crates/cavalry-raft/tests/relay/mod.rs`, `tests/partition.rs`,
  `tests/throttle.rs`.
- [ADR 0052](0052-build-raft-on-the-durable-log-fifth.md), the fourth feature and
  "Harder".
- [ADR 0077](0077-elect-the-leader-by-vote-and-close-a-ballot-on-the-members-heard.md),
  what the tests over three drivers assert and report.
- [ADR 0100](0100-move-the-module-to-the-election-in-ordered-changes-around-one-that-takes-the-appointed-path-out.md),
  the seventh change and Consequences.
- `spec/udp.md`, "The receiver", step 5, the image cool-down.
- `spec/cluster.md`, "Members and streams".
