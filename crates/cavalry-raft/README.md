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
- Holds the member's log directory for the module's life (ADR 0152). At its start,
  before it reads or writes `terms`, `log-recordings` or `snapshots`, the module
  takes an exclusive advisory lock of the operating system on the directory itself,
  tried once. The operating system drops it when the module's process ends, however
  it ends, so a member whose process was killed starts again at once. A second module
  on the directory, of any member id and in any process, is refused with
  `RaftError::LogDirectoryHeld`, `log-directory-held`, and writes nothing; a
  filesystem that refuses the lock gives `RaftError::LogDirectoryLock`. The driver's
  own lock is on the catalog, so the two never meet. A start whose `terms` file is
  gone beside its other files, with no log, starts with its log empty, which ADR
  0145's rule keeps from a second vote; with a log it is `terms-lost`.
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
  lacks a committed entry. So does a log that does not reach back to where the
  start reads it from (`RaftError::LogForgotten`): a start reads the log from the
  position of the newest snapshot it can read, or from position 0 with none, so a
  list that begins above that position, or whose newest 1,024 recordings do, is a
  log the member cannot start on (ADR 0130). The module counts what its list did in four
  counters: `cluster.log_recordings_listed`, `cluster.log_recordings_unsynced`,
  `cluster.log_recordings_forgotten` and `cluster.truncations_finished`. It does
  not tell a member whose host lost power from one whose process was ended, and
  needs not: the driver's recovery cuts every recording a crash left live to the
  position it had synced, so the log a member starts on holds no frame past what it
  synced, and no frame a power loss took the body of (ADR 0144). A recording that a
  driver of version 4 of the durable log or before was making has no synced position
  to cut to: a member whose host lost power under such a driver starts on a new log
  directory, where it withholds its vote until it holds the log (ADR 0145).
  `tests/restart.rs` stops a follower, the leader and all three members over three
  drivers and starts them again, and has all three hosts lose power together, each
  member's log holding a frame past its sync whose body is gone: each driver cuts it,
  and the three start again on what they synced.
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
  `cluster.protocol_version`, the version of the cluster protocol the member is of,
  256 for version 1, which it stores once and which `cavalry doctor` names; it
  holds no peer's version (ADR 0137),
  `cluster.append_position`, `cluster.commit_position`, `cluster.messages_rejected`,
  `cluster.log_recording_id`, the recording that holds its log, which its service
  replays, the four counts of its list of its log's recordings (ADR 0108), and the
  twenty-two counts of `Counts::by_counter`. The
  counters of sessions in `spec/cluster.md`, "Counters", are `Sessions`'s, eight
  with the gauge, stored each cycle with the others (ADR 0116), and the counter of
  the service's version comes with that feature. One thread calls `do_work()`; the leader's process appends
  with `offer(payload)`, up to 1,344 bytes an entry. A consensus message whose step
  faults does not take the rest of its poll with it: `do_work()` hands every message
  the poll read to the election, as the next cycle would have, and returns the first
  fault. An entry that the leader's log publication refuses for a reason that waiting
  does not mend, a stream at its largest position, a closed client or an entry longer
  than the publication takes, is an `append` error, `RaftError::Append`, whose message
  says what to do; under back pressure, on a log no member has joined yet and where a
  term rotated, nothing is appended and the leader appends again. A follower that
  leaves the log holds its recording until the driver has said where it stopped, and
  that stop is where its log ends. No duty cycle waits for it: the cycle that asks the
  stop returns at once, the member stays in init, and each cycle after it looks again,
  so the thread that calls `do_work()` is never held for a stop. A leader that gives
  its log up, and a member that ends a catch-up or a replication, stop their
  recordings the same way. A member in init reads no consensus message: what arrives
  meanwhile stays on the stream for the cycle after it has left init. So does the
  rest of a batch: a message that moves the member to init in the middle of a poll
  ends the read there, and the messages the poll would have read after it, 15 at
  most, stay on the stream and are acted on once the member has left init, with a
  controlled poll (ADR 0114; work item 282). A stop that the
  driver refuses is a `client` error, and a stop position not read within 1 s is a
  `stop-timeout` error. Either way the member stays where it was with the recording
  held, and the next `do_work()` stops it again.
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
  it changes nothing. A command or a write that faults leaves the recordings cut as
  far as the commands went, and the core owes the truncation: it asks for it again
  each duty cycle and neither leads nor joins a log until it finishes, counted in
  `cluster.truncations_faulted` with the position in `cluster.truncation_owed` (ADR
  0148). `drop_terms_above` takes the terms a leader's record lacks out
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
  directory (`wipe`), one that elects started again on the log it kept (`restart`),
  a second run of one member id on a host of its own (`start_twin`) and the member
  id's address moved from one run to the other (`move_address`), which only the run
  that holds the address hears (work item 470),
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
  Three tests search seeds of it, each seed a run of these faults at moments it
  draws, a leader's stalled module among them, with the check of every step:
  `tests/lost_log_search.rs`, `tests/faulted_truncation_search.rs` and
  `tests/timer_search.rs`. Each file's header says what its search draws and which
  rule its seeds hold.
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
- Takes a snapshot of a member's sessions (`spec/cluster.md`, "Snapshots"; ADR 0129).
  The leader appends a cluster action that asks for one each time its log has grown
  by 256 MiB, and when its own process asks, `ConsensusModule::snapshot()`. Every
  member takes the snapshot when its read of its own committed log reaches the
  action, so the three snapshots are of one position and hold the same bytes: a
  marker, each open session in the order of the sessions' ids, the next session id
  that the log below the action gives, and a marker. That id is
  `Sessions::committed_next_session_id()`, which only a committed session open event
  moves, and not the leader's own, which moves at a connect it accepts. A member
  writes them to its one publication on
  the snapshot stream, as a recording of its own, and lists the snapshot in its
  snapshots file once the recording has stopped at the end of what it wrote (ADR
  0128) and its own log has synced to the snapshot's position, so that no crash
  leaves it a snapshot its log does not reach (ADR 0143): a log not synced there
  within `LOG_SYNC_WAIT`, 2 s, gives the snapshot up. Each step is one duty cycle's work: the longest cycle while a snapshot was
  taken measured 25.44 ms at the most over 50 snapshots on a laptop, and 19.71 ms
  at the median. A step that fails is counted in
  `cluster.snapshots_failed`, and the member goes on with its log whole.
- Takes a control toggle while it leads (`spec/cluster.md`, "The control toggle";
  ADR 0147): the counter `cluster.control_toggle` that a command on the member's
  host adds to ask it for a snapshot, keyed by the member's id and the toggle's id
  and holding 4. The leader finds it by reading at most 16 metadata records of its
  driver's counters file no more often than each millisecond, in a ring: over
  three runs of 20,000 looks on a laptop a look measured p99.99 250 ns, 334 ns and
  292 ns, and 417 ns at the most (ADR 0147, "Evidence"), and a member that does not
  lead reads nothing. It appends one cluster action for each toggle it takes, when
  it takes no snapshot of its own and has read its log past the last action it
  appended, and stores the snapshot's log position in
  `cluster.control_toggle_snapshot_position` and then the toggle's id in
  `cluster.control_toggle_taken`, where the command reads its answer. It takes a
  toggle at most once, and none whose id is at or below the last it took: a toggle
  that stays is not taken again and holds no other back, and one with a lower id
  than the last taken is answered by that one's snapshot. The command is the toggle's one
  writer; the member writes nothing in it. `cavalry snapshot` is that command
  ([Ask a cluster for a snapshot](../../docs/how-to/ask-for-a-snapshot.md)).
- Holds the timers of the service beside it, and appends their events while it
  leads (`spec/cluster.md`, "The service", "Timers"; ADR 0151). A service tells the
  member on its driver of each pending timer with a schedule timer, and of each
  that is pending no more with a cancel timer, on the consensus module stream,
  which the member reads at most 10 messages of in a duty cycle, whatever its role.
  `MemberTimers` keeps what it was told, at most 2,048 timers, allocated when the
  member starts. A leader with its election closed appends a timer event for each
  timer whose deadline its wall clock has reached, the earliest first and at most
  10 in a duty cycle, marks the timer, and appends no second event for it until a
  schedule timer names it again. A duty cycle with no timer due costs one compare
  while the table has not changed, and one scan of it, 2,048 slots at the most,
  after a timer message or an append.
  What the member holds is in no snapshot and is no state it computes from the
  log: a member that starts holds none, and a service sends every pending timer
  again each second (`spec/cluster.md`, "Timers"); no service library has the
  calls yet. Which event is handed to a program is the service's to
  decide, from the log alone, so a table that is behind or ahead costs an entry
  that every service passes over and never a timer handed over twice.
  `cluster.timers`, `cluster.timer_events`, `cluster.timers_replaced` and
  `cluster.timers_cleared` count it. `tests/timer_search.rs` searches seeds of a
  cluster in memory for a timer handed over twice, early, after its cancel or not
  at all.
- Starts a member from its newest snapshot (`spec/cluster.md`, "A member's start";
  ADR 0130). The start reads the member's snapshots file, plays the newest
  snapshot of the consensus module back through the driver, and takes the sessions
  and the next session id from it; a snapshot whose recording is gone, or does not
  hold what the list names, or holds sessions the member's table does not take, is
  passed over for the one before it, counted in `cluster.snapshots_passed_over`: a
  member never begins at a snapshot's position with another state than that
  snapshot's. The member's read of its log then begins at the
  snapshot's position, `cluster.snapshot_start_position`, and not at 0. So the log
  need be whole from that position alone: a member starts though it forgot the
  recordings below it, though they were purged, and though its log is in more than
  1,024 recordings. With no snapshot it starts as before, from position 0. A
  purge counts as no truncation in `cluster.truncations_finished`, which counts
  the list's last recordings gone. Those end the log where the ones left end only
  while that is at or above the snapshot's position: a truncation never cuts below
  a commit position, so a log they leave ending below it is refused,
  `ListedFound::EndsBelowSnapshot`, and not taken as shorter than the member
  reported. A log that the catalog holds whole and that ends below the snapshot's
  position is refused as well, `RaftError::LogShort`: its end was cut, or the
  directory is older than the snapshot. A
  service reads its member's log from its own snapshot's position, or from
  position 0, so a member's log is needed from the lower of the two snapshots'
  positions: `cluster.log_purge_position`, and
  `ConsensusModule::log_purge_position()`. It is 0 while the member lists no
  snapshot of a service, and nothing below a position of 0 is purged (ADR 0132).
  The position assumes a service whose program calls `ClusterService::restore` at
  its start, which the member cannot tell: a service that takes snapshots and
  never calls it reads from position 0, and beside it nothing may be purged.
- Lists the snapshots of the service beside a member (`spec/cluster.md`, "The
  member's list of a service's snapshots"; ADR 0131). A service writes its own
  snapshot, a recording through the member's driver, and tells the member with a
  service ack on the consensus module stream, `cavalry:ipc` stream 105, which the
  member reads in every duty cycle, 10 messages at most. The member lists the
  snapshot in its snapshots file, with service id 0, when its catalog holds the
  recording as a stopped recording of the service snapshot stream, the position is
  above 0 and at or below its commit position, and the position is a multiple of
  32 above the newest it lists for the service; it does not check that an entry
  of its log ends there. `cluster.service_snapshot_position` holds that
  position, and an ack that fails a check is counted in
  `cluster.service_acks_rejected` with nothing listed. The member answers no ack
  and waits for none: it does not know its services (ADR 0066), so its own
  snapshot counts without a service's. `ConsensusModule::service_snapshot_position()`
  and `service_snapshot_counts()` say what it listed. The member names the newest
  one's recording in `cluster.service_snapshot_recording_id`, which a service that
  starts from its snapshot reads (ADR 0132).
- Holds a member's sessions, the member's side of the session protocol
  (`spec/cluster.md`, "Sessions"; ADR 0115): `Sessions` is plain state, as `Consensus`
  is. Its caller hands it each message of the ingress stream, each entry of the
  member's own log at or below the commit position, and a duty cycle, and it sends
  and appends through a trait, `SessionIo`. A follower answers a session connect with
  a redirect that names its leader and every member's ingress endpoint. The leader
  refuses a connect for its version, its response channel or the limit of 10
  sessions, or accepts it: the session has the next id and an egress publication,
  and its session open event goes to the log. A session is opened by its committed
  session open event and closed by its committed session close event, on every
  member, so a follower holds the sessions its leader opened and takes them up when
  it leads: a publication for each, the term's new leader event, and the session
  timeout counted from when it begins to serve. A member that wins an election reads
  every entry of the terms before its own before it serves sessions (ADR 0116), so
  it has every session they opened before it accepts a connect and gives no session
  id twice. The leader appends a session's messages
  with its own term and the cluster's time, leaves one unread while the log is
  back-pressured (`Ingress::Unread`), and closes a session it has heard nothing from
  for 10 s: the session close event goes to the log first, and the closed event
  follows, owed until the session's publication takes it or the close is
  committed. It counts what it refuses, drops and ignores in `SessionCounts`, eight
  counters of `counters.toml` with the gauge `cluster.sessions`. It allocates nothing
  after `Sessions::new`.
- Serves those sessions over a driver (ADR 0116): the consensus module subscribes to
  the ingress stream on its member's `ingress` endpoint and reads it with a
  controlled poll, so that a message its sessions leave unread stays on the stream
  and holds its client, and no message of another client is read past it. It answers
  on exclusive publications to the clients' response channels, 20 at most. It reads
  its own committed log as a service does, with `Cavalry::committed_log_on` on a
  stream of its own, the log replay stream, 103, in every role,
  and that read alone opens and closes its sessions: nothing above the commit
  position is in its table. A leader serves sessions from when its election has
  closed and that read has reached its term's base, so that it has every session and
  every session id of the terms before it. A message of the ingress stream is one
  frame, 1,344 bytes of payload at most; a fragment, and an entry its table cannot
  hold, are counted in `cluster.messages_rejected`. So is a session message longer
  than 1,344 bytes that a sender with a longer MTU delivers in one frame
  (`Ingress::Rejected`): nothing of it is acted on, and it holds no client's
  message behind it. The module's append refuses an entry longer than one frame
  of the log as a fault, `RaftError::Append` with `too-long`, and never as back
  pressure. A members string whose ingress
  endpoints do not fit one frame of the egress stream as a redirect lists them,
  1,320 bytes, is `RaftError::IngressEndpoints`, and the member does not start.
- Follows as a standby: a member whose entry in the members string ends in
  `,standby` records the log, reads its committed log for its sessions, takes the
  snapshots its leader asks for and redirects a client, and it has no vote. It
  never nominates itself, and it answers a request vote with nothing, counted in
  `cluster.standby_vote_requests`. A voter counts the voters of its own string
  alone, for a quorum, for a commit and for its ballot, and waits for no standby.
  It sends to every member and reads every member's messages, so it votes for a
  candidate, and follows a leader, that its own string marks a standby: that is
  what lets two strings one mark apart run side by side while a mark is changed.
  `Consensus::with_standbys(voter_ids, standby_ids, member_id, seed)` is that
  member, and `ConsensusModule::start` makes it from the string.
  `cluster.voters` and `cluster.standby` say which string a member runs. A
  standby is promoted by starting each member again with a string that gives it a
  vote, one mark at a time and each change complete before the next:
  `spec/cluster.md`, "Changing a standby's mark", has the three rules, which no
  member checks, and `docs/how-to/promote-a-standby.md` the steps. A test runs
  what the rules prevent: two marks changed at once elect a leader that lacks a
  committed entry (ADR 0134).
- Brings back a member that lost its log directory with no vote on what it forgot
  (`spec/cluster.md`, "A member that lost its log"; ADR 0145, ADR 0161). A member whose
  log is empty while another member's report holds a log, or that holds a vote floor,
  withholds: it never nominates itself, and it answers a request
  vote with nothing, counted in `cluster.votes_withheld`, unless every voter has
  reported a log the candidate's is at least and no voter but the candidate has named
  the request's term or a later one. Taking a leader with its log empty,
  it stores the leader's append position as its vote floor, through
  `Io::store_vote_floor`, in its `terms` file before it records a term, 0 included;
  `Consensus::vote_floor()` and the gauge `cluster.vote_floor` hold it. It clears the
  floor, through `Io::clear_vote_floor`, once in one run it has joined a leader's
  live log, going to follower ready, with its append position at or past the floor
  and at or past its leader's floor answer to that run, counted in
  `cluster.vote_floors_cleared`. The live log's image joins past every
  entry the leader's sender sent it before it came back, so a new leadership term the
  sender held back, written before an entry it had acknowledged, gives it no vote
  before it holds the entry; the floor holds what the leader's duty cycle had read as
  its append position before it wrote the term the member took, and an entry the
  recording synced after that read, which a replay may have brought the member's
  earlier run, stands above it (ADR 0163, which corrects ADR 0161 here). While it holds a floor it asks its leader with
  the floor flag of each append position, and a leader answers with a floor answer
  that names the session the report came in, read through `Consensus::on_message_in`;
  the member takes one for its own session, `Io::consensus_session`, keeps the lowest
  in `Consensus::floor_answer()` and the gauge `cluster.floor_answer`, and so holds,
  once it clears, every entry a late report of its earlier run can commit (ADR 0163).
  A voter
  whose log is empty takes a leader only when it voted it in, or when every other
  voter has named a term in this election and none named one above the leader's
  ("Taking a leader"): a leader that a later term deposed answers it too. It
  ignores any other new leadership term, counted in `cluster.leaders_withheld`, and
  a follower that holds a log answers its canvass position with its own, so that it
  hears the followers.
  `tests/lost_log_directory.rs` makes the history: an entry committed by two, one of
  them on an empty directory, the other stopped, and no leader elected without the
  entry until the stopped one runs again. It makes the review's schedule too: a
  deposed leader that answers the member with no log, which follows it not. And it
  makes the how-to's path: a follower on an empty directory while the other two run
  on, with no election, takes the leader once the other follower answers it.
  `tests/canvass_answer.rs` pins that answer, one core at a time: who sends it, what
  it holds and to whom, and when nothing is sent. The rule rests on what a member
  started again reads: nothing that a peer sent before its start, as on the new
  driver it starts on ("Taking a leader", "What the second case rests on"). So
  `memory::Cluster::wipe` and `memory::Cluster::restart` lose every consensus message
  in flight to the member. With the review's schedule and the canvass positions of
  the new leader's canvass held late past the wipe, `tests/lost_log_directory.rs`
  fails without that loss. A member that withholds and votes all the same, every
  voter heard, votes in no term that a voter other than the candidate has named, so
  that it votes no second time in a term it voted in before it lost its log:
  `tests/vote_in_one_term.rs` pins it, one core by hand.
- Runs beside members of another version of its major, as the members of a cluster
  do while it is upgraded one member at a time (`spec/cluster.md`, "Versions and
  the upgrade"; ADR 0136; work item 231). A member writes its own version, 256 for
  version 1, in canvass position and in request vote, reads a peer's for its major
  alone, and votes for a candidate, follows a leader and counts toward a quorum of
  any version of its major. It holds its own version in
  `cluster.protocol_version`, and no peer's (ADR 0137). `conformance/upgrade/`
  runs the upgrade on the loopback, from the last tree of the version before to
  the tree under test, and `docs/how-to/upgrade-a-cluster.md` has an operator's
  steps.
- Built on it since, in other crates (work item 229): the cluster client and the
  service's replies, `Cavalry::connect_cluster` with `ClusterSession` and
  `Cavalry::cluster_service` with `ClusterService` in `cavalry-client`
  ([ADR 0117](../../docs/decisions/0117-open-a-cluster-session-from-the-client-with-a-publication-to-every-member.md),
  [ADR 0118](../../docs/decisions/0118-reply-to-a-session-from-the-service-beside-the-entrys-leader-and-wait-for-no-client-past-the-keepalive-interval.md));
  the C ABI's calls for both, and a session object and a service object in each of
  the five generated clients, so that a client or a service is a program in any of
  their languages
  ([ADR 0124](../../docs/decisions/0124-give-each-client-a-session-object-and-a-service-object-whose-handlers-failure-aborts-its-entry.md));
  and the tutorial that runs three members, a service and a client,
  [Run a cluster in ten minutes](../../docs/tutorials/run-a-cluster.md).
- To come, each with its work item: the gate across three hosts (work item 232), a
  service's snapshot through the five clients, which the C ABI has since its
  version 6 (work item 335; ADR 0146), and a
  start from a peer's snapshot, for a member that lost its log directory beside
  members whose logs no longer begin at position 0 (work item 365).

In Aeron, this is Cluster, with Cluster Standby as a paid component
([glossary](../../docs/glossary.md)).

### What the sessions do not do yet

A message of the session protocol is one frame. A client's message and a service's
reply carry at most 1,344 bytes, and nothing longer is reassembled (ADR 0116, ADR
0118). A service cannot close a session, and no event tells a client that a reply
to it was dropped (ADR 0118, Revisit). The protocol has no authentication: the
challenge and its response are reserved (`spec/cluster.md`, "Out of this
version").

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
- [ADR 0115](../../docs/decisions/0115-keep-a-clusters-sessions-on-every-member-opened-and-closed-by-the-log.md),
  why every member holds the sessions, opened and closed by committed entries of
  the log, and what the leader adds to that table.
- [ADR 0116](../../docs/decisions/0116-read-a-members-sessions-from-its-committed-log-and-serve-them-from-its-terms-base.md),
  how the consensus module reads its own committed log for its sessions, and why a
  leader serves them only once that read has reached its term's base.
- [ADR 0067](../../docs/decisions/0067-register-the-elections-state-machine-with-its-core-and-count-each-step.md),
  why the election's machine is registered with the core and what its two step
  counters count.
- [`spec/udp.md`](../../spec/udp.md), "The sender", for the multi-destination cast the
  log travels over, and [`spec/durable-log.md`](../../spec/durable-log.md) for the
  recording every member keeps of it.
- [`CLAUDE.md`](../../CLAUDE.md) for the invariants and the gates, and
  [ADR 0003](../../docs/decisions/0003-lay-out-the-workspace.md) for how the workspace
  is laid out.
