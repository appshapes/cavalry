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
  leader answers a member that asks for a term it missed, with a catch-up position below
  the leader's term base: it replays its own recordings from the position asked to the
  end of the term that holds it, through `Io::start_replay`, one replay a member at a
  time, the next recording's once the member has reported the end of the last. It stops
  the replay of a member that canvasses or asks again, and every replay when it holds an
  election (ADR 0085). A member below its leader's term base replicates one term it
  lacks, to that term's end, and canvasses again (follower log replication): it records
  a term it holds none of at its base before it asks, checks that each replay's image
  joins where its log ends, fails its election when its append position does not move
  for the leader heartbeat timeout, and at its leader's base records the leader's term
  and waits for the commit position (ADR 0086). The consensus module does not use this
  path yet: a member over a driver is appointed until the election can do what the next
  section lists, and then the module moves to it in one change (ADR 0081).
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
  the recording that holds it from its first frame, keeps the record of terms in the
  `terms` file of `log_dir`, and stores what it counts in the driver's counters file:
  `cluster.role`, `cluster.election_state`, `cluster.leadership_term_id`,
  `cluster.append_position`, `cluster.commit_position`, `cluster.messages_rejected`,
  `cluster.log_recording_id`, the recording that holds its log, which its service
  replays, and the twenty-one counts of `Counts::by_counter`, of which the four of the
  ballot, the two of an election held on a higher term, the two of a truncation and the
  two of a replication stay 0 while the leader is appointed. The counters of sessions,
  of the service's version and of the catch-up in `spec/cluster.md`, "Counters", come
  with those features. One thread calls `do_work()`; the leader's process appends with
  `offer(payload)`, up to 1,344 bytes an entry. A follower that leaves the log holds its
  recording until the driver has said where it stopped, and that stop is where its log
  ends. A stop that the driver refuses or does not answer is a `client` error, and a
  stop position not read within 1 s is a `stop-timeout` error. Either way the member
  stays in init with the recording held, and the next `do_work()` stops it again.
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
  driver receives (`stall`), paused syncs and a member started again on an empty log
  directory to set, and a leader's replay of its own log to a member that asked for it:
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
elections a leader holds, the truncation and the replication of a term a member missed
of `spec/cluster.md`. Where the page's election does more, it does this instead, until
the step that builds it (ADR 0081):

- A member catches up with nothing, and a leader serves no catch-up within its own
  term, a catch-up position at or above its term base. Where a member's append
  position is behind its leader's log position within the leader's term, it fails its
  election, counted in `cluster.elections_failed`, and canvasses again, every election
  status interval, 100 ms, until that step. So a leader that gave up the lead does not
  join the leader elected after it: it truncates an entry no quorum recorded, is then
  at the new term's base, and is behind the new leader's log by what was appended
  since. So too a member that replicated to its leader's base while the leader waited
  for it: the leader counts it, closes its election and appends the term's first
  entry, and the member is behind by that entry. Where the leader needs that member
  for its quorum, nothing above the term's base is committed until the member can
  catch up.
- Over a driver a leader cannot give its log up, a member cannot truncate, a leader
  cannot replay and a member cannot replicate: the module's `Io` refuses `remove_log`,
  `truncate_log`, `start_replay`, `stop_replay`, `replay_stopped`, `add_catchup`,
  `catchup_image` and `remove_catchup` as `unsupported`, and sends no catch-up
  position. The module's member is appointed and never asks (ADR 0082, ADR 0083, ADR
  0085, ADR 0086).
- A leader answers a member for the term the member reports, and does not check that
  its own record of terms holds that term. A member that recorded a term which fewer
  than a quorum recorded keeps it in its record under a leader whose record has none.
  When that member leads, it can ask another for a truncation below a commit position
  that member knows, which is refused at every election: the member does not join
  (card 267, with a test that makes the history).
- Leader ready waits for every member the leader has heard, as the appointed leader's
  does, since a follower that misses a term's first entry cannot catch up with it.
- A member starts with no term: its record of terms and its candidate term are not
  read back.

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
