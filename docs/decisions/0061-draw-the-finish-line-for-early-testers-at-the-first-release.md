# 0061. Draw the finish line for early testers at the first release, after the fifth slice

- **Status:** Accepted
- **Date:** 2026-11-09
- **Invariants affected:** 1, 2, 3, 4, 5, 6
- **Supersedes:** none; ADR 0052's Revisit names two candidates for the sixth slice,
  and this ADR picks one. The order of the features inside that slice stays with the
  ADR that Revisit asks for

## Context

Card 234 asks for "a clear definition of finish line - when Cavalry is ready for
early testers", and on 2026-11-09 Rjae asked for a survey of where the project is from
where it started and a roadmap to that line. The survey is
[Where is Cavalry, and what is left before early testers?](../explanation/where-cavalry-is-and-what-is-left.md),
read from the repository at d5909f3. This ADR is the decision the survey needs: no
page says when a person outside AppShapes can be handed Cavalry, so every slice ends
at its own acceptance test and nothing says which slice is the last before a stranger.

What the repository already says about that line. ADR 0003 kept every crate
`publish = false` and gave the reason: "Publish `0.1.0` now. Lost: nothing works. A
crate on crates.io whose binary prints 'not built yet' lies to `cargo install`". The
brief, `docs/explanation/why-cavalry.md`, says "durable log and Raft replication are
core components from the first release, not a phase two", and "One lighthouse customer
before a public launch". ADR 0052's Revisit names what follows the fifth slice: "a new
ADR for the sixth slice, the first release (ADR 0003's Revisit: `publish` flipped, the
five registries, the version the ABI freezes at, `conformance/upgrade/` over released
versions) or `cavalry-control`'s export, discovery and `console`". ADR 0051 and ADR
0057 say the first release is what freezes the C ABI. Invariant 6 names the reader: a
competent engineer who has never seen Aeron, in another time zone, with nobody to ask,
who runs Cavalry in ten minutes from the repository alone.

What exists at d5909f3, in the survey's words. Four slices work and are gated:
publish and subscribe over shared memory (ADR 0007), the durable log and replay (ADR
0023), UDP between hosts (ADR 0039), and the C ABI with clients in Python, Node, Go,
the JVM and .NET (ADR 0046). The fifth, Raft on the durable log (ADR 0052), has two
of its eight features on `master` and the third in flight. Nothing is released: the
workspace version is 0.1.0, crates.io holds 0.0.0 placeholders (ADR 0002), and a
client is used from a checkout with a Rust toolchain. The README's first paragraph
promises "Encrypted, observable and highly available in the open core"; no encryption
exists, `cavalry-control` holds `doctor` and no export, and the `console` command
prints that it is not built. Fifty ADRs end with a Revisit paragraph, and nine of
their clauses begin "when a user": nobody outside the project has run Cavalry, so
none of the nine can fire.

## Decision

An early tester is the reader of invariant 6, outside AppShapes: an engineer who
installs Cavalry on their own machines, in their own language, runs their own workload
on it to decide whether it fits, and reports what they find, with nobody to ask.
Cavalry is ready for early testers when version 0.1.0 is published and the six
conditions below hold. That is the finish line, and it is the project's MVP. The
sixth slice is the first release, not `cavalry-control`; its ADR is written when ADR
0052's Revisit fires, orders its own features, and takes these conditions as its
acceptance test. Each condition names the gate or the record that proves it, because
a condition nothing checks is a hope (`CLAUDE.md`, Gates).

1. **The first sentence works.** Transport over shared memory and UDP, the durable
   log, and Raft replication: the five slices. Proved by ADR 0052's acceptance test,
   the `cluster (hosts)` job green as a required check of `master` with feature 7's
   rolling upgrade in it, and the README's status paragraph saying the fifth slice
   works.
2. **Installed, not cloned.** `cargo install cavalry` gives the binary and
   `cargo add cavalry` the Rust client; the five generated clients install from
   their languages' registries with the native library inside the package, on the two
   platforms `gates (linux)` and `gates (macos)` run on. The README's ten-minute path
   starts from the install. Proved by `just quickstart` running that path from the
   published artefacts, and by each client README's example run against its
   published package, on both runners.
3. **Nothing known lies about delivery.** No open card or issue is an invariant-1
   defect. Issues 14, 16 and 17 are closed: two holes in `spec/client-protocol.md`,
   which the freeze below would otherwise freeze, and a driver that removes a
   directory it did not make. The gates a tester can run read green on an unchanged
   tree: cards 197, 223, 233 and 235, the four known flakes, are closed. Proved by
   the issue and card lists, and by `just conformance`, `just replay-check` and
   `just test` on the release's tree.
4. **The frozen surfaces are named and an upgrade across them has run.** The release
   ADR names the version of every `spec/` page that 0.1.0 ships and freezes the C
   ABI's. Proved by `conformance/upgrade/` running a cluster from one published
   version to the next, which a release candidate before 0.1.0 provides.
5. **Bounded for days, as a cluster.** Card 159's week of one driver is closed, and
   the same soak runs seven days as a cluster of three members on the soak host, on
   the tree the release candidate is cut from, with every `doctor` finding explained
   or carded. A candidate that needs a change under a data-plane crate is replaced
   by the next candidate, and its week starts again. Proved by the soak card's
   close-out, with the first and the last counters samples in it.
6. **Honest and explainable to a stranger.** The README's status paragraph names
   every promise of its first paragraph and of the brief's table that 0.1.0 does not
   keep, and says that the transport is neither encrypted nor authenticated, so it
   belongs on a network the tester trusts. A tutorial for a cluster stands beside the
   one for publish and subscribe, both run in CI. A how-to page says how to report a
   problem with the `doctor` report and the counters file attached, since invariant 4
   promises that those two explain any loss or latency problem, and `SECURITY.md`
   says where a vulnerability goes. The release's latency numbers are a generated
   page: p50, p99, p99.99 and max per gated row, with the host named. Proved by
   `just docs`, `just lint-docs` and `just quickstart`.

Out of the finish line, each with its reason and each a slice or a Revisit clause
after it: wire encryption; the OpenTelemetry export, the console and discovery in
`cavalry-control`; topics and request and reply; the Kubernetes operator and Helm
chart; kernel bypass; multicast, the `tagged` strategy and the dynamic control mode;
dynamic cluster membership and the session challenge; a benchmark page against Aeron
on the same hardware; a platform beyond the two CI gates; a compatibility promise
longer than one version to the next. The survey gives the order this ADR recommends
for them and does not decide it: each is its own slice ADR when the one before it
ends, as the five so far were.

What needs a person's hand is listed in the survey and is three things: `kip`'s runner
and the lab's network for card 232, which ADR 0052 already sends to Rjae on that
card; an account and a publishing credential on each registry, for the sixth slice;
and the testers themselves, whom to invite, which no gate can answer.

## Alternatives considered

- **Early testers now, from a checkout, on the four slices that work.** Lost: the
  brief says the log and Raft are in the first release, and the beachhead it names
  compares Cavalry with replicated systems, so a tester handed a single-host log
  tests half the product. Nothing is frozen either: ADR 0052 says the ABI "moves to a
  new version twice before the first release" and every client regenerates each time,
  so a tester's program breaks twice with no notice a tool could give.
- **The line at the end of the fifth slice, from a tag, with no registries.** Lost:
  clients in five languages are the brief's fourth opening, and a Python or Node
  tester who must install a Rust toolchain and build a shared library to try it is
  being asked for the expertise the product says it removes. The install is the
  first minute of the ten, and it is the part no outsider has tried.
- **The line after wire encryption, since the README says "Encrypted".** Lost for
  the line, kept as the slice this ADR recommends first after it. A tester evaluates
  on a network of their own, where the missing part is said plainly (condition 6);
  `spec/udp.md` already reserves Aeron's encrypted frame types, and condition 4
  builds the gate that lets a later version turn encryption on beside peers of 0.1.0.
  The cost is real: until that slice lands the README's first paragraph promises
  more than a release keeps, which is why condition 6 makes the status paragraph say
  so.
- **`cavalry-control`'s export, discovery and console as the sixth slice, the
  release seventh.** ADR 0052's other candidate. Lost: counters and `doctor` already
  hold invariant 4 for one host and for a cluster, an export serves a tester who is
  already running Cavalry beside their own monitoring, and nine Revisit clauses wait
  on a user, so a slice chosen before there is one is chosen without the evidence a
  tester's first report would give.
- **The line after the Kubernetes operator.** Lost: the operator packages a release,
  so it follows one, and the two CI runners and the three lab hosts are what the
  gates can prove today.
- **A date as the line.** Lost: `CLAUDE.md` says the gates are the enforcement, and a
  date is not a condition a gate can check. The survey carries an estimate and names
  what it rests on.
- **The public launch as the line, with the benchmark page against Aeron and a
  lighthouse customer.** Lost: the brief orders them, "One lighthouse customer before
  a public launch", and the lighthouse customer comes from the testers.
- **Version 1.0.0 for the first release.** Lost: ADR 0003 set the family's version
  to 0.1.0, and a 0.x version says what is true, that a surface can still move with
  a version bump and a tested upgrade.
- **A soak of the cluster left out, card 159's week of one driver being enough.**
  Lost: what a tester does first with a replicated log is leave three members running,
  and the snapshot, the terms file and retention on a cluster have never run for
  longer than a test. The week is the line's one fixed cost in wall-clock time.
- **Leaving the line to the sixth slice's ADR.** Lost: the question was asked now,
  conditions 3 and 5 name work that starts before the fifth slice ends, and a
  decision that is not an ADR does not exist (ADR 0001).

## Evidence

None measured: a scope decision. What informed it: card 234 and Rjae's question of
2026-11-09; the state of `master` at d5909f3, run 42699795464 green; the survey page
and what it counts, 218 commits from 1100d76 on 2026-10-10, ADRs 0001 to 0059, the
five slice ADRs' acceptance tests and Revisit clauses, the ten pages of `spec/` and
their versions, the 18 open cards of the Cavalry list and the four open issues, read
on 2026-11-09; ADR 0003's Decision and Revisit, ADR 0051's and ADR 0057's Revisit and
ADR 0052's Decision, Consequences and Revisit; the promises of `README.md`,
`CLAUDE.md` and `docs/explanation/why-cavalry.md`; card 159's comments, the soak's
state on 2026-11-05 and its first finding; and the repository's visibility, public,
read from GitHub on 2026-11-09.

## Consequences

Easier: one page says what "ready" means, so a card is either on the path to it or
after it, and `/next-task` and a person agree on which; the sixth slice's ADR starts
from its acceptance test; the debts a tester would meet have a reason to be closed
before the release and not after a report; the Revisit clauses that wait on a user
start to fire once there is one.

Harder: the release carries a week that no amount of work shortens, and a finding in
it can cost another; five registries each have their own way to carry a native
library, which nothing here has tried; the README must say what is not built in the
paragraph that sells the product, until the slices after the line make it true;
encryption follows a published version, so it lands as a negotiated change across
versions and not as a first design; the line names issues and cards that are not on
any slice's list, and they are work the fifth slice's cards now share the queue with.

Revisit: when 0.1.0 is published and the six conditions hold, for the README, the
survey page and card 234's successor to say so, and for the ADR of the slice after
it; when a tester's first report shows a condition this list lacks; when a tester
who would otherwise start needs something on the out list first, encryption above
all, for an ADR that supersedes this one and moves the line; when the candidate's
soak has cost two weeks without a data-plane defect behind either, for what the
week is proving; when a registry cannot carry the native library as its packages
do, for that client's place in the release.

## References

- `CLAUDE.md`: invariants 1 to 6; Gates; Configuration and defaults; Documentation;
  Working rules for agents; When in doubt
- ADR 0001, ADR 0002, ADR 0003, ADR 0007, ADR 0023, ADR 0039, ADR 0046, ADR 0051,
  ADR 0052, ADR 0057
- [Where is Cavalry, and what is left before early testers?](../explanation/where-cavalry-is-and-what-is-left.md);
  `README.md`, the first and the status paragraphs; `docs/explanation/why-cavalry.md`;
  `docs/how-to/set-up-a-soak-host.md`; `spec/README.md`, the pages and their versions;
  `spec/udp.md`, the reserved frame types
- GitHub issues 14, 16, 17 and 112 of `appshapes/cavalry`
- AppShapes Trello cards 159, 197, 223, 227 to 232, 233, 234 and 235
