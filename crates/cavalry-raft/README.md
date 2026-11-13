# cavalry-raft

The replicated state machine on top of the durable log. Data plane: the append and
commit path runs per message.

## What this does

- Runs one cluster member's part of `spec/cluster.md`: `Consensus` is the state
  machine of a member, its role, its election, its terms and the positions it knows.
  The leader records its term, announces it, appends the term's first entry and commits
  what a quorum has recorded; a follower canvasses, joins the leader's log at its
  append position and reports that position as it rises.
- Elects its leader, in memory and over a driver (ADR 0104):
  `Consensus::with_election(member_ids, member_id, seed)` is a member that canvasses,
  nominates itself after a delay drawn from its seed, asks for the other members' votes
  and leads the term a quorum votes for, at once when every member it has heard has
  voted for it (ADR 0077). It stores a term before the vote or the request vote that
  raises it, and votes once in a term. A candidate asks for votes in the term it
  nominated itself for and no other: one whose term another candidate's request raised
  asks for none in the raised term and does not lead it. A candidate that cannot win
  its ballot, one that has read a vote against its term or holds no vote of its own
  for it, canvasses again on its next cycle and does not wait out the election
  timeout, so a split vote costs a second delay of at most 500 ms and not a second.
  It reads its ballot before it sends, so on that cycle it asks no member for a vote
  (ADR 0105). A member that replicated a term it lacked canvasses again with no
  member's report, so that it waits for its leader's answer and does not nominate
  itself against a leader it has caught up with (ADR 0106). A leader tells a member whose
  log is of an older term which term its own record of terms holds next, with that
  term's base and its end, read through `Io::terms_after`: the member truncates what it
  holds above that base, so the leader names the next term and never a later one, over
  which the member would keep bytes of its own. A leader gives up the lead:
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
  sends stop catch-up (ADR 0088). A leader closes its election in leader
  ready when a quorum of members, itself among them, has joined its log at its log
  position, and waits for no other member: one that joins after the term's first entry
  catches up with it (`spec/cluster.md`, row 8). The core starts on the
  log directory a member kept: its first init reads the candidate term it stored and the
  highest term of its record, through `Io::candidate_term` and
  `Io::log_leadership_term_id`, so that it votes once in a term across a restart and
  canvasses with the log it holds. The term it followed, its leader and the commit
  position it knew are not kept: a leader tells it. Its canvass waits the startup
  canvass timeout, 60 s, for a member it has not heard, as at any start, and a new
  leadership term says in `is startup` whether its leader leads it from its first
  election since it started (ADR 0089). The consensus module builds its core so, with a
  seed from the operating system mixed with the member id (ADR 0104).
- Starts a member on the log directory it kept (ADR 0108). The module lists the
  recordings that hold the member's log in the file `log-recordings` in its log
  directory, each write synced, and counts a recording toward the append position
  only once a list names it. At a start it reads the list back against the driver's
  catalog: the log is the recordings the list names, each to where it stopped, and
  it ends at the furthest of those ends. A
  truncation that the member's stop cut short is finished: the log ends where the
  recordings that are left end, and a term based above that leaves the record of
  terms. Nothing else shortens the log. A directory of another member's, a record
  of terms that holds a term beside no list, a log that ends above 0 beside a
  record of terms that holds no term, and a catalog that holds anything
  else in place of a listed recording refuse the start, with what to do: a member
  that started with a shorter log than it reported could vote for a candidate that
  lacks a committed entry. So does a log the member forgot a recording of, one
  whose list begins above position 0 or names more than 1,024 recordings that hold
  bytes (`RaftError::LogForgotten`): a start replays the log from position 0, and
  this version takes no snapshot. The module counts what its list did in four
  counters: `cluster.log_recordings_listed`, `cluster.log_recordings_unsynced`,
  `cluster.log_recordings_forgotten` and `cluster.truncations_finished`. It does
  not tell a member whose host lost power from one whose process was ended, and
  the first can hold damaged entries at the end of its log: such a member is not
  started again, by the operator's hand, until card 275 (ADR 0108, "What a start
  does not hold"). `tests/restart.rs` stops a follower, the leader and all
  three members over three drivers and starts them again.
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
  `cluster.leader_member_id`, the member it leads as or follows and −1 while it has
  none, `cluster.election_cause`, why its last election began (ADR 0102),
  `cluster.append_position`, `cluster.commit_position`, `cluster.messages_rejected`,
  `cluster.log_recording_id`, the recording that holds its log, which its service
  replays, the four counts of its list of its log's recordings (ADR 0108), and the
  twenty-two counts of `Counts::by_counter`. The
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
  holds starts nothing. The crate's unit tests drive the module's I/O over a driver,
  with the other member played by the test.
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
  it changes nothing. `drop_terms_above` takes the terms a leader's record lacks out
  of that file by term, after such a truncation, each write synced
  (`TermsFile::drop_above_term`; ADR 0096).
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
- Tells a service where the log is below that recording: before the counter names a
  recording, the module writes the file `log-recordings-<member id>` in its driver
  directory, which lists the recordings that hold the member's log below the live
  one, each with where it begins and where it stopped, or, for the recording of a
  catch-up's replay that is still being made, what it has synced, so that a service
  at any position the member keeps replays the recording that holds it. A member
  whose log is in several recordings, one that left the log and joined it again or
  caught up through a replay, is read from position 0 as one that never left. A
  file that cannot be written is `RaftError::LogRecordings` from `do_work()`, which
  says what to do. In each call that fails so the member still records and
  replicates, and it names no recording until a later call has written the file;
  `cavalry run` makes that call again every 100 ms and gives the member up after
  10 s of calls that fail, as for any error of a duty cycle. It truncates no
  recording of its log while the counter names one, and removes the file when it
  closes, after it has stored −1 in the counter (`spec/cluster.md`, "The log's
  recordings"; ADR 0103).
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
  `memory::Cluster::with_election(member_ids, seed)` makes it: which member leads
  depends on the seed, `leader()` says which, `offer` goes to it, and `log_to(member, position)` is a member's log up to a position, so that
  a test can hold what one leader committed against the log of the next. It is a test
  double and allocates; its consensus stream holds at most 4,096 messages in flight and
  loses, and counts, one sent while it is full, and a leader's log publication and each
  member's log hold every entry appended or received for the cluster's life.
- Is held to gate 3's faults over real drivers, by three tests beside the ones in
  memory (ADR 0111). `tests/cluster.rs` stops the leader's driver: the two that remain elect
  another and commit. `tests/partition.rs` sends every datagram between the members
  through a relay that loses one data frame in 23 and holds back one in 7, and cuts a
  member off: a follower cut off joins again and holds what was committed without
  it, and a leader cut off is replaced, gives up the lead, truncates what no quorum
  held and joins as a follower. `tests/throttle.rs` runs each member as a process
  and gives one follower a twentieth of a processor for five seconds, by its cgroup
  where the runner allows it and by stop and continue elsewhere. In each, a service
  beside every member reads the same entries. Two costs these tests print, measured
  on a laptop: a member cut off for 10 s or more joins again about 10 s after the
  cut is mended, the driver's image cool-down (`spec/udp.md`, "The receiver"); and a
  throttled follower paces the cluster, since the leader's log is sent under
  `fc=min`: offered 30,000 entries of 96 bytes a second, the cluster committed
  about 12,000 a second while one follower was throttled, there by a stop of 95 ms
  in every 100 ms, one receiver window each 100 ms, though the other two members
  were a quorum (ADR 0052, "Harder"). Under a cgroup's quota of a twentieth of a
  processor, on CI's Linux runner, it committed 13,000 to 18,000 a second: the
  member runs a part of every period (ADR 0111, "Evidence").
- To come, each with its card: the C ABI's calls for a service, so that a service is
  a program in any of the five client languages (card 227), mixed versions in a
  rolling upgrade (card 228), the
  cluster client (card 229), snapshots
  and catch-up (card 230), and standby members, which follow the log without voting,
  in the open core.

In Aeron, this is Cluster, with Cluster Standby as a paid component
([glossary](../../docs/glossary.md)).

### What the election does not do yet

A truncation whose command faults after an earlier one succeeded leaves the member
reporting the end and the log term it had until the leader asks again; ADR 0095's
Consequences say where that is tested.

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

Three members in one process, with no driver: they elect a leader, the others join
its log, and an entry is committed once a quorum has recorded it.

```rust
use cavalry_raft::memory::Cluster;
use cavalry_raft::{ElectionState, Role};

let mut cluster = Cluster::with_election(&[0, 1, 2], 1)?;
// One second of the cluster's clock: every member canvasses, one nominates itself
// and wins the others' votes, they join its log and report, and every election closes.
cluster.run_ms(1_000);
let leader = cluster.leader().ok_or("no member leads")?;
assert_eq!(cluster.role(leader), Some(Role::Leader));
let closed = Some(ElectionState::Closed);
assert!([0, 1, 2].iter().all(|&member| cluster.election_state(member) == closed));

// The leader's own process appends an entry. It is committed when a quorum has
// recorded it, and every member learns the commit position from the leader.
let position = cluster.offer(b"hello")?;
cluster.run_ms(1_000);
for member in [0, 1, 2] {
    assert_eq!(cluster.commit_position(member), Some(position));
    // Every member's recording holds the same bytes.
    assert_eq!(cluster.log(member), cluster.log(leader));
}
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
  driver, in two layers, a core that does no I/O and the module that acts for it. Its
  leader was appointed until the election was built, and
  [ADR 0104](../../docs/decisions/0104-have-the-consensus-module-elect-its-leader-and-take-the-appointed-leader-out.md)
  took that path out.
- [ADR 0067](../../docs/decisions/0067-register-the-elections-state-machine-with-its-core-and-count-each-step.md),
  why the election's machine is registered with the core and what its two step
  counters count.
- [`spec/udp.md`](../../spec/udp.md), "The sender", for the multi-destination cast the
  log travels over, and [`spec/durable-log.md`](../../spec/durable-log.md) for the
  recording every member keeps of it.
- [`CLAUDE.md`](../../CLAUDE.md) for the invariants and the gates, and
  [ADR 0003](../../docs/decisions/0003-lay-out-the-workspace.md) for how the workspace
  is laid out.
