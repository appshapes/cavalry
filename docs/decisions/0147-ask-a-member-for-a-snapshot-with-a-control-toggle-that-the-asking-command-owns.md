# 0147. Ask a member for a snapshot with a control toggle that the asking command owns

- **Status:** Accepted
- **Date:** 2026-11-22
- **Invariants affected:** 1, 2, 3, 4, 6
- **Supersedes:** none. Moves one item of ADR 0138's feature order: the command that
  asks the leader for a snapshot, which ADR 0138 placed in feature 5 (work item 365),
  lands with work item 335, the first part of feature 6. Changes `spec/cluster.md`: a
  paragraph of "Snapshots", "The control toggle", and a sentence of "The action";
  three rows of "Counters", which `counters.toml` gains as ids 184 to 186, and the
  text of `cluster.snapshot_actions`; three rows of "Values"; and a sentence of "Out
  of this version". The page's version stays 1: no message, layout, template id,
  enumeration or check of "What a reader rejects" changes, and no entry does anything
  new to the state.

## Context

Work item 335 is done when "a service in each of the five client languages takes a
snapshot that its member lists, in `just conformance`". The cluster round of each
runner starts its three members as `cavalry run` processes. A leader appends the
cluster action that asks for a snapshot in two cases (`spec/cluster.md`,
"Snapshots"): when its log has grown by the snapshot interval, 256 MiB, and when its
own process calls `ConsensusModule::snapshot`. `cavalry run` never calls it, and
nothing else reaches a running member. So no round can have a snapshot taken, and
feature 6's own condition, a snapshot in six languages in `just conformance` (ADR
0138, condition 2), waits on a command that ADR 0138 lists under feature 5, on the
other chain.

In Aeron, an operator runs `ClusterTool snapshot` on the leader's host. The
consensus module allocates a counter, the cluster control toggle; the tool writes
`SNAPSHOT` into it by compare-and-set from `NEUTRAL`; the module, which reads the
toggle in its duty cycle, appends the action and sets the toggle back to `NEUTRAL`
when the snapshot is taken.

Two rules of Cavalry's own pages bear on how the request reaches the member. A
counter's value has one writer: "a counter's value is written by the counter's
owner, the driver or the client the driver allocated it for", and a counter a client
adds has "its value written by that client and never by the driver"
(`spec/counters-file.md`). And within one major of `spec/cluster.md` no version
changes "a template's id ... a check of 'What a reader rejects'" ("Versions and the
upgrade"; ADR 0136): a new message, or a known one on a stream that does not carry
it today, is another major, whose members form no cluster with members of major 0.

## Decision

A command on a member's host asks that member for a snapshot by adding a counter,
`cluster.control_toggle`, through the member's driver, keyed by the member's id and
the toggle's id, the command's client id, and holding 4, Aeron's `SNAPSHOT`. The
command is the counter's one writer, and removes it when it ends. A leader that
serves sessions looks for a toggle of its own id in its driver's counters file, at
most 16 metadata records no more often than each millisecond, in a ring. It takes
one when it takes no snapshot of its own and has read its log past every action it
appended: it appends the cluster action, and stores the snapshot's log position in
`cluster.control_toggle_snapshot_position` and then the toggle's id in
`cluster.control_toggle_taken`, each with a release store. It serves one toggle at a
time and each once: it remembers the counter and the id of the toggle it took for as
long as that counter holds it, and takes no toggle whose id it took last. The
command reads its answer in those two gauges and in `cluster.snapshot_position`.
The item moves to work item 335 because the first feature that needs a thing builds
it: the round of feature 6 needs it, and nothing else of feature 5 moves.

## Alternatives considered

- **Aeron's split: the member allocates the toggle, and the command writes it by
  compare-and-set.** Lost. It puts a second writer on one value slot, which changes
  the writer rule of `spec/counters-file.md` that every counter relies on, and needs
  a way for a process to write a slot it does not own, in `cavalry-shm`, the crate
  where `unsafe` lives. A command that adds its own counter is what a cluster's
  service does already for `cluster.service_snapshots_taken`, "with its member's id
  as the key", through the client protocol's `add counter`. The name and the value
  are Aeron's; who allocates is the difference, and the page says so once.
- **A message on the consensus module stream, Aeron's admin request (template 26)
  with the request type snapshot.** Lost. The stream carries template 33 alone, and
  the page names 26 under "Out of this version". Either change is a check of "What
  a reader rejects", which the page's own rule makes a new major: a cluster could
  not be upgraded one member at a time across the change. It would have cost the
  member nothing to read, since it reads that stream each duty cycle already.
- **An admin request from a session, on the ingress stream, as Aeron's newer clients
  send it.** Lost, for the same rule, and because any client that can reach the
  cluster could then ask for snapshots: Aeron guards it with an authorisation
  service, and this version has no authentication.
- **A test-only member in `conformance/` that calls `ConsensusModule::snapshot` on a
  signal.** Lost. The round is there to hold `cavalry run`, and the helper would be
  a second way to run a member that nothing else uses. An operator needs the command
  anyway (ADR 0138, condition 8).
- **A round that writes 256 MiB to the log.** Lost. It is about 200,000 messages
  through three members for each of six runners.
- **The snapshot interval as a configuration key, set low in the rounds.** Lost.
  A key added to make a test pass is what `CLAUDE.md` rules out, and it gives an
  operator no way to ask for one snapshot now.
- **The leader reads the whole counters file on a timer.** Lost. The file has no sign
  that a counter was added: its header never changes, and a record holds no sequence.
  A pass over 16,384 records on the thread that replicates and commits the log is a
  stall of that thread each time. A slice is bounded.
- **A larger slice, 64 records each millisecond.** Lost. A pass in 256 ms answers
  the command sooner than it needs: the command waits for 10 s, and a pass in 1,024
  ms, or 4,096 ms over the largest file, leaves the snapshot the rest. 16 records is
  the smallest look whose pass over the largest file is under half of the wait.
- **A toggle served by its value: the command writes a second value once it is
  answered.** Lost. A command that dies writes nothing, and its toggle stays until
  the driver times its client out. The leader's memory of the counter it served
  covers both.
- **The leader takes a toggle whose id is above the highest it took.** Lost. Of two
  toggles the look may meet the higher first, and the lower would then never be
  taken. Serving one at a time until its counter is free takes both, in the order
  the ring meets them.
- **The command waits for `cluster.snapshot_actions` to rise, or for
  `cluster.snapshot_position` to pass the commit position it read.** Lost. An action
  the interval appended a moment before the request moves both, and the command
  would return for a snapshot that is not its own. The two gauges are of its own
  toggle.

## Evidence

- `crates/cavalry-raft/tests/snapshot.rs`,
  `a_leader_takes_a_control_toggle_once_and_every_member_takes_its_snapshot`: three
  members over three drivers on the loopback, each toggle added by a client of a
  member's driver as a command adds it. A toggle beside a follower and one beside
  the leader that holds 2, Aeron's suspend, stay for 1.3 s, longer than a whole
  pass, and no member takes either: every `cluster.control_toggle_taken` is −1 and
  no action is counted. A toggle beside the leader that holds 4 is taken: the leader
  stores the toggle's id, and the position it stores is the position after the
  action's entry, at which every member lists its snapshot;
  `cluster.snapshot_actions` is 1. With that toggle still there and a second added,
  1.3 s later the leader has taken neither again: one request is one action. When
  the first is removed the second is taken, with a snapshot one entry later that
  every member lists. The toggle that held 2 is taken once its command stores 4 in
  it and the second has gone. No follower takes a toggle or counts an action.
- What one look costs the thread that runs a leader, timed in a release build on an
  Apple M5 Pro laptop, macOS 26.6.1, by the ignored test
  `a_control_toggles_look_timed`, 20,000 looks of 16 records over a member's
  counters file of 16,384 records, three runs at a load average of 3: p50 41 ns or
  below the clock's step, p99.99 250 ns, 334 ns and 292 ns, max 417 ns, 417 ns and
  333 ns. At one look each millisecond that is under 0.05% of the thread at the
  longest look measured. No Linux host has timed it.
- The stores and the load are the ones `spec/counters-file.md` gives a position:
  `Counter::store_release` for both gauges, the position first, and an acquire load
  of the id by the command. No code of `cavalry-shm` changes.

## Consequences

- A leader pays one look each millisecond for as long as it leads, with or without
  a command, and nothing for a message. A member that does not lead pays nothing.
- A request is answered within about a second of its toggle's add by the look
  alone, 4 s over the largest counters file, before the snapshot's own time.
- The command is the next change of work item 335, `cavalry snapshot`, with its
  how-to; then each client's round asks for its snapshot with it. Feature 5 keeps
  its other command, the one that shows a cluster's members, leader and positions.
- A member of a build before this one adds no `cluster.control_toggle_taken` and
  serves no toggle. The command looks for that gauge first and says so at once, so
  an upgrade one member at a time is not held for the command's wait.
- Two more counters for each member, and one for each command while it asks, of the
  256 a driver gives its clients.
- A member's memory of the toggle it serves is its process's: a member that starts
  again and leads takes a toggle it finds, which is at most one more snapshot.
- Revisit: when the counters file gains a sign that a counter was added, for a look
  that reads nothing until it moves; when a command must suspend or resume a
  cluster, for Aeron's other toggle states; when the page takes its next major, for
  the admin request, which costs the member no look.

## References

- Work item 335 on the Brigade Board, "Take a service's snapshot through the C ABI
  and the five clients"; work item 366, feature 6; work item 365, feature 5, which
  the command is moved from.
- `spec/cluster.md`: "Snapshots", "The action" and "The control toggle"; "Counters";
  "Values"; "Out of this version"; "Versions and the upgrade". `spec/counters-file.md`,
  the opening and "Value slot"; `spec/client-protocol.md`, "Commands", add counter.
- `crates/cavalry-raft/src/toggle.rs`, `src/module.rs` (`take_control_toggle`),
  `tests/snapshot.rs`; `crates/cavalry-proto/src/cluster.rs` (`control_toggle_key`,
  `control_toggle_id`); `counters.toml`.
- [ADR 0138](0138-draw-the-finish-line-at-a-beta-that-is-an-alternative-to-aeron-and-build-it-sixth.md),
  the feature order;
  [ADR 0136](0136-run-the-versions-of-one-major-as-one-cluster-and-upgrade-from-the-last-tree-of-the-version-before.md),
  what a version of one major may change;
  [ADR 0129](0129-snapshot-a-members-sessions-when-its-read-of-its-log-reaches-the-leaders-action.md)
  and
  [ADR 0131](0131-have-a-service-write-its-own-snapshot-and-its-member-list-it.md),
  the action and a service's counters keyed by its member's id;
  [ADR 0013](0013-lay-out-the-log-buffer.md), the release store and the acquire
  load of a position.
