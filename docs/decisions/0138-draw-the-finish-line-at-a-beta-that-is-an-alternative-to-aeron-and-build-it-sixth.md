# 0138. Draw the finish line at a beta that is an alternative to Aeron, and build it sixth in twelve features

- **Status:** Accepted
- **Date:** 2026-11-21
- **Invariants affected:** 1, 2, 3, 4, 5, 6
- **Supersedes:** 0061, whose finish line this ADR redraws and whose conditions it
  keeps where they still hold; and three things of ADR 0052: the place of its feature
  8 in CI, a required check of every push; the fifth slice's acceptance test, that
  job green with feature 7's upgrade in it; and the first clause of its Revisit,
  which asked for the sixth slice's ADR once that job was green. This is that ADR,
  written before the clause fired

## Context

On 2026-11-21 the owner asked for a roadmap to a release with three milestones:
"Ready for beta use as an alternative to Aeron", "Cluster support of high availability
and automatic failover", and "Continued support of use by Brigade (in Brigade
server)". The owner also said what the roadmap must avoid: "what we spent a couple
weeks doing: bench run after bench run, without much progress on features. When we get
to the point of needing to meet or beat Aeron latency and throughput metrics, then we
can focus on that. I think at this point, the architecture is at least in the ballpark
and we will not need to make major changes to the platform." It repeats the owner's
ruling of 2026-11-15 on work item 358, which parked a search for what holds a bench
row's tail high: "we are chasing something we shouldn't be chasing **right now**",
and the priorities are "finishing features" and then a release.

ADR 0061 drew the finish line on 2026-11-09 at version 0.1.0 for early testers, with
six conditions. Twelve days later, at `master` 20741de3, seven of the fifth slice's
eight features are on `master` (ADR 0052; cards 225 to 231): the cluster elects,
serves sessions and services in six languages, catches a member up, takes snapshots,
runs a standby and upgrades one member at a time. Feature 8, work item 232, has not
started: it waits on a person's hand on a host. A survey of the repository on
2026-11-21, five readings, with each line that this ADR rests on checked against
the files, found what stands between that tree and the three milestones:

- **Against Aeron.** A subscriber is handed fragments and nothing reassembles them
  (`spec/c-abi.md`, "Out of this version"). No client call lists a driver's
  recordings, and no command lists the stopped ones: `cavalry doctor` lists the live
  ones, and the id of a stopped one "is the one `cavalry record` printed"
  (`docs/how-to/replay-a-recording.md`). Nothing prints every counter. A cluster
  holds 10 sessions, a session message is at most 1,344 bytes, a service has no timer,
  and a service takes its snapshot in Rust alone (`spec/cluster.md`, "Values" and
  "Out of this version"; work item 335). A `cavalry:ipc` channel takes no parameter,
  and its publication holds three terms of 64 MiB.
- **The cluster.** A lost leader is replaced in about 11 s, and so is one stopped on
  purpose (`docs/how-to/upgrade-a-cluster.md`). A leader whose consensus module is
  silent while its driver runs was replaced after 10.3 s to 52.2 s in 17 runs (work
  item 280). Five faults are held by an operator's care and by no check: a power
  loss (work item 287), a lost log directory (286), a truncation that faulted midway
  (277), a second consensus module started on one log directory (288), and members
  started with different members strings (ADR 0134).
  A member's log is purged by hand, with the member stopped. No member starts from a
  peer's snapshot, so once logs are purged a lost member cannot be replaced (ADR
  0130). No test kills a `cavalry run` process, none crosses hosts and none runs for
  days.
- **Brigade.** Brigade's server embeds the driver from a git dependency pinned at
  620a2e29, `master` of 2026-11-12, and two teams' servers run that build in
  production, each on a log directory it keeps. No test opens such a directory under
  a later tree, and no test here holds the shape the server uses. The server reads
  the catalog through `cavalry-log`, because the client lists no recordings. It
  waits on crates.io for a version to depend on.
- **The release.** Every crate is `publish = false`. The Python and JVM clients have
  no package manifest, and every client takes its native library from a checkout.
  The C ABI starts no driver, so a user of any client needs the `cavalry` binary,
  which only `cargo` gives. Every error and `doctor` finding names its page by a path
  in the repository, which an installed user cannot open. There is no tag and no
  release workflow.
- **The bench.** Of the 253 commits since 2026-10-28, 29 are about the bench, a
  baseline or a bench host by their subject, 53 name a re-record in their message,
  and 14 changed `bench/baseline.json`. In all, 217 name the bench, since every
  change under a data-plane crate carries its histograms. A pull request under
  `crates/` takes both bench hosts at every push, one run at a time across every
  session, and its merge waits on three bench checks. Six of eight processes at one
  head failed a row by 5.6% to 8.1% with the same instructions on the message path
  (ADR 0125), and a re-record takes two hours of both hosts (ADR 0110).

Three rules bound the answer. The invariants decide conflicts in their order, so a
defect of invariant 1 blocks a release and a latency number does not. An accepted ADR
is immutable, so the line moves by a new one. And "a weakened gate says why in its
commit message" (`CLAUDE.md`, Working rules for agents).

## Decision

The finish line is version 0.1.0, published as a beta: the first release that a team
can use in place of Aeron's open-source Transport, Archive and Cluster. The sixth
slice is that release. It has twelve features, in the order below, and its acceptance
test is the nine conditions below. From the day its first feature lands until the
slice that follows it, the latency gate paces no pull request: it runs each night as
a coarse check, and the per-message rules of invariant 2 stay enforced by the tests
and lints that need no quiet host. The fifth slice ends with its seven features,
gated on one host's loopback. The test that was to end it, the job across three
hosts with the upgrade of one member at a time in it, is this slice's feature 9.

A beta user is the reader of invariant 6 with a system to build: a team that would
otherwise run Aeron. It installs Cavalry on its own hosts, writes its service in one
of six languages, runs it for weeks on a network it trusts, in a place where a fault
costs time and not money, and reports what it finds, with nobody to ask.

### The nine conditions

Each names the gate or the record that proves it, as ADR 0061's did.

*It is an alternative to Aeron.*

1. **Installed, not cloned.** `cargo install cavalry` gives the binary and
   `cargo add cavalry` the Rust client. The generated clients install as each
   language's users expect: the Python, Node, JVM and .NET packages from PyPI, npm,
   Maven Central and NuGet with the native library inside, and the Go module by a tag
   of the repository. A user with no
   Rust toolchain takes the `cavalry` binary from the release's files. The platforms
   are the two the gates run on, Linux on x86-64 and macOS on Apple silicon. The book
   is published at a URL, and every error and `doctor` finding names its page there.
   Proved by `just quickstart` running the README's path and both tutorials from the
   published artefacts, and by each client README's example run against its published
   package, on both runners.
2. **What an Aeron user reaches for first is there, or is named as absent.** A
   subscriber is handed a whole message that was sent in fragments. A client lists and
   describes its driver's recordings. `cavalry counters` prints every counter. A
   cluster's service has timers, and takes and loads its snapshot, in each of the six
   languages. A cluster holds more than 10 sessions, and a session message may be
   longer than one frame. A `cavalry:ipc` channel names its term length. A page for
   the reader who comes from Aeron maps each of Aeron's concepts, tools and channel
   parameters to Cavalry's, or says that this version has none. Proved by `just
   conformance` holding each of these through every client, and by a generated
   reference page of channel parameters that the page links to.
3. **Nothing known lies about delivery.** No open work item or issue is an
   invariant-1 defect. Issues 14, 16 and 17 are closed. The gates a user can run read
   green on an unchanged tree: issues 245, 252 and 274 and work items 334 and 338 are
   each found or bounded. The defects known today that a user would meet first are
   closed: issue 238 and work items 279, 283 and 285. Proved by the two lists, and by
   `just conformance`, `just replay-check` and `just test` on the release's tree.
4. **The frozen surfaces are named, and an upgrade across them has run.** The release
   ADR names the version of every `spec/` page that 0.1.0 ships and freezes the C
   ABI's. It says which versions of a client and a driver work together, and what
   0.1.x promises its users. A client that meets a driver it cannot work with is
   refused with an error that says what to do. Proved by `conformance/upgrade/` running three things from
   the release candidate to 0.1.0: a cluster upgraded one member at a time, a log
   directory that the version before wrote, and a client against a driver of the
   other version.
5. **Honest and explainable to a stranger.** The README's status paragraph names
   every promise of its first paragraph and of the brief's table that 0.1.0 does not
   keep. It says that the transport is neither encrypted nor authenticated, and that
   0.1.0 makes no claim against Aeron's numbers. The release's own latency numbers are
   a generated page: p50, p99, p99.99 and max per row, with the host named. A how-to
   page says how to report a problem, and `SECURITY.md` says where a vulnerability
   goes. Proved by `just docs`, `just lint-docs` and `just quickstart`.

*Its cluster is highly available and fails over by itself.*

6. **No fault is left to an operator's rule where a check can hold it.** A power
   loss, a lost log directory, a truncation that faulted midway, a second consensus
   module on one log directory, and members started with different members strings:
   each
   has a test that makes the fault and holds that no committed entry is lost, and a
   `doctor` finding that names it. Where no check can hold a fault, the page says so
   and `doctor` names the sign. Proved by those tests in `just test`.
7. **A leader that stops, in any way, is replaced within a stated bound, and its
   sessions follow.** The ways: its process is ended, its host is cut off, its
   consensus module is silent while its driver runs, its service is gone, its recorder
   cannot sync, and it is stopped on purpose, which hands the lead over at once.
   `spec/cluster.md` states the bound for each. A session in each of the six
   languages follows the next leader with no call of its program's, and what a
   client is told about a message that was in flight is specified. `cavalry doctor`
   names a member that holds the leader's log back. Proved by a fault suite over
   `cavalry run` processes on the Linux runner, in `just test`, and by the cluster
   round of `just conformance`.
8. **Redundancy is restored while the cluster runs, and the cluster runs for days.**
   A lost member is replaced on a new host: it starts from a peer's snapshot and the
   log after it. A member purges its log below its snapshots while it runs. One
   command shows a cluster's members, their leader and their positions, and another
   asks the leader for a snapshot. Proved by tests over drivers; by the job `cluster (hosts)`, three members across three
   hosts, green at the candidate's tree before the soak starts; and by the
   candidate's soak, a run of days that shows what a test of seconds cannot: three
   members for seven days on the soak host, `kip`, with a fault made each day, every
   `doctor` finding explained or filed. The job across hosts does not run on `kip`
   during that week. A candidate that needs a change under a data-plane crate is
   replaced by the next, and its week starts again.

*Brigade's server keeps working.*

9. **What Brigade's server runs is a gate here, and the release carries its data.**
   The shape of the server's feed, its ordered log kept as one recorded publication,
   is a test in `just test`. Brigade's own server
   suite is built against `master` each night, and is green at the candidate. A log
   directory written at 620a2e29 opens under the release. Brigade's server builds
   from the published crates, and the server of AppShapes' own team runs the
   candidate through the soak week. When conditions 6 to 8 hold, the owner is told that Cavalry is ready
   for the server's second node (work item 312). Proved by those jobs and by the
   record on work item 312.

No condition names a latency number against Aeron. The release publishes its own
numbers and says so (condition 5).

### The twelve features, in order

One work item each on the Brigade Board. A feature is several pull requests, as cards
227 to 231 were. Where a feature takes in work items and issues that are open today,
it lists them, and the change that lands one moves it to Done.

1. **Run the latency gate each night as a coarse check (work item 361).** Pull requests and landings
   stop taking the bench hosts. A workflow of its own measures `master` each night
   and on demand, and fails only on a regression large enough to mean a design error.
   Its ADR sets that size; this ADR recommends a p99.99 half again above the slowest
   run recorded. Its ADR also changes the last sentence of invariant 2 in
   `CLAUDE.md`, "A measured regression fails CI", the latency row of the Gates table,
   the bench rule under Commands, and `.claude/rules/data-plane.md`; and it
   supersedes the sentences that it changes in the bench's ADRs, ADR 0016, ADR 0029,
   ADR 0033, ADR 0050 and ADR 0071 among them. The `/next-task` skill changes with
   it: it reads the last night's runs, where a failure two nights in a row on one
   row is a task ahead of the features, and it takes the chains below as the slice's
   order. What stays in every pull request: the
   allocation tests, the bench's own refusal of an allocation in a measured loop,
   which runs on the hosted runner, and the lints for `async`, locks, the dependency
   allowlist and the padded slots. What goes: "both histograms" in a commit message
   and a pull request body, the three bench checks as conditions of a merge, and
   re-records. A feature whose decision needs a number takes one measurement, by hand
   or by the workflow on its branch, and records it in its ADR. Work item 198 is
   parked beside 358. This feature comes first because every later one is paced by
   the hosts until it lands. It lands from a person's session on `master`: the
   reviewer's brief refuses a change that weakens a gate, and the fixer may write
   neither workflows nor rules.
2. **Hold what Brigade's server runs (work item 362).** A test over a driver of the feed's shape: an
   exclusive publication in manual control mode at a position, a subscription to its
   own feed, a recording for each run whose positions continue, a sync before each
   answer, a replay of the earlier recordings, a roll and a purge. The log
   directory's half of `conformance/upgrade/`: a directory written at 620a2e29 opens
   under the tree. And a nightly job that builds Brigade's server against `master`
   and runs its suite, reported and not required, since both repositories are
   public. It comes second because feature 3 changes the log's format.
3. **Hold invariant 1 through the faults that an operator's rule holds today (work item 363).** Work
   items 287, 286, 277, 288, 282 and 281. Work item 287 changes the log's format, so
   its ADR comes first: a driver opens a directory of the format before it in place,
   with no step by an operator, and feature 2's gate holds that.
4. **Replace a leader however it stops (work item 364).** Work item 280; a leader stopped on purpose
   hands the lead over; a member whose service is gone or whose recorder cannot sync
   gives the lead up; the bound for each in `spec/cluster.md`; `doctor`'s findings of
   work items 343 and 355. Its ADR decides whether the leader heartbeat timeout stays
   a constant of 10 s or becomes a key with a `doctor` check.
5. **Restore a cluster's redundancy while it runs (work item 365).** A member starts from a peer's
   snapshot. A lost member is replaced on a new host, with the how-to run by a test.
   Members tell each other the voters their strings name, so that a member started
   with another string is counted and named. A member purges its log below its snapshots
   while it runs. Two commands, one that shows a cluster's members, leader and
   positions, and one that asks the leader for a snapshot now.
6. **Finish the service and the session in every language (work item 366).** Work item 335, a
   service's snapshot through the C ABI and the five clients. Timers. More than 10
   sessions, with the bound and what a client is told at it. A session message and a
   reply longer than one frame. Work item 322, a session's refusal of a message that
   is too long, and work item 325, a Python handler's integer that does not fit 32
   bits. A cluster round in `just
   conformance` that ends the leader while each client has messages in flight; its
   ADR decides whether a new leader event tells a session how many of its messages
   the log holds.
7. **Give an Aeron user what they reach for first (work item 367).** Reassembly of a fragmented
   message for a subscriber. A list and a description of recordings from the client,
   the command line and the C ABI, so that Brigade's server stops reading the catalog
   itself. `cavalry counters`. A term length on `cavalry:ipc`, an `alias` on any
   channel, and a refusal of each Aeron key that names the page that says what to do
   instead.
8. **Clear the debts a beta user would meet (work item 368).** Issues 14, 16, 17, 238, 245, 252 and
   274, and work items 279, 283, 285, 334 and 338. Each is small; they fit between
   the others, and none waits on another feature.
9. **Prove the cluster under faults, on one host and across three (work item 232).** A suite over
   `cavalry run` processes on the Linux runner: a process killed, a process held, a
   service ended, a member stopped on purpose, a full disk, a member cut off, a clock
   stepped. And work item 232, changed: the job `cluster (hosts)` across `bench`,
   `grip` and `kip`, with the upgrade of one member at a time in it, runs each night
   and on demand, and is no check of a push. It still needs the owner's hand on
   `kip`.
10. **Freeze the surfaces and package the release (work item 369).** The release ADR: the version of
    every `spec/` page, the C ABI's freeze, the platforms, how a user with no Rust
    toolchain gets the driver, and what 0.1.x promises its users. A client's refusal
    of a driver it cannot work with. The crates' metadata. The Python, Node, JVM and
    .NET packages with the native library inside, and the Go module with what its
    build needs. The binaries. The workflow that builds, checks
    and publishes from a tag. The package names are reserved first, since one that
    is taken changes the plan.
11. **Write for the stranger who comes from Aeron (work item 370).** The book at a URL, and the
    errors' and `doctor`'s anchors as URLs. An install page. The page for the reader
    who comes from Aeron. Generated reference pages for the channel parameters and
    the command line. How to report a problem, and `SECURITY.md`. The generated
    latency page. How to run in a container. The README's status paragraph, and
    `cavalry-control`'s description, which names an export it does not have.
12. **Cut the candidate, soak it, and release 0.1.0 (work item 371).** The changelog's fold and the
    version's heading. The candidate's tag. `just quickstart` from the published
    artefacts. `conformance/upgrade/` over released versions. The soak of condition
    8, and Brigade's server on the candidate. Then 0.1.0, the README, and the notice
    to the owner that work item 312 asks for.

Features run side by side in three chains, since no bench host orders them once
feature 1 has landed: 3, 4, 5 and then 9; 6 and then 7; 2, 10 and then 11. Feature 8
fits anywhere, and feature 12 waits for all. Two orders cross the chains: feature
2's gate for the log directory lands before work item 287's change in feature 3, and
feature 10's freeze waits for features 6 and 7, which move the C ABI.
`spec/cluster.md`'s version and the C
ABI's each move in one pull request at a time: the session that is about to move one
says so first, as `CLAUDE.md` asks for the files that every change appends to.

### What is out

Each of these is a slice or a Revisit clause after the line.

- **Wire encryption and authentication, and the session challenge.** ADR 0061's
  reason holds: a beta user runs on a network it trusts, and condition 5 says so.
- **The OpenTelemetry export, the console and discovery in `cavalry-control`; topics
  and request and reply.** Counters, `doctor` and feature 7's `cavalry counters` hold
  invariant 4 until a user's own monitoring asks for more.
- **The Kubernetes operator and Helm chart.** They package a release.
- **Kernel bypass.** ADR 0039's Revisit waits on a user whose latency the kernel
  bounds.
- **Multicast, the dynamic control mode, a subscription with several destinations,
  and the `tagged` strategy.** No slice has needed one, and a cloud network routes no
  multicast (ADR 0039, ADR 0053).
- **Dynamic cluster membership, suspend and resume, and a cluster's termination by
  command.** A standby's mark and feature 5 replace a member with the string an
  operator already writes (ADR 0052's Revisit).
- **A client that drives a remote driver's recordings, and a replay that merges into
  a live stream.** A client drives its own host's driver, and a replay that follows a
  live recording serves the common case (ADR 0030).
- **A retention policy that purges by age or by size.** The commands exist, and
  feature 5 gives a cluster's member its own purge.
- **Windows.** The driver is built on Unix calls.
- **A compatibility promise longer than one version to the next.**

A benchmark page against Aeron on the same hardware is out as well, and so is every
re-record, every search for what a bench host does, and work items 198 and 358. This
ADR recommends that they be the slice after the line and does not decide it: the
slice in which Cavalry meets or beats Aeron's numbers, with the latency gate armed
again. The one after it is recommended as wire encryption, for ADR 0061's reason.

### What needs a person's hand

Given on 2026-11-21, before this ADR landed, and recorded on work item 360: the
owner's agreement to this plan, with five rulings. The latency gate runs each night
as feature 1 has it, and fails a row whose p99.99 is more than half again above the
slowest run recorded. The job across three hosts runs each night and must be green
at the candidate's tree, and is no check of a push. The beta carries the twelve
features whole. The slice recommended first after the line is the one that measures
Cavalry against Aeron, and wire encryption follows it. And work item 340: the
review loop's fixer may write a `spec/` page when the pull request's branch holds an
Accepted ADR that names that page. Feature 1 and work item 340 change workflows and
rules, so each lands from a person's session.

Still to come from a person: a runner on `kip` and the three hosts' addresses, for
feature 9; an account, a name and a publishing credential on crates.io, PyPI, npm,
Maven Central and NuGet, the setting that publishes the book, and a contact address
for `SECURITY.md`, for features 10 and 11; a stop and a start of AppShapes' own
Brigade server on the candidate, for feature 12; and whom to invite.

## Alternatives considered

- **Keep ADR 0061's line and release what works.** Lost: its line was drawn for a
  tester who finds out whether Cavalry fits. The owner's line is a team that uses it
  in Aeron's place, and that team's first hour ends at a message that arrives in
  fragments, at an eleventh session, or at a recording it cannot find. ADR 0061's
  own Revisit names this case: "when a tester who would otherwise start needs
  something on the out list first".
- **A beta that adds safety and packaging and no feature.** Lost for the same reason,
  and for the second milestone: a cluster whose lost member cannot be replaced, and
  whose log is purged with a member stopped, is not highly available for the weeks a
  beta user runs it.
- **Meet Aeron's numbers before the beta.** Lost: the owner ruled twice that the
  numbers wait. No comparison with Aeron has been measured, and this ADR claims
  none: the slice after the line measures both on the same hardware. What a beta
  user cannot work around is a missing feature, not a microsecond.
- **Switch the latency gate off until the performance slice.** Lost: a lock or a
  system call that reaches the message path would go unseen for weeks and be found
  among a hundred commits. One run a night finds it among a day's.
- **Keep the latency gate as it is.** Lost on the evidence in Context: it orders
  every change under `crates/` behind two hosts, it fails with no instruction
  changed, and it has taken more of the last three weeks than any feature.
- **The cross-host job as a required check of every push, as ADR 0052 decided.**
  Lost: it would put every push back in the hosts' queue, for a test whose result
  one run a night gives as well. The candidate must pass it; a push need not.
- **Dynamic membership, so that a member is replaced by the cluster's own log.**
  Lost for this line: a standby's mark (ADR 0134), a snapshot from a peer and a
  check of the strings between members replace a lost member with the string an
  operator already writes. ADR 0052's Revisit keeps dynamic membership for the user
  who needs it.
- **Wire encryption before the beta.** Lost again, for ADR 0061's reasons, and for
  one more: Aeron's open source has none, so its absence does not stop a team that
  would otherwise run that.
- **Leave Brigade's server on its pin until 0.1.0.** Lost: two servers hold their
  teams' data in a format that feature 3 changes, and a break found at the release
  is found too late to choose another format.
- **The beta as 1.0.0.** Lost, as in ADR 0061: 0.1.0 says what is true, that a
  surface can still move with a version and a tested upgrade.
- **A date as the line.** Lost, as in ADR 0061. The survey page carries an estimate.

## Evidence

None measured: a scope decision. What informed it: the owner's words of 2026-11-21
in the session that wrote this ADR, and of 2026-11-15 on work item 358; `master` at
20741de3, run 43785292781 green in every job; the five readings of 2026-11-21 that
Context sums up, of the repository against Aeron's features, of the cluster's faults
and tests, of Brigade's server at its `master` b4b97790, of the release's packaging,
and of the bench's rules and cost; the 21 open work items tagged `cavalry` and the
seven open issues, read that day; work items 89, 312 and 313 for what Brigade's
server needs; and the counts in Context, from `git log --since=2026-10-28`. The
survey page has the state in full.

## Consequences

Easier: a pull request under `crates/` waits on one check, `gates (linux)`, so
sessions work side by side; a work item is either in a feature or after the line,
so a person and, once feature 1 has changed it, `/next-task` agree on which;
Brigade's server has a gate here
before it has a release to depend on; the cluster's promise is held by faults made
on purpose and not by an operator's care.

Harder: the slice is the largest yet, twelve features where the fifth had eight, and
more than a hundred pull requests; a regression of a tenth in a tail goes unseen until the
performance slice, which is the price the owner chose; the C ABI and the cluster
protocol each move several more times before the freeze, and every client is
generated again each time; five registries each carry a native library in their own
way, which nothing here has tried; the nightly jobs fail away from any pull request,
so somebody must read them, and feature 1 gives `/next-task` that reading; the README
promises encryption until the slice that builds it.

Revisit: when 0.1.0 is published and the nine conditions hold, for the README, the
survey page and the ADR of the slice after it; when the nightly check fails two
nights in a row on one row, for a work item ahead of the features; when a beta
user's first report shows a condition this list lacks; when a feature of the twelve
turns out not to be needed by any condition, for its removal by the ADR that finds
so; when the candidate's soak has cost two weeks without a data-plane defect behind
either, for what the week is proving; when a registry cannot carry the native
library as its packages do, for that client's place in the release.

## References

- Work item 360 on the Brigade Board, which carried this decision; work items 232,
  277, 279 to 283, 285 to 288, 322, 325, 334, 335, 338, 340, 343 and 355, which it
  places; 198 and 358, which it parks; 89, 312 and 313, Brigade's server
- GitHub issues 14, 16, 17, 238, 245, 252 and 274 of `appshapes/cavalry`
- `CLAUDE.md`: invariants 1 to 6; Gates; Working rules for agents
- ADR 0003, ADR 0016, ADR 0029, ADR 0033, ADR 0050, ADR 0052, ADR 0061, ADR 0071,
  ADR 0104, ADR 0108, ADR 0110, ADR 0125, ADR 0130, ADR 0134, ADR 0136
- [Where is Cavalry, and what is left before the beta?](../explanation/where-cavalry-is-and-what-is-left.md);
  `README.md`; `docs/explanation/why-cavalry.md`; `spec/cluster.md`, "Values" and
  "Out of this version"; `spec/c-abi.md`, "Out of this version";
  `docs/how-to/run-the-bench.md`; `docs/how-to/upgrade-a-cluster.md`;
  `docs/how-to/promote-a-standby.md`
- Brigade's server, `server/crates/brigade-log` of `appshapes/brigade`, and its pin
  on Cavalry at 620a2e29
