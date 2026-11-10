# Where is Cavalry, and what is left before early testers?

This page answers one question: where is Cavalry now, measured from where it started,
and what work is left before a person outside AppShapes can be handed it. It was
written for AppShapes Trello card 234 on 2026-11-09, against `master` at commit
d5909f3. Every state on this page is the state at that commit. The line itself is
decided in
[ADR 0061](../decisions/0061-draw-the-finish-line-for-early-testers-at-the-first-release.md);
this page is the reading of the repository behind that decision, and the order of the
work that reaches the line.

Three terms. A *slice* is a part of the product that works from end to end, decided
by one ADR, with its own acceptance test. An *early tester* is an engineer outside
AppShapes who installs Cavalry on their own machines, runs their own workload on it,
and reports what they find, with nobody to ask. The *finish line* is the state in
which Cavalry is ready for early testers; it is also what this project means by MVP.

## Answer

- **Where it started.** On 2026-10-10 the repository held a brief, six invariants and
  nine empty crates. Nothing ran.
- **Where it is.** Thirty days and 218 commits later, four of the five slices that
  the product's first sentence names work and are gated in CI: publish and subscribe
  over shared memory, the durable log and replay, UDP between hosts, and the C ABI
  with clients in five languages. The fifth, Raft replication, has two of its eight
  features on `master` and the third in progress.
- **The finish line.** Version 0.1.0, published, with six conditions that a gate or
  a record proves (ADR 0061). In short: all five slices work, a tester installs from
  a registry and never clones, no known defect lies about delivery, the frozen
  surfaces are named and an upgrade across two published versions has run, a cluster
  has run for a week, and the README says what is not built.
- **What is left.** Three pieces of work: the rest of the fifth slice, six cards; the
  debts a tester would meet, six items; and a sixth slice, the first release, which
  ends with a week of soak on a release candidate.
- **When.** An estimate, not a gate: five to seven weeks from 2026-11-09, so 0.1.0
  between about 2026-12-14 and 2026-12-28. One of those weeks is the soak and cannot
  be made shorter.
- **What waits on a person.** Three things: the third lab host's runner and network,
  an account and a publishing credential on each package registry, and the choice of
  whom to invite.

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
| 3. UDP between hosts | ADR 0039, 2026-10-31 | 2026-11-04, at 61fbfdd | `cavalry:udp` unicast on the same API, a sender thread and a receiver thread, loss recovered by the receiver, replay over UDP | `bench (udp)` a required check; the ten-minute path over UDP |
| 4. The C ABI and the clients | ADR 0046, 2026-11-05 | 2026-11-06, at f7e1014 | `cavalry-ffi` and its header; Python, Node, Go, JVM and .NET clients generated from the header | `just conformance` passes for all five on both runners |
| 5. Raft on the durable log | ADR 0052, 2026-11-06 | Open: two of eight features | One publication sent to several destinations (card 225); the cluster protocol specified, with its messages read and written in `cavalry-proto` (card 226); a publication created at a position and a recording armed before its stream arrives (card 227, part) | The `cluster (hosts)` job green across three hosts, with a rolling upgrade in it |

Beside the slices, the project built the way it works: agents on branches with a
review loop (ADR 0004, ADR 0005), the documentation gate (ADR 0006), a latency gate on
dedicated hosts (ADR 0016, ADR 0034, ADR 0044), and a weekly email of the changes
(ADR 0026).

## What the gates hold today

- **Decisions.** 59 ADRs, 0001 to 0059, all Accepted but one that a later one
  superseded.
- **Specification.** Ten pages under `spec/`, each with a version, and 814 test
  vectors under `spec/vectors/` that every implementation must agree with.
- **Code.** 56,083 lines of Rust under `crates/*/src`, unit tests included, and 494
  test functions. `cavalry-raft` is two lines: its first public item is card 227's.
- **Counters.** 109 counters, and 7 state machines with 37 transitions, each
  transition with a counter, in `counters.toml`.
- **Configuration.** Three keys in `config.toml`. Zero configuration is the supported
  default.
- **CI.** Six jobs on every push and pull request. Five are required checks of
  `master`: `gates (linux)`, `gates (macos)`, `bench (linux)`, `bench (grip)` and
  `bench (udp)`.

The latency gate's baselines are in `bench/baseline.json`. The rows below are 32-byte
messages on `grip`, an Intel Core i7-10700 with isolated cores. A sample is the time
from the publisher's timestamp to the subscriber's handler. Four rows,
`one_way_32_exclusive`, `one_way_32_client`, `one_way_32_recorded` and
`one_way_32_udp`, were recorded on 2026-11-04 from 13 processes, and each of their
numbers is the largest the host measured over those processes. The `one_way_32_ffi`
row was added by hand on 2026-11-06 from 3 processes at the tree of c208279, in
commit b409b78: its p50, p99 and max are the largest over those 3 processes, and its
p99.99 is the slowest of their 63 runs, the number the gate compares a row with when
the row has no list of runs (ADR 0033). Either way a number is the slow edge of the
host's band and not a typical run.

| Row | Path | p50 | p99 | p99.99 | max |
| --- | --- | --- | --- | --- | --- |
| `one_way_32_exclusive` | Shared memory, in one process | 93 ns | 141 ns | 405 ns | 637 ns |
| `one_way_32_client` | Shared memory, through the client API | 106 ns | 185 ns | 493 ns | 1,453 ns |
| `one_way_32_ffi` | The same, through the C ABI | 123 ns | 224 ns | 537 ns | 1,144 ns |
| `one_way_32_recorded` | The same, while the driver records the stream | 105 ns | 217 ns | 521 ns | 1,535 ns |
| `one_way_32_udp` | UDP on the loopback, through the sender and the receiver | 2,479 ns | 2,699 ns | 5,651 ns | 7,359 ns |

Across the cable between the two bench hosts, at 1 Gb/s, the round trip
`udp_round_trip_32` reads p50 111,679 ns, p99 122,815 ns, p99.99 189,567 ns and max
262,911 ns, measured from `bench`, an AMD Ryzen 7 2700X, on 2026-11-04 from 3
processes.

## What was promised and is not built

The README's first paragraph and the brief's table promise more than the five slices.
This table places each promise against the finish line.

| Promise | State at d5909f3 | Lands |
| --- | --- | --- |
| Raft replication; highly available | Two of eight features | Fifth slice, before the line |
| A standby member in the open core | Not started | Card 230, before the line |
| Mixed versions run during an upgrade | The gate `conformance/upgrade/` does not exist | Card 231, then the sixth slice over published versions, before the line |
| `cargo install cavalry`; clients on five registries | Every crate is `publish = false`; a client needs a checkout | Sixth slice, the line itself |
| Wire encryption, on by default | None; `spec/udp.md` reserves the frame types | After the line; recommended first |
| OpenTelemetry export and a console | `cavalry doctor` only; the `console` command says it is not built | After the line |
| Discovery, topics, request and reply | None, and no ADR | After the line; an ADR decides whether they belong in the API |
| A Kubernetes operator and a Helm chart | None | After the line |
| Kernel bypass | None | After the line, when ADR 0039's Revisit fires |
| Multicast | A multicast address is refused; one publication to several destinations exists | After the line, when a user needs it |
| Retention policy tooling | The truncate, purge and delete commands exist; no policy drives them | After the line |
| A benchmark page against the reference competitor | Cavalry's own rows only | After the line |

## The finish line

ADR 0061 gives the six conditions in full, with the gate or the record that proves
each. Their state today:

| Condition | State at d5909f3 |
| --- | --- |
| 1. The first sentence works | Four of five slices |
| 2. Installed, not cloned | Not started |
| 3. Nothing known lies about delivery | Issues 14, 16 and 17 are open; cards 197, 223, 233 and 235, four known flakes, are open |
| 4. The frozen surfaces are named and an upgrade across them has run | Not started; card 231 builds the gate |
| 5. Bounded for days, as a cluster | One driver ran a week on the soak host, from 2026-10-31 to 2026-11-07; its close-out comment on card 159 is owed; no cluster has soaked |
| 6. Honest and explainable to a stranger | The README's status says "Not yet: Raft, and a release on crates.io" and does not name encryption; the cluster tutorial is card 229's; there is no page for reporting a problem, no `SECURITY.md` and no generated page of the latency numbers |

## The roadmap

### 1. Finish the fifth slice

Six cards, in ADR 0052's order. Each is one feature and several pull requests.

1. **Card 227, the replicated log.** On `master`: ADR 0059 and the driver's half
   (fc81ec1, 8264a0c). Open: pull request 159, the record of terms in the durable
   log. Left: the consensus module as the driver's sixth thread, the committed log
   fed to a service through the client API and the ABI, `cavalry run` in a member's
   role, and `just replay-check` over a cluster of three on the loopback.
2. **Card 228, election.** A leader elected in under a second, and the fault
   injection for a cluster: a lost leader, a partitioned member, a throttled member.
3. **Card 229, the cluster client.** The session protocol in the client, the ABI and
   the five clients, and the tutorial a stranger runs a cluster from.
4. **Card 230, catch-up, snapshots and standby.**
5. **Card 231, a rolling upgrade.** It builds `conformance/upgrade/`.
6. **Card 232, the gate across three hosts.** The slice's acceptance test. It starts
   when card 159's close-out is written and card 228 is on `master`, and it runs
   beside cards 229 to 231. It is the one card of the slice that needs a person's
   hand.

### 2. Clear the debts a tester would meet

None of these is on a slice's list. ADR 0061's third and fifth conditions put the
first five before the release.

1. **Issue 17.** A starting driver removes any directory whose command ring is
   missing, so a mistyped `CAVALRY_DIR` can remove a directory that is not Cavalry's.
   The fix changes `spec/client-protocol.md`, so it needs an ADR.
2. **Issues 14 and 16.** Two behaviours `spec/client-protocol.md` does not state.
   Unspecified behaviour is a bug in the spec, and the release freezes the page.
3. **Cards 223, 233 and 235.** One race in the conformance runners. Done on
   2026-11-10: the round polled once after ten offers, and a publication can accept
   an offer before the subscription holds its image, so the poll could deliver
   nothing. No message was lost. The five language runners now wait for the image
   first.
4. **Card 197, issue 112.** The test that sends a stream through a lossy, reordering
   path failed about one laptop run in five. It is the proof of the receive path's
   delivery, so the card had to say whether the test or the product was wrong. Done
   on 2026-11-10: the test was. Every failure was one assertion at the end, a read of
   the counter `image.removed` made the moment the client saw the image go; the
   conductor tells the client first and counts after it has unlinked the image's log,
   so on a loaded machine the read could come first. Every message arrived once and
   in order in every run. The test now waits for the counter.
5. **Card 159.** The soak week's close-out: the first and the last counters samples,
   and every `doctor` finding explained or carded.
6. **Card 211.** The client's allocation tests do not count a reallocation.
   Recommended before the release, since the release repeats the claim the tests
   make.

Five more open cards are debts too, but not a tester's, so they are not on the path
to the line. Cards 177, 198 and 219 are the bench harness's and the bench hosts'.
Cards 212 and 215 are the review loop's. They cost the project time, not a tester.

### 3. The sixth slice: the first release

Its ADR is written when the fifth slice ends, and orders its own features. What it
must hold comes from the Revisit clauses of ADR 0003, ADR 0046, ADR 0051, ADR 0052
and ADR 0057, and from ADR 0061's conditions:

- `publish` set to true, and the crate family published to crates.io in dependency
  order.
- The five clients published: PyPI, npm, Maven Central and NuGet each take a package
  with the native library inside it for the two platforms CI gates on; the Go module
  is published by a tag on the repository.
- A release workflow in CI that builds, checks and publishes from a tag.
- The version the C ABI freezes at, and the version of every `spec/` page that 0.1.0
  ships.
- The README's ten-minute path starting from an install, with `just quickstart`
  running it from the published artefacts.
- A release candidate, then 0.1.0, so that `conformance/upgrade/` runs a cluster from
  one published version to the next.
- The candidate's soak: a cluster of three for seven days on the soak host.
- The pages of condition 6: how to report a problem, `SECURITY.md`, the generated
  latency page, and a status paragraph that names what is not built.
- `CHANGELOG.md`'s Unreleased section, more than 1,600 lines today, becoming the
  entry for 0.1.0.

### 4. After the line

ADR 0061 recommends an order and does not decide it. Each item is a slice with its
own ADR, written when the one before it ends.

1. **Wire encryption, on by default.** It is the largest promise the release does
   not keep, and it is what moves a tester from a lab network to a real one. It
   changes the wire, so every version published before it is one more version it
   must negotiate with.
2. **`cavalry-control`: the OpenTelemetry export, the console and discovery.** With
   the ADR that decides whether topics and request and reply belong in the API.
3. **Kubernetes: the operator and the Helm chart.** They package a release.
4. **The public benchmark page and the launch.** The brief asks for one customer
   running Cavalry before a public launch. That customer comes from the testers.
5. **The clauses that wait on a user.** Kernel bypass, multicast, tagged flow
   control, dynamic cluster membership, authentication. Nine Revisit clauses in
   `docs/decisions/` begin "when a user". Testers are how those clauses fire.

## What needs a person's hand

Everything else on this page a session does on its own, and records.

| What | For | When |
| --- | --- | --- |
| A runner on `kip` with its own label, and the three hosts' addresses on the lab's network | Card 232 | When card 228 is on `master`. ADR 0052 sends the request on that card |
| An account and a publishing credential on crates.io, PyPI, npm, Maven Central and NuGet, stored as secrets of the repository | The sixth slice | Before its release workflow first runs. Maven Central also asks for a verified namespace |
| Whom to invite as testers, and how | The finish line | Any time before 0.1.0 |

The repository is public today, so a person can find Cavalry before the line. The
line is when the project invites them.

## The estimate, and what it rests on

The estimate is a reading of the pace so far. It is not a gate, and no date is a
condition of ADR 0061.

- From 2026-10-31 to 2026-11-09, `master` took 91 commits in ten days, two of them
  idle. Card 227, the first card with consensus in it, merged two pull requests in
  its first two days.
- The fifth slice has about 29 pull requests left, if card 227 needs six more, cards
  228 to 231 five each and card 232 three. At one and a half to two a day, that is
  15 to 20 days.
- The debts are about six pull requests. They fit between reviews.
- The sixth slice is about twelve pull requests, six to eight days, and then the
  seven days of soak.
- The sum is 28 to 35 days if nothing waits: at most five weeks, so 2026-12-14.
  Seven weeks, so 2026-12-28, if the review loop or a person's hand costs days, or if
  the first candidate's soak finds a defect and a second candidate soaks again.

## What could move the line

- **The size of cards 228 to 231.** No slice before the fifth had a feature as large
  as election or catch-up. ADR 0052 says the slice "is larger than any before it".
- **The native library in five package formats.** Nothing in the repository has
  tried it. It is the largest unknown of the sixth slice.
- **The hosts.** After card 232 every push runs member jobs on the two bench hosts
  and on `kip`, after the latency jobs. A push waits on the slowest.
- **The review loop.** The reviewer and the fixer have stopped for a night before on
  a usage limit.
- **A finding in the soak.** That is what the week is for.
- **A tester who needs encryption before they can start.** ADR 0061's Revisit names
  this case: a new ADR supersedes it and moves the line.

## How this page stays true

The card that closes an item of "The roadmap" marks the item done on this page in the
same commit, with the date and what was found, as the card that lands a slice's last
feature edits the README's status paragraph. The sections above "The roadmap" stay the
reading at the commit the first paragraph names; they are read again, and the date and
the commit moved, when a slice ends. When 0.1.0 is published and the six conditions
hold, ADR 0061's Revisit fires, and this page becomes the survey for the slice after
it.

## Decisions behind this page

- [ADR 0061](../decisions/0061-draw-the-finish-line-for-early-testers-at-the-first-release.md),
  the finish line.
- [ADR 0052](../decisions/0052-build-raft-on-the-durable-log-fifth.md), the fifth
  slice and what follows it.
- [ADR 0003](../decisions/0003-lay-out-the-workspace.md), why nothing is published
  yet.
- [ADR 0007](../decisions/0007-build-ipc-pub-sub-first.md),
  [ADR 0023](../decisions/0023-build-the-durable-log-second.md),
  [ADR 0039](../decisions/0039-build-udp-between-hosts-third.md) and
  [ADR 0046](../decisions/0046-build-the-c-abi-and-the-clients-fourth.md), the first
  four slices.
- [The brief](why-cavalry.md), the promises this page measures against.
