# Where is Cavalry, and what is left before the beta?

This page answers one question: where is Cavalry now, measured from where it started,
and what work is left before a team can use it in place of Aeron. It was read again
for work item 360 on 2026-11-21, against `master` at commit 20741de3. Every state on
this page is the state at that commit, but for a state that a feature changed when it
landed, which gives the date. The line itself is decided in
[ADR 0138](../decisions/0138-draw-the-finish-line-at-a-beta-that-is-an-alternative-to-aeron-and-build-it-sixth.md);
this page is the reading of the repository behind that decision, and the order of the
work that reaches the line. The page was first written on 2026-11-09 for the line of
[ADR 0061](../decisions/0061-draw-the-finish-line-for-early-testers-at-the-first-release.md),
which ADR 0138 supersedes.

Three terms. A *slice* is a part of the product that works from end to end, decided
by one ADR, with its own acceptance test. A *beta user* is a team that would otherwise
run Aeron: it installs Cavalry on its own hosts, writes its service in one of six
languages, runs it for weeks on a network it trusts, and reports what it finds, with
nobody to ask. The *finish line* is the state in which Cavalry is ready for beta
users: version 0.1.0, published.

## Answer

- **Where it started.** On 2026-10-10 the repository held a brief, six invariants and
  nine empty crates. Nothing ran.
- **Where it is.** Forty-two days and 364 commits later, the five slices that the
  product's first sentence names work and are gated in CI: publish and subscribe over
  shared memory, the durable log and replay, UDP between hosts, the C ABI with
  clients in five languages, and Raft replication, which is gated on one host's
  loopback so far. The fifth slice has seven of its eight features. The eighth, a
  gate across three hosts, has not started.
- **The finish line.** Version 0.1.0 as a beta, with nine conditions under three
  milestones (ADR 0138): it is an alternative to Aeron, its cluster is highly
  available and fails over by itself, and Brigade's server keeps working.
- **What is left.** Twelve features, about 115 pull requests. Two protect what
  exists, the latency gate's change and a gate for what Brigade's server runs. Three
  close the cluster's faults. Two give a team that comes from Aeron what it reaches
  for first. One clears debts. One proves the cluster under faults. Three are the
  release itself.
- **When.** An estimate, not a gate: three to five weeks from the day work starts.
  From 2026-11-23 that is 0.1.0 between about 2026-12-14 and 2026-12-28. One of
  those weeks is the soak, a run of seven days on the release candidate that shows
  what a test of seconds cannot, and it cannot be made shorter.
- **What waits on a person.** A runner on the third lab host, an account and a
  credential on each package registry, a restart of one server, and the choice of
  whom to invite. The table below has each.
- **What the plan no longer does.** It measures no pull request on the bench hosts.
  The latency gate runs each night as a coarse check until the slice after the line,
  in which Cavalry is measured against Aeron.

## Where it started

The first commit, 1100d76 on 2026-10-10, held `CLAUDE.md` with its six invariants and
the gates that enforce them, the documentation rules, the ADR template, and
[the brief](why-cavalry.md), which says what the product must beat and keep. By
2026-10-11 the repository had "its invariants, four of its twelve gates, an agent loop
and nine empty crates", and of the promise, transports over shared memory and UDP, a
durable log, Raft replication, a C ABI and clients in five languages, "none of it
exists" (ADR 0007, Context).

Two decisions of the first days shaped everything after them. Work goes in slices,
each with one ADR that orders its features and names its acceptance test, and a slice's
last sentence, its Revisit clause, names the next. And a gate that does not exist is
built by the first feature that needs it, so the gates grew with the product and none
was left for later.

## What was built, slice by slice

| Slice | Decided | Done | What works | Acceptance test |
| --- | --- | --- | --- | --- |
| 1. Publish and subscribe over shared memory | ADR 0007, 2026-10-11 | 2026-10-13, at 2a3d9cb | One [driver](../glossary.md) per host, a Rust client, the `cavalry:ipc` channel, claim and commit, back-pressure returned to the caller, a counter for every transition, `cavalry doctor` | `just quickstart` on Linux and macOS |
| 2. The durable log and replay | ADR 0023, 2026-10-13 | 2026-10-31 | The driver records a stream and replays it into a subscription; truncate, purge and delete; replay is deterministic | `just replay-check`; the ten-minute path records and replays |
| 3. UDP between hosts | ADR 0039, 2026-10-31 | 2026-11-04, at 61fbfdd | `cavalry:udp` unicast on the same API, a sender thread and a receiver thread, loss recovered by the receiver, replay over UDP | The ten-minute path over UDP; the UDP rows of the bench |
| 4. The C ABI and the clients | ADR 0046, 2026-11-05 | 2026-11-06, at f7e1014 | `cavalry-ffi` and its header; Python, Node, Go, JVM and .NET clients generated from the header | `just conformance` passes for all five on both runners |
| 5. Raft on the durable log | ADR 0052, 2026-11-06 | Seven of eight features, at 20741de3 | One publication sent to several destinations; the cluster protocol; the replicated log; election in under a second after the timeout; sessions and services in six languages; catch-up, snapshots and a standby; an upgrade one member at a time | Changed by ADR 0138: the job across three hosts runs each night and for the release candidate, and is feature 9 of the sixth slice |

Beside the slices, the project built the way it works: agents on branches with a
review loop (ADR 0004, ADR 0005), the documentation gate (ADR 0006), a latency gate on
dedicated hosts (ADR 0016, ADR 0034, ADR 0044), a weekly email of the changes (ADR
0026), and its work on the Brigade Board (ADR 0113).

## What the gates hold today

- **Decisions.** 137 ADRs, 0001 to 0137, all Accepted but four that later ones
  superseded.
- **Specification.** Ten pages under `spec/`, each with a version. The client
  protocol is at version 10, the C ABI at 6 since 2026-11-22, the UDP transport at 4, the durable log
  at 6 since 2026-11-22, and the cluster protocol at 1.
- **Code.** 97,279 lines of Rust under `crates/*/src`, unit tests included, and 969
  test functions under `crates/`. `cavalry-raft` is 15,851 lines; on 2026-11-09 it
  was two.
- **Counters.** 179 counters, and 11 state machines with 113 transitions, each
  transition with a counter, in `counters.toml`.
- **Configuration.** Five keys in `config.toml`. Zero configuration is the supported
  default, and the two cluster keys have no default a host could guess.
- **CI.** `gates (linux)` on every pull request and `gates (macos)` on every commit
  of `master`, about 11 and 13 minutes. `just conformance` in them runs ten vector
  suites, six runners with a cluster of three on the loopback, and an upgrade of a
  cluster one member at a time. At this commit three bench jobs on two dedicated hosts
  are required checks; feature 1 below changed that on 2026-11-21.

## The latency rows, and why none is a condition

The rows below are 32-byte messages on `grip`, an Intel Core i7-10700 with isolated
cores. A sample is the time from the publisher's timestamp to the subscriber's
handler. The rows were recorded on 2026-11-15 at 6e4cc7ba from 8 processes, each of 21
runs, and each number is the largest the host measured over those processes, so a
number is the slow edge of the host's band and not a typical run.

| Row | Path | p50 | p99 | p99.99 | max |
| --- | --- | --- | --- | --- | --- |
| `one_way_32_exclusive` | Shared memory, in one process | 90 ns | 136 ns | 398 ns | 711 ns |
| `one_way_32_client` | Shared memory, through the client API | 122 ns | 194 ns | 473 ns | 938 ns |
| `one_way_32_ffi` | The same, through the C ABI | 124 ns | 222 ns | 479 ns | 1,393 ns |
| `one_way_32_recorded` | The same, while the driver records the stream | 110 ns | 187 ns | 495 ns | 1,090 ns |
| `one_way_32_udp` | UDP on the loopback, through the sender and the receiver | 2,475 ns | 2,683 ns | 5,747 ns | 6,591 ns |

Across the cable between the two bench hosts, at 1 Gb/s, the round trip
`udp_round_trip_32` reads p50 111,807 ns, p99 161,535 ns, p99.99 207,231 ns and max
421,631 ns, measured from `bench`, an AMD Ryzen 7 2700X, on 2026-11-15 from 3
processes. Work item 358 holds the evidence that most of that round trip is the two
network cards' interrupt moderation and not Cavalry's path; it is parked.

No condition of the line names one of these numbers, and none compares them with
Aeron's. The owner ruled on 2026-11-15 and again on 2026-11-21 that features come
first and that the numbers are worked on when Cavalry must meet or beat Aeron's. The
reading behind that ruling:

- Of the 253 commits since 2026-10-28, 29 are about the bench, a baseline or a bench
  host by their subject, 53 name a re-record of a baseline in their message, and 14
  changed `bench/baseline.json`. In all, 217 name the bench, since every change
  under a data-plane crate carries its histograms.
- At this commit, a pull request under `crates/` takes both bench hosts at every push,
  about 16 minutes, one run at a time across every session, and its merge waits on
  three bench checks. On 2026-11-10 one commit took 81 minutes from its push to its last check
  for 21 minutes of work on the hosts (ADR 0071).
- The gate fails with no instruction changed. Six of eight processes at one head
  failed a row by 5.6% to 8.1% with the same instructions on the message path (ADR
  0125). A re-record takes two hours of both hosts (ADR 0110).

What holds invariant 2 in every pull request meanwhile needs no quiet host: the
allocation tests in `just test`, the bench's own refusal of an allocation in a
measured loop, which `gates (linux)` runs on a hosted runner, and the lints of `just
lint` for `async`, locks, the dependency allowlist and the padded slots.

## What a team that comes from Aeron would find

Read on 2026-11-21 against Aeron's open-source Transport, Archive and Cluster. "Feature
n" is a feature of the roadmap below. "Out" is the list in ADR 0138 of what is after
the line.

| What the team reaches for | State at 20741de3 | Lands |
| --- | --- | --- |
| Shared memory and UDP unicast on one API | Works | Done |
| One publication to several destinations | Works in manual control mode, 64 destinations; the dynamic control mode is refused | Done; the rest is out |
| Multicast | A multicast address is refused | Out |
| Claim and commit; an offer that fragments | Works; an offer carries up to the lesser of a term ÷ 8 and 16 MiB | Done |
| A whole message handed to the subscriber | Absent: the handler is handed fragments | Feature 7 |
| Channel parameters | Ten keys are read; any other is refused with error code 1; `cavalry:ipc` takes none, and its publication holds three terms of 64 MiB | Feature 7 |
| Flow control | `max` and `min`; `tagged` is refused | Out |
| Loss recovered by the receiver | Works | Done |
| A tool that prints every counter | Absent; `cavalry doctor` explains, and prints only what it finds | Feature 7 |
| An embedded driver | Works in Rust; `cavalry run` elsewhere | Done |
| Record and replay | Works: from a position or an instant, bounded, following a live recording, onto shared memory or UDP | Done |
| List and describe recordings | `cavalry doctor` lists the live ones; no call and no command lists the stopped ones, and the id of one is what `cavalry record` printed | Feature 7 |
| Truncate, purge and delete | Works on a stopped recording, by command | Done |
| A checksum beside recorded frames | None, and none is planned. Since 2026-11-22 a recording that this version's recorder was making at a crash ends at the position it had synced, so a power loss leaves in it no frame whose body was lost (ADR 0144). A recording that a driver of an earlier version was making at the loss keeps what its files hold. Nothing checks a frame below the synced position, so a disk that keeps bytes wrongly goes unseen | Done, by the synced position |
| A client that drives a remote driver's recordings; a replay that merges into a live stream | Absent | Out |
| Election and a commit by quorum | Works; the membership is a string of up to 8 members | Done |
| Sessions in six languages | Work: connect, redirect, follow the leader, keepalive, close. At most 10 sessions; a message of at most 1,344 bytes | Feature 6 |
| A service in six languages | Works: session open and close, message, new term, the cluster's time, replies. No timer | Feature 6 |
| A service's snapshot | Rust, and the C ABI since version 6 (ADR 0146); none of the five clients yet | Feature 6 |
| Catch-up, a restart on the log a member kept, a start from its own snapshot | Work | Done |
| A start from a peer's snapshot | Absent | Feature 5 |
| A standby | Works: marked in the members string, promoted by restarts | Done |
| An upgrade one member at a time | Works within one major version, gated on the loopback | Done |
| Commands that show a cluster or ask for a snapshot | `cavalry snapshot` asks the leader for a snapshot and waits until it lists its own (ADR 0147); no command shows a cluster | Feature 5 |
| A log purged below a snapshot | By hand, with the member stopped | Feature 5 |
| Dynamic membership, authentication, suspend and resume | Absent | Out |
| Encryption, a metrics export, a console, discovery, Kubernetes, kernel bypass, Windows | Absent | Out |
| Packages | None: every crate is `publish = false`, and a client is used from a checkout | Feature 10 |

## What the cluster does when something fails

| Fault | Today | The line asks | Feature |
| --- | --- | --- | --- |
| The leader's process ends, or its host is lost | Replaced in about 11 s: a timeout of 10 s, then an election of under 1 s. Tests over three drivers on the loopback | The bound stated, and held with processes ended | 4, 9 |
| The leader is stopped on purpose | The same 11 s | It hands the lead over at once | 4 |
| The leader's consensus module is silent while its driver runs | Replaced after 10.3 s to 52.2 s in 17 runs (work item 280) | Within the bound | 4 |
| The leader's service is gone, or its recorder cannot sync | Nothing detects a service that is gone (ADR 0118), and no test holds what a member does whose recorder cannot sync | It gives the lead up | 4 |
| A follower is cut off or started again | It rejoins about 10 s after the cut is mended; tests over drivers | As today | Done |
| A member takes the log slowly | It paces the whole cluster under `fc=min`; `doctor` has no finding | `doctor` names the member | 4 |
| A host loses power | Since 2026-11-22, held by work item 287, three pull requests on `master`: the driver cuts each recording a crash left live to what it had synced, and the member starts on its directory again (ADR 0143, ADR 0144). A test over three drivers has all three hosts lose power together. A member that lost power under a driver of an earlier version is refused on its directory, and is started on a new one, as the next row says | A check at the start | Done |
| A member's log directory is lost | Since 2026-11-22 the member is started on a new log directory, and it comes back with no vote on what it forgot (ADR 0145). It takes a leader only once it has heard every other member, so it follows no leader that a later term deposed. Then it withholds its vote and seeks no leadership until its log reaches its vote floor, the position its leader held when it came back. `cluster.vote_floor`, `cluster.votes_withheld` and `cluster.leaders_withheld` say so, and `cavalry doctor` names a member below its vote floor, with how far it has to go, and one that has taken no leader yet. While another member is stopped it follows no leader, and while it withholds, a cluster of three elects a leader only while both other members run. Two cases are left. Beside a member that started late and never caught up, while the member that holds the log is stopped, the two elect as a new cluster. And a new leadership term that the leader's sender held back by flow control can give the member a vote floor below an entry it had acknowledged, which an election while it catches up can lose (work item 411). Tests of three members in memory hold the rest | The member comes back with no vote on what it forgot | 3 |
| Members are started with different members strings | Three rules that no member checks (ADR 0134) | Members tell each other, and `doctor` names the one | 5 |
| A member is lost after the logs were purged | It cannot be replaced: no member starts from a peer's snapshot | Replaced on a new host while the cluster runs | 5 |
| A client has messages in flight at a leader change | Its session follows the next leader. A message in an entry that was cut is lost, and the client is not told which | Specified, and held in six languages | 6 |

No cluster test crosses hosts and none runs for days. Feature 9 and the release
candidate's soak are those two.

## What Brigade's server runs

Brigade's server is a Rust process that embeds the driver. Its ordered log, the
feed, is one recorded publication. It depends on Cavalry by a git dependency pinned
at 620a2e29, `master` of 2026-11-12, and two teams' servers run that build in
production, each on a log directory it keeps.

- It uses the embedded driver, an exclusive publication in manual control mode
  created at a position, a subscription to its own feed, a recording for each run, a
  sync before each answer, a replay of the earlier recordings, and a purge of what
  its snapshots cover. It uses no cluster call.
- It reads the catalog through `cavalry-log`, because the client lists no
  recordings. Feature 7 gives it that call.
- The durable log's format has moved from version 2 to 6 since its pin. Versions 3
  and 4 each added a file for cluster members and changed nothing a server's
  directory holds. Since 2026-11-22, version 5 keeps each recording's synced position
  in bytes of its time index that were reserved. No index the pin wrote has a synced
  position, and version 5 recovers such an index as version 4 did (ADR 0144).
  Version 6, of the same day, adds a cluster member's vote floor to its terms file,
  which a directory with no cluster member does not have, so it changes nothing a
  server's directory holds (ADR 0145). So such a directory opens under `master`. Since 2026-11-22,
  `conformance/upgrade/log-directory.sh` holds that in every pull request: it builds
  the tree at 620a2e29, has it write a directory as a server writes its own, and
  opens it in place under the tree under test
  ([ADR 0141](../decisions/0141-open-in-place-a-log-directory-written-by-the-tree-that-servers-run-and-build-that-tree-on-every-run.md)).
  Version 5 changes one thing a server's directory keeps: after `master`'s driver
  crashes, by its process or by its host, the driver's next start cuts the recording
  that the crash left live to the position it had synced. A power loss then leaves
  no frame whose body was lost. A crash of the process alone drops at most 1 s of
  the feed, which version 4 kept. The server syncs before each answer, so no record
  it answered is dropped.
- Its standby, work item 312, waits on Cavalry's cluster. Condition 9 of the line
  ends by telling the owner that the cluster is ready for the second node.

Feature 2 holds what the server runs, since 2026-11-22 (work item 362). The server's
use of Cavalry is a test of `just test`, copied from its code call for call. Its log
directory is held as the third point above says. A workflow builds the server at its
`master` against this `master` each night and runs its suite.
[What does Brigade's server rely on?](what-brigades-server-relies-on.md) lists the
calls and what the server relies on in each, and
[Read the nightly run of Brigade's server](../how-to/read-the-nightly-run-of-brigades-server.md)
says what a red night means and who acts. Two things that the server's code expects do
not hold. The first page names both, and work item 372 carries the first.

## The finish line

ADR 0138 gives the nine conditions in full, with the gate or the record that proves
each. Their state today:

| Condition | State at 20741de3 |
| --- | --- |
| 1. Installed, not cloned | Not started |
| 2. What an Aeron user reaches for first is there, or is named as absent | The table above: ten rows wait on features 5, 6 and 7; no page for the reader who comes from Aeron |
| 3. Nothing known lies about delivery | Issues 14, 16 and 17 are open; a member that lost its log and a member that never caught up elect as a new cluster while the one that holds the log is stopped (ADR 0145); a new leadership term that flow control held back can give a member that lost its log a vote floor below an entry it had acknowledged (work item 411); issues 245, 252 and 274 and work items 334 and 338 are tests that failed with no cause found yet |
| 4. The frozen surfaces are named and an upgrade across them has run | The upgrade gate runs a cluster across two trees; nothing is frozen, and a client and a driver of different builds have never met |
| 5. Honest and explainable to a stranger | The README's status is corrected with this page; no page for reporting a problem, no `SECURITY.md`, no generated latency page; errors name pages by a path in the repository |
| 6. No fault is left to an operator's rule where a check can hold it | Three faults are held by rules: work items 277 and 288, and the members string |
| 7. A leader that stops, in any way, is replaced within a stated bound | Two of six ways are held by tests; a planned stop costs 11 s; work item 280 is open |
| 8. Redundancy is restored while the cluster runs, and the cluster runs for days | Not started: no start from a peer's snapshot, no purge while running, no job across hosts, no cluster soak |
| 9. What Brigade's server runs is a gate here, and the release carries its data | The three gates are in since 2026-11-22 (feature 2): the feed's shape in `just test`, a log directory of 620a2e29 opened in place in `just conformance`, and the server's suite against `master` each night. Left for features 10 and 12: the server builds from the published crates, the candidate's soak, and the notice of work item 312 |

Row 9 is the state on 2026-11-22, when feature 2 landed. Rows 3 and 6 are the state on
2026-11-22, when work item 286's vote floor landed. Every other row is the state at
20741de3.

## The roadmap

Twelve features, in ADR 0138's order. Each is one work item on the Brigade Board,
whose number the table gives, and several pull requests. The size is a count of pull requests, read from the fifth
slice, whose seven features took 98 commits.

| # | Feature | Work item | Takes in | Size | Chain |
| --- | --- | --- | --- | --- | --- |
| 1 | Run the latency gate each night as a coarse check. **Done on 2026-11-21** | 361 | Parks work item 198 beside 358 | 2 | First |
| 2 | Hold what Brigade's server runs. **Done on 2026-11-22** | 362 | | 3 | Release |
| 3 | Hold invariant 1 through the faults that an operator's rule holds today | 363 | Work items 287, 286, 277, 288, 282, 281 | 13 | Cluster |
| 4 | Replace a leader however it stops | 364 | Work items 280, 343, 355 | 11 | Cluster |
| 5 | Restore a cluster's redundancy while it runs | 365 | | 13 | Cluster |
| 6 | Finish the service and the session in every language | 366 | Work items 335, 322, 325 | 18 | Client |
| 7 | Give an Aeron user what they reach for first | 367 | | 12 | Client |
| 8 | Clear the debts a beta user would meet | 368 | Issues 14, 16, 17, 238, 245, 252, 274; work items 279, 283, 285, 334, 338 | 10 | Any |
| 9 | Prove the cluster under faults, on one host and across three | 232 | The gate across three hosts of ADR 0052, changed | 6 | Cluster |
| 10 | Freeze the surfaces and package the release | 369 | | 13 | Release |
| 11 | Write for the stranger who comes from Aeron | 370 | | 8 | Release |
| 12 | Cut the candidate, soak it, and release 0.1.0 | 371 | | 5 | Last |

What has landed:

- **Feature 1, on 2026-11-21**
  ([ADR 0139](../decisions/0139-run-the-latency-gate-each-night-as-a-coarse-check.md)).
  No change takes a bench host. The gate runs in `.github/workflows/bench.yml` at
  07:17 UTC each day and on demand for a branch, and fails a row whose p99.99 is more
  than 50 percent above the slowest run recorded. Branch protection on `master`
  requires `gates (linux)` alone: the owner dropped the three bench names from the
  rule with this change. What was found: the coarse check does not see a regression
  under half again, and may not see one added system call on a row of 500 ns. The
  lints and the allocation tests hold those rules by reading the code, and the slice
  after the line measures what they cannot.
- **Feature 2, on 2026-11-22**
  ([ADR 0141](../decisions/0141-open-in-place-a-log-directory-written-by-the-tree-that-servers-run-and-build-that-tree-on-every-run.md)
  and
  [ADR 0142](../decisions/0142-build-brigades-server-against-master-each-night-through-a-cargo-patch.md)).
  What Brigade's server runs is held in three places. `just test` runs the server's
  use of Cavalry, copied from its code call for call, in both shapes of its feed.
  `just conformance` builds the tree at 620a2e29, has it write a log directory as a
  server writes its own, and opens that directory in place under the tree under
  test. `.github/workflows/brigade-server.yml` builds the server at Brigade's
  `master` against this `master` at 06:43 UTC each night and runs the server's own
  suite, reported and not required. What was found: two things that the server's
  code expects do not hold. A recording started beside an earlier client that the
  driver still has is of that client's publication, which work item 372 carries.
  The server's read of a recording that ends in padding can fail its start, which
  is the server's own to change.
  [What Brigade's server runs](#what-brigades-server-runs) has the rest.

How the order was chosen:

- **Feature 1 is first** because every later change under `crates/` is paced by the
  bench hosts until it lands. After it, a pull request waits on `gates (linux)`
  alone, and the three chains run side by side.
- **Feature 2 is second** because feature 3 changes the log's format, and two
  servers hold their teams' data in the format before it. A driver must open such a
  directory in place, with no step by an operator.
- **Feature 3 leads the cluster chain** because invariant 1 outranks the others: a
  defect there blocks a release, and nothing else does automatically.
- **Features 6 and 7 are a chain of their own** because each moves the C ABI's
  version and generates the five clients again, one change at a time. Feature 10's
  freeze waits for both.
- **Features 10 and 11 start early.** The package names are reserved first, and one
  client's package with the native library inside is built before the others. Five
  registries are the largest unknown of the slice.
- **Feature 12 is last** and holds the one fixed cost in wall-clock time, the soak.

## After the line

ADR 0138 recommends the first two items and decides none. The others keep the order
this page gave under ADR 0061. Each item is a slice with its own ADR, written when
the one before it ends.

1. **Meet or beat Aeron's numbers.** The latency gate is armed again, with what work
   items 198 and 358 found about the two hosts settled first. Cavalry and Aeron are
   measured on the same hardware, and the result is the public benchmark page.
2. **Wire encryption and authentication, on by default.** It is the largest promise
   the release does not keep, and it is what moves a beta user from a network it
   trusts to a real one.
3. **`cavalry-control`: the OpenTelemetry export, the console and discovery.** With
   the ADR that decides whether topics and request and reply belong in the API.
4. **Kubernetes: the operator and the Helm chart.** They package a release.
5. **The clauses that wait on a user.** Multicast, the dynamic control mode, tagged
   flow control, dynamic cluster membership, kernel bypass, Windows. Beta users are
   how those clauses fire.

## What needs a person's hand

Everything else on this page a session does on its own, and records.

| What | For | When |
| --- | --- | --- |
| The owner's leave to change the bench's workflows and rules | Feature 1 | Given on 2026-11-21, and used: feature 1 is on `master`, and the owner dropped the three bench names from branch protection the same day |
| A ruling on work item 340, whether the review loop's fixer may write `spec/` | Every feature that changes `spec/` | Ruled on 2026-11-21: it may, when the pull request's branch holds an Accepted ADR that names the page. Landed the same day ([ADR 0140](../decisions/0140-let-the-fixer-write-a-spec-page-that-an-accepted-adr-on-its-branch-names.md)) |
| An account, a name and a publishing credential on crates.io, PyPI, npm, Maven Central and NuGet, stored as secrets of the repository | Feature 10 | At its start: the names are reserved first, and Maven Central asks for a verified namespace |
| The setting that publishes the book, and a contact address for `SECURITY.md` | Feature 11 | At its start |
| A runner on `kip` with its own label, and the three hosts' addresses on the lab's network | Feature 9 | Before feature 9, which follows feature 5. Asked for early: a runner's registration has a lead time |
| A stop and a start of AppShapes' own Brigade server on the candidate's build | Feature 12 | When the candidate is cut |
| Whom to invite as beta users, and how | The finish line | Any time before 0.1.0 |

The repository is public today, so a person can find Cavalry before the line. The
line is when the project invites them.

## The estimate, and what it rests on

The estimate is a reading of the pace so far. It is not a gate, and no date is a
condition of ADR 0138.

- From 2026-11-09 to 2026-11-15, `master` took 147 commits in seven days, about 20 a
  day, while every change under `crates/` waited on the bench hosts. The page's
  first estimate, of 2026-11-09, gave the rest of the fifth slice 15 to 20 days. All
  of it but the gate across hosts took seven.
- The twelve features are about 115 pull requests. At half that pace they are
  twelve days of landings. The estimate allows two to four weeks, for what the count
  leaves out: a feature that grows, the review loop's cycles, and the hands in the
  table above.
- Then the seven days of soak.
- The sum is three to five weeks: 0.1.0 between about 2026-12-14 and 2026-12-28 if
  work starts on 2026-11-23, and into January if a second candidate must soak or a
  registry refuses a package's shape.

## What could move the line

- **The size of features 5 and 6.** A start from a peer's snapshot and a session
  message longer than one frame each change the cluster protocol. Timers add an
  entry to the log, and the first version that does so must say when a leader may
  use it (ADR 0136, Revisit).
- **The native library in five package formats.** Nothing in the repository has
  tried it.
- **A package name that is taken.** Feature 10 finds out first.
- **The review loop.** The reviewer and the fixer have stopped for a night before on
  a usage limit.
- **A finding in the soak.** That is what the week is for.
- **A beta user who needs encryption before they can start.** ADR 0138 keeps ADR
  0061's answer: a new ADR supersedes it and moves the line.

## How this page stays true

The work item that lands a feature of "The roadmap" marks the feature done on this
page in the same change, with the date and what was found, and gives the date of any
other state on the page that it changes. The sections above "The finish line" stay
the reading at the commit the first paragraph names, but for those dated states; they
are read again, and the date and the commit moved, when the candidate is cut. When
0.1.0 is published and the nine conditions hold, ADR 0138's Revisit fires, and this
page becomes the survey for the slice after it.

## Decisions behind this page

- [ADR 0138](../decisions/0138-draw-the-finish-line-at-a-beta-that-is-an-alternative-to-aeron-and-build-it-sixth.md),
  the finish line and the sixth slice.
- [ADR 0061](../decisions/0061-draw-the-finish-line-for-early-testers-at-the-first-release.md),
  the first finish line, superseded.
- [ADR 0052](../decisions/0052-build-raft-on-the-durable-log-fifth.md), the fifth
  slice.
- [ADR 0003](../decisions/0003-lay-out-the-workspace.md), why nothing is published
  yet.
- [ADR 0007](../decisions/0007-build-ipc-pub-sub-first.md),
  [ADR 0023](../decisions/0023-build-the-durable-log-second.md),
  [ADR 0039](../decisions/0039-build-udp-between-hosts-third.md) and
  [ADR 0046](../decisions/0046-build-the-c-abi-and-the-clients-fourth.md), the first
  four slices.
- [The brief](why-cavalry.md), the promises this page measures against.
