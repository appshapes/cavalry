# cavalry-raft

The replicated state machine on top of the durable log. Data plane: the append and
commit path runs per message.

## What this does

- Runs one cluster member's part of `spec/cluster.md`: `Consensus` is the state
  machine of a member, its role, its election, its terms and the positions it knows.
  The leader records its term, announces it, appends the term's first entry and commits
  what a quorum has recorded; a follower canvasses, joins the leader's log at its
  append position and reports that position as it rises.
- Leads by appointment, until the election is built: the member with the lowest id is
  the one member that may lead, and every other member follows it (`spec/cluster.md`,
  "The appointed leader"; ADR 0063). It canvasses first and leads only when no member
  reports a log ahead of its own, so a leader that lost its log directory never leads
  a second time. A member that could join only with a catch-up or a truncation, one
  that starts late, is left out and counted, never joined wrong.
- Elects, on a path of its own that is being built:
  `Consensus::with_election(member_ids, member_id, seed)` is a member that canvasses,
  nominates itself after a delay drawn from its seed, asks for the other members' votes
  and leads the term a quorum votes for, at once when every member it has heard has
  voted for it (ADR 0077). It stores a term before the vote or the request vote that
  raises it, and votes once in a term. A candidate asks for votes in the term it
  nominated itself for and no other: one whose term another candidate's request raised
  asks for none in the raised term and does not lead it. A leader tells a member whose
  log is of an older term which term its own record of terms holds next, with that
  term's base and its end, read through `Io::terms_after`: the member truncates what it
  holds above that base, so the leader names the next term and never a later one, over
  which the member would keep bytes of its own. A leader on this path gives up the lead:
  it holds an election when its active members fall below a quorum, and when a canvass
  position, a request vote or a commit position carries a term above its own, and it is
  a follower from that call on, its log publication given up in init. A follower holds
  an election on a request vote or a commit position of a higher term too (ADR 0082). A
  member told a next term whose base is below its append position truncates to that base
  first, the tail of an older term that no quorum recorded, and never below the commit
  position it knows: told to, it keeps its log and fails its election (ADR 0083). A
  leader answers a member for the highest term its own record holds at or below the one
  the member reported, and a member told a term below its own log term, with a next
  term above it, drops from its record the terms its leader's lacks, terms that fewer
  than a quorum recorded, cutting first what it holds above the base of the lowest, so
  that a member's record is its leader's before it replicates or joins (ADR 0096). A
  leader answers a member that asks for a term it missed, with a catch-up position below
  the leader's term base: it replays the recording of its own that holds the position
  asked, to the end of the term that holds it or to that recording's end, through
  `Io::start_replay`, one replay a member at a time and one for each catch-up position.
  A replay that has stopped is followed by nothing of the leader's own. It stops the
  replay of a member that canvasses or asks again, and every replay when it holds an
  election (ADR 0085, ADR 0092). A member below its leader's term base replicates one
  term it lacks, to that term's end, and canvasses again (follower log replication): it
  records a term it holds none of at its base before it asks, and asks for one replay
  at a time. When a replay's image has gone and its log ends further on, it subscribes
  again and asks from there, so that a recording is armed for each image; it asks from
  a position once (ADR 0092). It checks that each replay's image joins where it asked
  from, fails its election when its append position does not move for the leader
  heartbeat timeout, and at its leader's base records the leader's term and waits for
  the commit position (ADR 0086). A leader answers a member that is behind
  within the leader's own term too, with a catch-up position from its term base to its
  append position: the replay follows its live recording, with no end, and the leader
  stops it and sends stop catch-up when the member reports with the catch-up flag (ADR
  0087, ADR 0088). A member that has joined its leader's term behind the leader's log
  asks for it from its append position, records the leader's replay and adds the live
  log when it is near the leader's position. It sets the catch-up flag in its reports
  once it is on the live log, its append position at or past where its live image
  joined, and not before: the replay reads no further than the leader has synced, and
  one stopped sooner would leave a hole below the live image. It joins when the leader
  sends stop catch-up (ADR 0088). A leader on this path closes its election in leader
  ready when a quorum of members, itself among them, has joined its log at its log
  position, and waits for no other member: one that joins after the term's first entry
  catches up with it (`spec/cluster.md`, row 8). A member on this path starts on the
  log directory it kept: its first init reads the candidate term it stored and the
  highest term of its record, through `Io::candidate_term` and
  `Io::log_leadership_term_id`, so that it votes once in a term across a restart and
  canvasses with the log it holds. The term it followed, its leader and the commit
  position it knew are not kept: a leader tells it. Its canvass waits the startup
  canvass timeout, 60 s, for a member it has not heard, as at any start, and a new
  leadership term says in `is startup` whether its leader leads it from its first
  election since it started (ADR 0089). The consensus module does not use this path yet: a member
  over a driver is appointed until the election can do what the next section lists, and
  then the module moves to it in one change (ADR 0081).
- Counts every move of the election. `ElectionState` is the state machine
  `cluster_election` of `counters.toml`, and each of its transitions adds one to a
  count of `Counts`, the counter registered for it; `Counts::by_counter` pairs each
  count with its counter's id, for the module to store as a client counter (ADR 0067;
  [the counters reference](../../docs/reference/counters.md)).
- Does no I/O of its own in its core: `Consensus` reaches the consensus stream, the
  log, its recording and its `terms` file through one trait, `Io`.
- Runs a member over a driver: `ConsensusModule::start(dir, log_dir, members,
  member_id)` is a client of the driver in `dir`. It sends the consensus messages on a
  publication to each peer, adds the leader's log as a publication with every other
  member as a destination and records it, the publication created at the term's base log
  position so that the recording begins there and every frame of the term lies at its
  position in the cluster's log (ADR 0059), or subscribes to the leader's log and arms
  the recording that holds it from its first frame. Where that recording begins it
  reads from the driver's catalog in `log_dir`, which must be the driver's log
  directory: a directory with no catalog is refused at the member's start with
  `RaftError::Catalog`, before anything is written there, and a catalog whose slot
  does not hold the recording the driver started gives no start. It does not read
  it from the image: an image that arrived between its two commands
  is recorded from what it had received by then, so the module tells its election
  where the recording begins, and counts a recording as the member's log only once
  the election has been told its image and when it begins where the log ended (ADR
  0093). It keeps the record of terms in the
  `terms` file of `log_dir`, and stores what it counts in the driver's counters file:
  `cluster.role`, `cluster.election_state`, `cluster.leadership_term_id`,
  `cluster.append_position`, `cluster.commit_position`, `cluster.messages_rejected`,
  `cluster.log_recording_id`, the recording that holds its log, which its service
  replays, and the twenty-three counts of `Counts::by_counter`, of which the four of the
  ballot, the two of an election held on a higher term, the two of a truncation, the one
  of dropped terms, the two of a replication and the one of a catch-up stay 0 while the
  leader is appointed. The
  counters of sessions and of the service's version in `spec/cluster.md`, "Counters",
  come with those features. One thread calls `do_work()`; the leader's process appends
  with `offer(payload)`, up to 1,344 bytes an entry. A follower that leaves the log
  holds its recording until the driver has said where it stopped, and that stop is where
  its log ends. A stop that the driver refuses or does not answer is a `client` error,
  and a stop position not read within 1 s is a `stop-timeout` error. Either way the
  member stays in init with the recording held, and the next `do_work()` stops it again.
- Gives a leader's log up and serves replays over a driver, for a core that elects
  (ADR 0091). The module keeps the recordings of the log it has stopped, in position
  order: the last 1,024, each with where it begins and where it stopped. `remove_log`
  stops the recording of the term the member led, whose stop is where the member's
  log ends, and then closes that term's log publication. `start_replay` replays the
  recording that holds the position asked onto stream 100 at the asking member's
  `catchup` endpoint, as this member's own members string names it: to the end asked
  or to the recording's own, whichever comes first, and with no end asked it follows
  the live recording of the term the member leads. A replay still running to the
  member is stopped first. `stop_replay` stops it, and `replay_stopped` says once
  where it ended and whether it ended by itself. A position that no recording kept
  holds starts nothing. The module's own core is appointed and asks for none of this
  yet; the tests drive the module's I/O over a driver, with the other member played
  by the test.
- Records the replays a member asks for, and catches up, over a driver, for a core
  that elects (ADR 0094). `add_catchup` subscribes to stream 100 on the member's own
  `catchup` endpoint and arms a recording there, before the catch-up position leaves:
  one recording for one replay's image (ADR 0092). The catch-up position carries the
  member's id and that endpoint. `catchup_image` says where the member's recording of
  the image begins, from the catalog (ADR 0093), once at least, and nothing once that
  recording has stopped, which is when the image has gone. `remove_catchup` stops the
  recording, whose stop is where the member's log ends, and removes the subscription.
  The member's append position is the end of what it holds in order: the recordings
  it stopped, its recording of a replay, and its recording of the live log once the
  log has reached where that one begins, so that a member in follower catch-up that
  has added the live log follows either. A member that leaves the log in a catch-up
  keeps both recordings. The tests drive the `Io` of two modules, each on a driver
  of its own.
- Truncates a member's log over a driver, for a core that elects (ADR 0095):
  `truncate_log` purges each recording the member keeps that begins above the
  position, truncates each that stopped above it, which after a catch-up can be two
  that overlap, then takes every term based above the position out of the `terms`
  file. Each command waits for the driver's answer, so it is one step; asked again,
  it changes nothing.
- Asks for the sync a commit waits for: a member's append position is its
  recording's synced position, and the driver's recorder syncs a recording by itself
  once a second. So the module asks its driver to sync its recording of the log,
  `Recording::request_sync`, whenever the recorder has written past what it has
  synced, once for each position written and without waiting for the answer. A
  commit then waits for one sync on a quorum: ten entries, each appended once the
  last was committed, took from 186 ms to 228 ms in twenty runs on three members on
  a macOS laptop, and 10.0 s without the requests (`spec/cluster.md`, "The log"; ADR
  0060, ADR 0069).
- Tells a service where the log is: `cluster.log_recording_id` holds the id of the
  recording the member records the live log into, −1 while it records none, stored
  before the member's commit position exists and changed before a commit position
  the recording does not hold is stored. A service is a client of the same driver that replays that
  recording and reads it no further than `cluster.commit_position`:
  `cavalry-client`'s `Cavalry::committed_log` (`spec/cluster.md`, "The service"; ADR
  0066).
- Runs the same core in memory: `memory::Cluster` is one to eight members in one process
  on a clock the caller steps, with loss, delay, a member cut from every other, two
  members parted from each other (`cut_between`) or one that the other stops hearing
  (`cut_one_way`), a member that stops (`halt`) or whose module stands still while its
  driver receives (`stall`), paused syncs, a member started again on an empty log
  directory (`wipe`), one that elects started again on the log it kept (`restart`)
  and a member's recording ended where its log stands (`end_recording`) to set, and a
  leader's replay of its own log to a member that asked for it, which a member
  records only when it subscribed for it, as a driver arms one recording for one
  image (ADR 0092):
  what the example below and most of the crate's tests run. Every member holds a log of
  its own, the entries that reached it, so `log(member)` is that member's bytes and a
  test can find two members that hold different bytes at one position.
  `memory::Cluster::with_election(member_ids, seed)` is the same cluster with members
  that elect: which member leads depends on the seed, `leader()` says which, `offer`
  goes to it, and `log_to(member, position)` is a member's log up to a position, so that
  a test can hold what one leader committed against the log of the next. It is a test
  double and allocates; its consensus stream holds at most 4,096 messages in flight and
  loses, and counts, one sent while it is full, and a leader's log publication and each
  member's log hold every entry appended or received for the cluster's life.
- To come, each with its card: the C ABI's calls for a service, so that a service is
  a program in any of the five client languages (card 227), the election and mixed
  versions in a rolling upgrade (card 228), the cluster client (card 229), snapshots
  and catch-up (card 230), and standby members, which follow the log without voting,
  in the open core.

In Aeron, this is Cluster, with Cluster Standby as a paid component
([glossary](../../docs/glossary.md)).

### What the election does not do yet

A member made with `Consensus::with_election` has nominate, the two ballots, the
elections a leader holds, the truncation, the replication of a term a member missed
and the catch-up within a leader's term of `spec/cluster.md`. Where the page's
election does more, it does this instead, until the step that builds it (ADR 0081):

- Over a driver the module's `Io` can do all an electing core asks: give a leader's
  log up and replay it (ADR 0091), record the replays a member asks for and catch
  up (ADR 0094), and truncate (ADR 0095). The module's own member is still
  appointed and asks for none of it, until the change that moves it (ADR 0081).
  A truncation whose command faults after an earlier one succeeded leaves the
  member reporting the end and the log term it had until the leader asks again;
  ADR 0095's Consequences say where that is tested.
- Over a driver a member does not start on a log directory that holds a term or a
  stored candidate term: the module refuses, and says to start the cluster again
  as a new one. The core starts on such a directory (ADR 0089); the module does
  when it moves to this path, with the replay of the log it kept into its own
  state, the open sessions, which the core's leader replay and follower replay
  leave to it.

## What it must never do

- Report an entry as committed before a majority holds it durably, or apply entries in
  any order but log order (invariant 1).
- Lock, await or allocate on the append and commit path (invariant 2). Election and
  membership code follow the same rules while they live in this crate.
- Queue without a bound. A slow follower is handled by an explicit, documented policy,
  not by a growing buffer (invariant 3).
- Change the format of the log it replicates without an ADR; that is the on-disk format
  of `cavalry-log`.

## Example

Three members in one process, with no driver: the leader is appointed, the others join
its log, and an entry is committed once a quorum has recorded it.

```rust
use cavalry_raft::memory::Cluster;
use cavalry_raft::{ElectionState, Role};

let mut cluster = Cluster::new(&[0, 1, 2])?;
// One second of the cluster's clock: every member canvasses, the member with the
// lowest id leads, the others join its log and report, and every election closes.
cluster.run_ms(1_000);
assert_eq!(cluster.role(0), Some(Role::Leader));
assert_eq!(cluster.role(1), Some(Role::Follower));
assert_eq!(cluster.election_state(2), Some(ElectionState::Closed));

// The leader's own process appends an entry. It is committed when a quorum has
// recorded it, and every member learns the commit position from the leader.
let position = cluster.offer(b"hello")?;
cluster.run_ms(1_000);
assert_eq!(cluster.commit_position(0), Some(position));
assert_eq!(cluster.commit_position(2), Some(position));
// Every member's recording holds the same bytes.
assert_eq!(cluster.log(1), cluster.log(0));
assert_eq!(cluster.log(2), cluster.log(0));
# Ok::<(), Box<dyn std::error::Error>>(())
```

## What to read next

- [`spec/cluster.md`](../../spec/cluster.md), the cluster protocol this crate implements:
  the members string and the streams, the log's entries and their positions, the
  consensus messages, the session protocol, the election's states, the timeouts and
  what a reader rejects; its vectors are `spec/vectors/cluster.json`.
- [ADR 0058](../../docs/decisions/0058-specify-the-cluster-protocol-from-aerons-codecs.md),
  why the protocol is Aeron Cluster's field for field under a header of Cavalry's own,
  and [ADR 0052](../../docs/decisions/0052-build-raft-on-the-durable-log-fifth.md), the
  slice's eight features in order, of which this crate's are the third to the seventh.
- [ADR 0063](../../docs/decisions/0063-run-the-consensus-module-as-a-client-and-lead-by-appointment.md),
  how this crate's first feature is built: the consensus module is a client of its
  driver, in two layers, a core that does no I/O and the module that acts for it, and
  its leader is appointed, on the path `spec/cluster.md`, "The appointed leader",
  gives through the election's states.
- [ADR 0067](../../docs/decisions/0067-register-the-elections-state-machine-with-its-core-and-count-each-step.md),
  why the election's machine is registered with the core and what its two step
  counters count.
- [`spec/udp.md`](../../spec/udp.md), "The sender", for the multi-destination cast the
  log travels over, and [`spec/durable-log.md`](../../spec/durable-log.md) for the
  recording every member keeps of it.
- [`CLAUDE.md`](../../CLAUDE.md) for the invariants and the gates, and
  [ADR 0003](../../docs/decisions/0003-lay-out-the-workspace.md) for how the workspace
  is laid out.
