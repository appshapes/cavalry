# 0052. Build Raft on the durable log fifth, in eight features

- **Status:** Accepted
- **Date:** 2026-11-06
- **Invariants affected:** 1, 2, 3, 4, 5, 6
- **Supersedes:** none; ADR 0046's Revisit named the fifth slice as Raft, and this ADR is
  the one it asked for

## Context

The fourth slice is done. ADR 0046's Revisit clause fired on 2026-11-06: the fifth
client, .NET, passed `just conformance` on both runners at pull request 148's head
16c4bc0, its squash f7e1014 is on `master` with run 42218471543 green on all six jobs,
card 209 closed and was archived, and the README's status paragraph says the first
four slices work, with the C ABI and the five clients named. Cards 204 to 209 landed as
7bd4d0e to f7e1014. The clause names the fifth slice as Raft, and says the ADR "needs
its third host (card 159's note; ADR 0034) and its fan-out decision before it, one
publication per follower or multi-destination cast (ADR 0039's Revisit)". Raft lost
four times before, each time for a reason that has since run out or that this ADR
carries. ADR 0007 left it out because it sits on a stream and no stream existed; ADR
0023 because it sits on the durable log and on a stream; ADR 0039 because replication
between hosts needs a transport between hosts and "nothing of it can be gated before a
frame has crossed a wire"; ADR 0046 on four counts, the hosts, the fan-out, the scope
and what it blocks, and said "the ABI first costs Raft nothing, while Raft first leaves
the beachhead's languages without a client for another slice". The stream, the log,
the wire and the clients exist. The hosts and the fan-out are decided below; the scope
is the slice's own; and what Raft blocks is nothing, since every slice `CLAUDE.md`'s
first sentence names is built but this one. This ADR picks the order within it.

What the repository promises Raft. `CLAUDE.md`'s first sentence: "a durable log and
Raft replication"; its map gives `cavalry-raft` the "replicated state machine on top of
cavalry-log", on the data plane; invariant 1 says "Replaying the same log produces the
same state, byte for byte"; invariant 3 says "Mixed versions run during an upgrade" and
gate 3 says "`conformance/upgrade/` runs N and N+1 together", a gate that does not exist
and that the first feature needing it builds; gate 3 also names fault injection for
loss, reordering, CPU throttling and shm exhaustion. `crates/cavalry-raft/README.md`
promises a durable log replicated across a cluster of drivers with Raft, a
deterministic state machine fed from the committed log, "so any member can be rebuilt
by replaying its log and a failover loses nothing that was committed", a leader
elected in under a second, mixed versions in a rolling upgrade, both tested in CI with
fault injection, standby members in the open core, and the whole crate under the
data-plane rules "until a recorded decision splits the control parts out"; it must
never report an entry committed before a majority holds it durably, apply entries out
of log order, lock, await or allocate on the append and commit path, queue without a
bound, or change the log's format without an ADR; its example is "Not yet". The
glossary's rows for cluster, "a set of drivers running the same replicated state
machine over Raft, fed by a durable log", and standby point at `docs/reference/cluster.md`,
which does not exist. `docs/explanation/why-cavalry.md`: "Archive plus Cluster is the
real product", a recorded, ordered input log feeding a state machine; "Hot upgrades and
automatic failover", mixed-version clusters and sub-second election carried over; the
differentiators "the durable log plus Raft story, the operator experience and the open
core"; and, of Aeron, the open-source Cluster "has no warm standby story", Cluster
Standby and Insights are paid, and the upgrade story couples Transport, Archive and
Cluster. ADR 0003's Revisit: "when `cavalry-raft` needs a lock for membership (split
the crate, new ADR)". Nothing in `spec/` names a cluster.

What exists to build on, at f7e1014. The durable log (`spec/durable-log.md` version 1,
ADR 0024, ADR 0031): a fixed-slot catalog, Aeron's segment files and a time index,
recorded by the driver's recorder thread from any stream the client names (ADR 0025),
replayed into a subscription (ADR 0030) over `cavalry:ipc` or, through the sender, over
`cavalry:udp` (ADR 0043), truncated, purged and deleted (ADR 0031), its replay
deterministic by `just replay-check`, which plays one recording twice and once from an
instant and hashes the lines. The UDP transport (`spec/udp.md` version 0, ADR 0040,
ADR 0041, ADR 0042, ADR 0045): Aeron's setup, data, status message, NAK, RTT and
heartbeat frames field for field, "which is what lets multicast and multi-destination
cast (ADR 0039's Revisit) add no wire change"; a sender thread and a receiver thread,
the driver's fourth and fifth dedicated threads, each a bounded duty cycle; unicast
`cavalry:udp?endpoint=<host>:<port>[|interface=...]`, a multicast address refused, an
unknown parameter refused, so that nothing is silently ignored. The client protocol
(`spec/client-protocol.md` version 4, ADR 0019 and its amendments): Aeron's commands and
events in their id ranges, Cavalry's own from ranges of its own, the add and remove of
publications and subscriptions, the archive's commands, the channel status and the
source identity. The client API, the C ABI (`spec/c-abi.md` version 0, ADR 0051) and
the five generated clients over it, each with a conformance runner. Counters for every
transition and `cavalry doctor` over them (ADR 0012). Configuration by environment
variable with a `why_not_default` and a `doctor` check per key (ADR 0020), three keys
today. The `cavalry` crate's embeddable driver, examined by `doctor` while it runs.
The client's UDP tests run a lossy, reordering shim between two endpoints on one host
(`crates/cavalry-client/tests/udp_receive.rs`), the fault injection gate 3 has today.

What Aeron Cluster does, read on 2026-11-06 at `master`, version 1.55.0-SNAPSHOT. A
node runs a media driver, an Archive, a consensus module and a clustered service
container, in one process or several (Cluster Tutorial; Cluster Component Model). The
consensus module's agent runs the election and owns the log: on the leader a
`LogPublisher` writes "the single linearised log", recorded by the leader's Archive; on
followers a `LogAdapter` receives it and keeps a recording of it; the service container
reads the log through a spy subscription bounded by the commit position, so the
service is one behind the consensus and never stalls it. The log channel defaults to
`aeron:udp?term-length=64m` with no endpoint, which the context takes as
multi-destination cast (`isLogMdc`), in manual control mode (`MDC_CONTROL_MODE_MANUAL`
in `Election.java`): the leader adds each follower's log endpoint as a destination, and
the tutorial's driver uses `MinMulticastFlowControlSupplier`. A member is
`id,ingress,consensus,log,catchup,archive` endpoints, six to eight attributes, members
separated by `|`, with no default: `clusterMembers must be set`. The schema
(`aeron-cluster-codecs.xml`, id 111, version 17) names the session protocol between a
client and the cluster, `SessionConnectRequest`, `SessionEvent` with its `EventCode`
(`OK`, `ERROR`, `REDIRECT`, `AUTHENTICATION_REJECTED`, `CLOSED`), `SessionMessageHeader`
with the leadership term id, the session id and a timestamp, `SessionKeepAlive`,
`SessionCloseRequest`, `NewLeaderEvent`, `Challenge` and `ChallengeResponse`; the log's
own entries, `TimerEvent`, `SessionOpenEvent`, `SessionCloseEvent`,
`ClusterActionRequest` (`SUSPEND`, `RESUME`, `SNAPSHOT`), `NewLeadershipTermEvent` and
`MembershipChangeEvent`; the consensus messages between members, `CanvassPosition`,
`RequestVote`, `Vote`, `NewLeadershipTerm`, `AppendPosition`, `CommitPosition`,
`CatchupPosition` and `StopCatchup`, with `BackupQuery`, `BackupResponse` and
`StandbySnapshot` for the backup and standby components, and the dynamic membership
messages `AddPassiveMember`, `ClusterMembersChange`, `JoinCluster` and
`SnapshotRecordingQuery` deprecated since version 12; and the `SnapshotMarker`
(`BEGIN`, `SECTION`, `END`). The election (`Election.java`, `ClusterMember.java`) runs
`INIT`, `CANVASS`, `NOMINATE`, `CANDIDATE_BALLOT`, `FOLLOWER_BALLOT`,
`LEADER_LOG_REPLICATION`, `LEADER_REPLAY`, `LEADER_INIT`, `LEADER_READY`,
`FOLLOWER_LOG_REPLICATION`, `FOLLOWER_REPLAY`, `FOLLOWER_CATCHUP_INIT`,
`FOLLOWER_CATCHUP_AWAIT`, `FOLLOWER_CATCHUP`, `FOLLOWER_LOG_INIT`, `FOLLOWER_LOG_AWAIT`,
`FOLLOWER_READY` and `CLOSED`; a log is more recent by a higher leadership term, then a
higher position; a candidate nominates after a random delay of up to half the election
timeout; a vote goes to a candidate whose term is higher and whose log is no older; the
quorum is half the members plus one; the commit position is the quorum's rank among
the members' append positions; a follower behind the term's base replicates the
leader's recording from the leader's Archive, replays its own log, catches up through
the catch-up endpoint until near live, then joins the live log. Its defaults: leader
heartbeat timeout 10 s at an interval of 200 ms, election timeout 1 s, election status
interval 100 ms, startup canvass timeout 60 s, session timeout 10 s, log stream id 100,
consensus stream id 108, snapshot stream id 107 on `aeron:ipc?alias=snapshot`, ten
sessions, one service. `ClusteredService` is `onStart` with the snapshot image,
`onSessionOpen`, `onSessionClose`, `onSessionMessage` with the cluster's timestamp,
`onTimerEvent`, `onTakeSnapshot` to an exclusive publication, `onRoleChange`,
`onTerminate`, `onNewLeadershipTermEvent` and `doBackgroundWork`, Java only, and the
tutorial's determinism rules are the cluster's timestamp, no random numbers, no files,
no unordered iteration. `ClusterBackup` "polls for snapshots and replicates the log"
from a member's Archive at an interval of one hour, with a response timeout of 5 s and
a progress timeout of 10 s, in the open repository; Cluster Standby is the paid
component. The log channel's flow control is `max`, `min` or `tagged` by the `fc`
parameter, `max` the default, `min` the slowest receiver's pace, `tagged` the slowest
tagged receiver's, with a minimum group size below which the publication is not
connected (Flow and Congestion Control).

The hosts. `bench` and `grip` are the latency gate's two hosts, joined by a cable and a
chrony link for the UDP rows (ADR 0034, ADR 0044); every isolated core of both is
taken by the UDP rows, so "a fifth measured thread needs a host change" (ADR 0044's
Consequences). `kip` is the soak host, un-tuned by ADR 0034's undo, and carries card
159's week-long run, which started on 2026-10-31 at 16:13 UTC and ends on 2026-11-07;
card 159's brief says the soak host "joins the cluster slice later as its third member,
which is a card of its own when that ADR is written". The three hosts share the lab's
network; the cable joins two.

## Decision

The fifth slice is Raft on the durable log: a cluster of three drivers, each the
`cavalry` daemon in a member's role, replicating one stream, the cluster's log, from
its leader to its followers over multi-destination cast on `cavalry:udp`, each member
recording that stream into its durable log so that the three recordings are the same
bytes at the same positions, committing an entry when a quorum has recorded it,
feeding the committed log to a service that is a subscriber in any language through
the ABI, electing a leader in under a second with Aeron's election, catching a member
up from a peer's recording, restarting from a snapshot, following without a vote as a
standby, running two versions in a rolling upgrade, and gated across three hosts. The
two decisions ADR 0046 asked for first:

**The fan-out is multi-destination cast.** The leader's log is one publication whose
sender sends each datagram to every follower's log endpoint, in manual control mode as
Aeron Cluster's log channel is, each follower a unicast receiver for flow control and
NAKs, under the `min` strategy so that the slowest follower paces the leader and no
follower loses its image. One publication per follower lost (Alternatives): with one
claim and commit per entry on the leader's path, the per-entry work of the data plane
does not grow with the cluster; with one stream, one session id and one position, every
member's recording is the same bytes at the same positions, so the commit position is
a position in the stream, the replay check holds across members as it holds for one,
and a follower catches up from any peer's recording; and it is Aeron's shape, so
Aeron's names and its documentation explain Cavalry's. Multi-destination cast is a
transport feature with its own ADR and its own card, the slice's first, and it needs no
wire change: the setup frame's TTL and the status message's receiver id and group tag
are already on the wire for it (`spec/udp.md`, "Frames"; ADR 0040), and under `min` the
status message's receiver id is the field that tells a sender's destinations apart.

**The third host is `kip`.** When card 159's week ends and its close-out is on the
card, `kip` keeps its stock Ubuntu, gains a self-hosted runner with a label of its own,
and the three hosts run the cluster's cross-host job over the lab's network, not the
cable: the job is a correctness and availability gate, a leader lost and a member
partitioned with nothing committed lost, not a latency row, so it runs on housekeeping
cores and asks nothing of the isolated ones ADR 0044 gave the UDP rows. That is a card
of its own (feature 8), the only card of the slice that needs a hand on a host, and
the hand it needs, a runner's registration and the network between three hosts, goes
to Rjae as a `brigade decision` on that card and on no other.

Eight features make the slice, one Trello card each, in this order:

1. **Multi-destination cast on `cavalry:udp` (card 225).** Its ADR amends
   `spec/udp.md` to version 1 and `spec/client-protocol.md` to version 5, with
   Aeron's names: a publication channel `cavalry:udp?control=<host>:<port>|control-mode=manual`,
   whose control address is where its destinations' status messages and NAKs arrive;
   the commands `add destination` and `remove destination`, Aeron's `ADD_DESTINATION`
   and `REMOVE_DESTINATION`, each naming a publication and a destination channel of
   which only `endpoint` is read; the sender sending each datagram of a burst to every
   destination, with the per-destination state bounded as the endpoints are (`spec/udp.md`,
   64 distinct endpoints); flow control `fc=max|min` with `min` the slowest
   destination's pace and a destination that times out dropped from the accounting
   and counted; `tagged` and the group tag left to the Revisit; the dynamic control
   mode left out (Alternatives). No frame is new and no frame's field changes:
   `spec/udp.md` moves to version 1 for its channel grammar, the `control`,
   `control-mode` and `fc` keys that version 0 refuses as unknown, and for the
   sender's per-destination state, and `spec/client-protocol.md` to version 5 for the
   two commands. Every frame's version byte stays 0, since a receiver of version 0
   drops a frame whose version is not 0 (`spec/udp.md`, "The datagram"), and a byte
   that moved would have a member at N drop every datagram of a member at N+1, the
   opposite of feature 7's cluster of both. Vectors for the `add destination` and
   `remove destination` layouts in `spec/vectors/client-protocol.json`, and for the
   `control`, `control-mode` and `fc` parameters, parsed and refused, as `channel`
   vectors in `spec/vectors/udp.json`; the client API's `add_destination` and
   `remove_destination` on a publication and their ABI functions, a version bump of
   the ABI before the first release under the ADR's rule ("Nothing is released, so a
   client that later finds the ABI wanting sends it to a new version under a new
   ADR"); the UDP conformance suite and the C runner
   send one stream to two destinations on the loopback; a bench row, `one_way_32_udp_mdc`,
   over the cable to `grip` and the loopback at once, reported and not gated, since
   every isolated core of both bench hosts is taken (ADR 0044) and feature 8's job asks
   for none; it joins the gate when the Revisit clause for a cluster's row in the
   latency gate fires and a host change gives it cores. Builds nothing new among the
   gates; extends `conformance/` and the UDP vectors.
2. **The cluster protocol, specified (card 226).** `spec/cluster.md` version 0, with
   its ADR Accepted on the evidence of Aeron's schema read and dated, under Aeron's
   names where the concept matches: the log's entries, a session message header with
   the leadership term id, the session id and the cluster's timestamp before the
   user's message, the timer event, the session open and close events, the cluster
   action (suspend, resume, snapshot), the new leadership term event and the
   membership change event; the consensus messages between members, canvass position,
   request vote, vote, new leadership term, append position, commit position, catch-up
   position and stop catch-up; the session protocol between a client and the cluster,
   session connect, session event with its codes, keepalive, close and new leader, the
   challenge left to the Revisit; the snapshot marker; the members string
   `id,ingress,consensus,log,catchup,archive` with members separated by `|`; the
   consensus and log stream ids; the leadership term, the term base position and the
   position arithmetic that ties an entry to the stream; the timeouts, fixed at
   Aeron's defaults, heartbeat 10 s at 200 ms, election 1 s, canvass 60 s, session
   10 s, with no configuration key until `doctor` finds a host that needs one
   (invariant 5); and what a reader rejects. Vectors under `spec/vectors/`, a
   reference page, glossary rows, `cavalry-raft`'s README reading list. Nothing of the
   crate is written here.
3. **The replicated log (card 227).** `cavalry-raft`'s consensus module as the driver's
   sixth dedicated thread, a bounded duty cycle linked to the conductor by SPSC queues
   as the recorder, the sender and the receiver are, started by `cavalry run` in the
   member's role from the members string and the member id; the leader's log
   publication, a feature-1 channel with its followers as destinations; each follower's
   subscription on it, recorded into the member's durable log by the recorder, so that
   the three recordings are one stream's bytes; append positions from followers over
   the consensus channel, a unicast `cavalry:udp` publication per peer, and the commit
   position from the leader, the quorum's rank among the append positions, published
   as a counter and over the consensus channel; the committed log fed to the service,
   a subscription on the member's log stream bounded by the commit position, through
   the client API and the ABI, so that a service is a program in any of the five
   languages; a leader appointed by configuration for this card, with no election until
   feature 4. Builds the first `cavalry-raft` public item with the README's example,
   the crate's counters and a `[[state_machine]]` for the member's role, `doctor`
   explaining a stalled commit from the append positions, and extends
   `just replay-check` to a cluster on the loopback: three members' recordings of the
   same run hash the same, and a replay of any one gives the lines of the leader's.
4. **Election and membership (card 228).** Aeron's election state machine with its
   states as `spec/cluster.md` names them, as a `[[state_machine]]` with a counter per
   transition; a static membership of three from the members string; a leader elected
   in under a second after the heartbeat timeout; the new leadership term event in the
   log; a follower whose log is behind the term's base refusing to vote for a shorter
   log; `doctor` explaining an election from its counters. Builds gate 3's fault
   injection for the cluster: tests on the loopback that kill the leader, partition a
   member through the lossy shim and throttle one with the cgroup the driver already
   degrades under, each asserting that nothing committed is lost and that the cluster
   is available while a quorum remains, run by `just test` on both runners.
5. **The cluster client (card 229).** The session protocol in `cavalry-client`: a
   cluster session that connects over the ingress channel to the leader with an egress
   channel for the cluster's replies and events, keeps alive, is redirected to a new
   leader by the new leader event, and closes; its ABI functions, under the version
   feature 1 opened; the five clients regenerated from the header, each runner gaining
   a cluster round against a loopback cluster of three drivers; and
   `docs/tutorials/run-a-cluster.md`, the ten-minute path's shape for a cluster, since
   the README's six commands stay six (ADR 0046) and a cluster is three drivers.
6. **Catch-up, snapshots and standby (card 230).** A member that joins or restarts
   behind the leader replicates the missing span from a peer's recording through the
   replay the driver already serves over `cavalry:udp` (ADR 0043), replays its own log,
   and joins the live log near its head; a service writes its state to a snapshot, a
   recording of its own with the snapshot marker, on the cluster action the leader
   logs, and the recording log names the snapshot a restart loads before it replays
   the log after it, so retention (ADR 0031) can purge what a snapshot covers; standby
   members follow the log and take snapshots without a vote, in the open core, as the
   README promises and as Aeron sells. Builds the fault-injection tests for a restart
   from a snapshot and a standby promoted.
7. **A rolling upgrade (card 231).** The cluster protocol's version in every message,
   a member at N+1 joining a cluster at N and the election crossing the two,
   `doctor` naming the versions in a cluster. Builds `conformance/upgrade/`, the gate
   `CLAUDE.md` names and no feature has needed: it builds the checkout at the last
   commit that moved `spec/cluster.md`'s version and at `HEAD`, runs a cluster of
   both on the loopback through an upgrade one member at a time, and asserts nothing
   committed is lost, on both runners. The first release's ADR extends it to released
   versions (ADR 0003's Revisit).
8. **The cross-host gate (card 232).** `kip` as the third host: a self-hosted runner
   with a label of its own, the three hosts' addresses on the lab's network, and a job
   `cluster (hosts)` on every push to `master` and every pull request that runs the
   three-member cluster across `bench`, `grip` and `kip` through features 4 and 6's
   scenarios, a required check of `master` as `bench (grip)` is (ADR 0044's pattern),
   and the set-up page for it beside `docs/how-to/set-up-a-bench-host.md`. The card
   starts when card 159's close-out is on its card and feature 4 is on `master`, runs
   beside features 5 to 7, and carries the `brigade decision` for Rjae's hand on
   `kip` and the network; no other card waits on it. The slice's acceptance test is
   this job green with feature 7's upgrade in it, and the card that lands last changes
   the README's status paragraph.

Where invariant 2's rules stop. The leader's append path, a claim and a commit into
the log publication, is the data plane it already is; the follower's recording is the
recorder's duty cycle (ADR 0025); the sender's fan-out is the sender's duty cycle with
one more destination per datagram; and the consensus thread's duty cycle, reading
append positions and publishing the commit position, is per poll and not per message,
bounded, lock-free and allocation-free as the other dedicated threads' are. Election
and membership run on the same thread, by messages and timers, and need no lock, so
the crate stays whole under the data-plane rules as its README says, and ADR 0003's
Revisit for a split stays unfired; a feature that finds it needs a lock writes that
ADR. The service's path, a subscription bounded by the commit position, is the
subscriber's path of every slice before.

Configuration. The members string and the member id have no default a laptop could
guess, so they are the slice's two keys, each with its `why_not_default` and a `doctor`
check that resolves every member's endpoints and warns on a member that never sends an
append position; the timeouts, the stream ids and the snapshot cadence are constants
at Aeron's defaults until `doctor` shows a host that needs otherwise (`CLAUDE.md`,
Configuration and defaults). `cavalry run` with no arguments stays a driver with no
cluster.

Out of the slice: dynamic membership, a member added to or removed from a running
cluster, which Aeron deprecated in its schema and this ADR's Revisit names; the
challenge in the session protocol and any authentication; multicast, the dynamic
control mode and the `tagged` strategy (feature 1's Revisit); a backup component that
runs away from the cluster, since a standby is a member; `conformance/upgrade/` over
released versions, the registries, the ABI's freeze and the first release (ADR 0003's
Revisit); OTel export, discovery and the `console` command (`cavalry-control`);
encryption. Decisions left to the feature ADRs that must carry their evidence: the
control address and how a destination's status messages reach the right publication
(card 225); whether the consensus channel is one publication per peer or a feature-1
channel of its own (card 227); the leader's own append position and whether the leader
counts toward the quorum before its recorder has synced (card 227); where the cluster's
timestamp comes from and how a service reads it (card 226, card 229); the snapshot's
shape in the recording log and the cadence (card 230); and how `conformance/upgrade/`
names N (card 231).

The open cards, placed. Ten are open on the Cavalry list beside this ADR's own, and
none joins the slice.
Card 222, the claim struct the Python, Go and Node clients hand the ABI after a close,
is an invariant-1 defect in three clients and lands before card 225: invariant 1
outranks the order of work (`CLAUDE.md`, "A bug here blocks release"). Card 159, the
soak host's close-out, precedes card 232 and is its condition. Card 177, the bench's
purge mode, and cards 198 and 219, the bench's process effects, are the harness's.
Cards 197 and 223, two flakes of the client's tests and the Go runner, are the slice's
cost until fixed, as ADR 0046 said of cards 185, 187 and 197: a cluster's tests meet
the same windows. Cards 211, 212 and 215 are CI's. Issues 14, 16 and 17 are the client
protocol's and the driver's, and card 226 reads 16 before it writes the cluster's
events.

## Alternatives considered

- **One publication per follower.** Lost: the leader claims and commits each entry
  once per follower, so the per-entry work of its data plane grows with the cluster;
  each follower's stream has its own session id and positions, so the three
  recordings are not the same bytes at the same positions, the commit position is an
  entry index and not a stream position, the replay check across members needs a
  translation, and a follower catches up from the leader alone; and Aeron Cluster's
  log is one multi-destination cast publication, whose names this slice copies.
- **Multicast for the log.** Lost: the lab's switch and a cloud VM may route none, ADR
  0039 and ADR 0040 refused a multicast address for the same reason, and Aeron's own
  cluster log is multi-destination cast over unicast, each destination a receiver.
- **Raft with no transport change, over the unicast UDP of ADR 0040.** Lost: that is
  one publication per follower, above.
- **The consensus module as a process of its own beside the driver, as Aeron's is.**
  Lost: `CLAUDE.md` says one daemon per host, the driver embeds in the `cavalry` crate
  already, and a second process is a second thing a stranger runs in ten minutes; the
  member's role is a role of `cavalry run`.
- **A service API of Cavalry's own, Aeron's `ClusteredService` in Rust.** Lost: the
  fourth slice made every language a first-class client of the ABI, and a service that
  is a subscriber on the committed log reaches every language through the same header;
  a Rust-only service interface would make the cluster the one feature five clients
  cannot use. The cluster's timestamp and the session's reply path come through the
  session message header and the egress channel, which card 229 wraps.
- **The cluster gated on one host's loopback alone.** Lost, as ADR 0046 said: a
  cluster's promise is availability across hosts, and a loopback cluster leaves the
  third host's question open for a slice whose point is that question. The loopback
  cluster is each feature's gate on both runners; the hosts are the slice's acceptance.
- **Three gate hosts for latency rows across the cluster.** Lost: every isolated core
  of `bench` and `grip` is taken (ADR 0044), and a cluster's row is availability and
  commit latency across hosts, which feature 8's job measures and reports, not a row
  the latency gate holds to a baseline; a cluster latency row with its cores is a host
  change and a Revisit clause.
- **Election first, the log second.** Lost: an election elects a leader of a log, and
  Aeron's vote compares logs; a replicated log with an appointed leader is a cluster a
  stranger can run, record and replay after card 227, and the election joins it.
- **Snapshots left to a later slice.** Lost: retention (ADR 0031) purges segments, and a
  member that restarts after a purge cannot replay what is gone; a snapshot is what
  bounds the replay and what lets retention run on a cluster, so it is the slice's.
- **Dynamic membership in the slice.** Lost: Aeron deprecated its dynamic join
  messages in schema version 12, a static three is what its tutorial runs and what
  the hosts have, and membership change is a Revisit clause with its own evidence.
- **Standby as a paid tier, as Aeron sells it.** Lost: the README and `why-cavalry.md`
  promise standby in the open core, and a standby is a member that does not vote,
  which costs the election a flag.
- **Splitting `cavalry-raft` now into a data-plane crate and a control-plane crate.**
  Lost: the consensus thread needs no lock, since Aeron's agent is one thread driven by
  messages and timers, and ADR 0003's Revisit says the split comes when a lock is
  needed; a split with no lock to justify it is two crates for a stranger to read.
- **A configuration key per timeout.** Lost: `CLAUDE.md` says zero configuration is
  the primary configuration and a key needs a reason its default cannot be right;
  Aeron's defaults have run clusters for years, and `doctor` finds the host that needs
  another before a key does.
- **The first release in the slice.** Lost again: ADR 0003's Revisit and an ADR of its
  own, and this slice adds to the ABI before it freezes.
- **Deciding the slice in chat.** Lost: a decision that is not an ADR does not exist
  (ADR 0001), and this one orders eight cards and places eleven.

## Evidence

None measured: a scope decision. What informed it: ADR 0046's Revisit clause and the
state of `master` at f7e1014 on 2026-11-06, run 42218471543 green on six jobs, pull
request 148's gates green at 16c4bc0, card 209 archived, the README's status
paragraph; the promises in `CLAUDE.md`, `crates/cavalry-raft/README.md`, the glossary
and `why-cavalry.md`; the four slice ADRs' alternatives on Raft; the state of
`spec/durable-log.md`, `spec/udp.md`, `spec/client-protocol.md` and `spec/c-abi.md` at
f7e1014 and the three keys of `config.toml`; Aeron's sources named in Context, read on
2026-11-06 at `master`; the hosts' state in ADR 0034 and ADR 0044 and card 159's
brief; the ten open cards and five open issues with their briefs; and cards 224 to
232 on the Cavalry list with their briefs and "Done when" sections, read from this
text.

## Consequences

Easier: the product's first sentence is built when the slice ends, so `why-cavalry.md`'s
"Archive plus Cluster" is a thing a stranger runs from `docs/tutorials/run-a-cluster.md`;
one stream replicated byte for byte, so the replay check and the durable log's tools
serve a cluster unchanged; Aeron's names on the cluster's messages and states, so its
documentation explains Cavalry's; a service in any of the five languages, since the
committed log is a subscription; standby, snapshots and a rolling upgrade in the open
core, which Aeron sells or couples.

Harder: a sixth dedicated thread and two more channels per member, each a bounded duty
cycle with its counters; multi-destination cast adds per-destination state to the
sender and a control address to the receiver's path; the ABI moves to a new version
twice before the first release, for the destinations and for the cluster session,
and every client regenerates each time; the fault-injection tests run three drivers
on one runner and the gates grow by them; `conformance/upgrade/` builds the checkout
twice; the cross-host job occupies three self-hosted runners per push, so a push
waits on the slowest; `kip` changes role when card 159 ends, and the hand on it is
Rjae's; the slice is larger than any before it, eight cards where the fourth had six,
and the order above is what keeps a stranger reading one page.

Revisit: when card 232's job is green with feature 7's upgrade in it and the README
says the fifth slice works, with a new ADR for the sixth slice, the first release
(ADR 0003's Revisit: `publish` flipped, the five registries, the version the ABI
freezes at, `conformance/upgrade/` over released versions) or `cavalry-control`'s
export, discovery and `console`; when a user needs a member added to or removed from
a running cluster, for dynamic membership and the messages Aeron deprecated; when a
cluster's client needs the challenge, for authentication; when a cluster's row,
feature 1's `one_way_32_udp_mdc` among them, is wanted in the latency gate, for the
host change that gives it cores, on `bench` and `grip` or on a third host (ADR 0044);
when a cluster needs the `tagged` strategy, the group tag or the dynamic control mode;
when the consensus thread needs a lock, for the split ADR 0003 names; when `doctor`
shows a host that needs a timeout of its own, for the key and its `why_not_default`.

## References

- `CLAUDE.md`: invariants 1 to 6; Gates; Repository map; Data plane; Configuration and
  defaults; Working rules for agents; When in doubt
- ADR 0001, ADR 0003, ADR 0007, ADR 0012, ADR 0019, ADR 0020, ADR 0023, ADR 0024,
  ADR 0025, ADR 0030, ADR 0031, ADR 0034, ADR 0039, ADR 0040, ADR 0041, ADR 0042,
  ADR 0043, ADR 0044, ADR 0045, ADR 0046, ADR 0051
- `README.md`, the status paragraph; `crates/cavalry-raft/README.md`;
  `crates/cavalry/README.md`; `docs/glossary.md`; `docs/explanation/why-cavalry.md`;
  `docs/how-to/implement-a-feature.md`; `docs/how-to/set-up-a-bench-host.md`;
  `spec/durable-log.md`, `spec/udp.md`, `spec/client-protocol.md`, `spec/c-abi.md`;
  `scripts/replay-check.sh`; `crates/cavalry-client/tests/udp_receive.rs`; `config.toml`
- Aeron, read 2026-11-06 at `master` (1.55.0-SNAPSHOT):
  `aeron-cluster/src/main/resources/cluster/aeron-cluster-codecs.xml`
  (<https://github.com/aeron-io/aeron/blob/master/aeron-cluster/src/main/resources/cluster/aeron-cluster-codecs.xml>);
  `aeron-cluster/src/main/java/io/aeron/cluster/Election.java`, `ClusterMember.java`,
  `ConsensusModule.java`, `ClusterBackup.java` and `service/ClusteredService.java`;
  the Cluster Tutorial (<https://github.com/aeron-io/aeron/wiki/Cluster-Tutorial>),
  the Cluster Component Model (<https://github.com/aeron-io/aeron/wiki/Cluster-Component-Model>),
  Multiple Destinations (<https://github.com/aeron-io/aeron/wiki/Multiple-Destinations>)
  and Flow and Congestion Control (<https://github.com/aeron-io/aeron/wiki/Flow-and-Congestion-Control>)
- AppShapes Trello cards 159, 177, 197, 198, 211, 212, 215, 219, 222, 223, and 224 to
  232
