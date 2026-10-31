# Changelog

All notable changes to Cavalry are recorded here, in the reader's words, one line per
user-visible change, added in the same commit as the change. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## Unreleased

### Added

- Repository scaffold: `CLAUDE.md` invariants and gates, documentation rules, ADR
  template and ADR 0001, glossary, and the competitive brief
  (`docs/explanation/why-cavalry.md`).
- ADR 0002: `cavalry` is the one crate users add and install; `cavalry-cli` is
  removed from the crate map; the crate family is reserved on crates.io.
- Claude Code skills under `.claude/skills/`, ported from Brigade: `/commit` runs
  `just lint` and `just test`, then commits, pulls and pushes to `master`;
  `/trello-create` and `/trello-read` work the Cavalry list on the AppShapes board;
  `/playwright-cli` drives a browser.
- Cargo workspace with the nine crates from the repository map, one shared version
  0.1.0, edition 2024, `publish = false` on every crate. `cavalry` builds a binary
  that prints "not built yet" and exits 1.
- `just lint` passes for real: `unsafe` is denied outside `cavalry-shm` and
  `cavalry-ffi`, locks are banned on the data plane through `clippy.toml`, and
  `scripts/lint-workspace.py` checks the data-plane flags, the dependency allowlist,
  the absence of `async`, and that `cavalry` holds only re-exports.
- GitHub Actions runs `just lint` and `just test` on Linux and macOS Blacksmith
  runners for every push to `master` and every pull request. Actions are pinned to
  commit SHAs; Dependabot opens a weekly pull request to move them.
- A README in every crate says what it does and what it must never do, and is the
  crate's rustdoc front page. The example and reading list say "not yet" and link
  ADR 0003.
- ADR 0003: one workspace version, `publish = false` until the first release, where
  each lint lives and why, why crate README examples are deferred, and that bots and
  agent workflows use pull requests while developers commit to `master`.
- ADR 0004: bots and agents work on branches; the writer, reviewer and
  auto-merge workflows, which token each job holds and why, squash merges whose
  message is the pull request body, and Blacksmith runners for every job.
- A reviewer agent reviews every same-repository pull request, read-only, and
  approves or requests changes; the job fails if no review lands. The `review` label
  runs it again. The briefs for the reviewer and the writer are in `.claude/agents/`
  (ADR 0004).
- A writer agent implements an issue labelled `claude` on a branch and opens the
  pull request, answers `@claude` comments, and applies the reviewer's blockers up to
  three times. `docs/how-to/ask-the-agent.md` is the page for asking (ADR 0004).
- A pull request labelled `auto-merge` squash-merges on its own once the reviewer
  approves its head and the two CI checks pass; the commit's subject and body are the
  pull request's. Dependabot labels its pull requests `auto-merge` (ADR 0004).
- ADR 0005: the reviewer runs on `master`, started by a small dispatcher
  from each same-repository pull request's events, so a pull request that changes the
  reviewer's own workflow file, Dependabot's bumps of its pins included, is
  agent-reviewed, Dependabot's pull requests are reviewed directly, and the hand-off
  workflow is gone, and so is the "cannot run on this pull request" comment. The
  `review pr` run is on the Actions tab, not among the pull request's checks.
- `just docs` builds rustdoc with warnings denied and the mdBook under `target/book`,
  runs the book's Rust code blocks, and checks every relative link and anchor in
  `docs/` and the READMEs. `just lint-docs` fails a change to a public item, `spec/`,
  `counters.toml` or `config.toml` that touches no documentation, a page missing from
  `docs/SUMMARY.md`, a malformed ADR, or an edit to an accepted ADR. CI runs both
  (ADR 0006).
- ADR 0007: the first slice is publish/subscribe over shared memory on one host,
  with the README's ten-minute path as its test, built as five features in a fixed
  order, the data frame header first; UDP, the durable log, Raft and the language
  clients wait for the next slice.
- `spec/` exists. `spec/README.md` states the conventions, and `spec/frame.md`
  specifies the 32-byte data frame header at version 0, Aeron's layout field for
  field, with the checks a reader runs and the reason it reports. Its 18 test
  vectors are written by a reference encoder, `spec/vectors/frame.py`, and the book
  includes the page as its first reference page (ADR 0008).
- ADR 0009: `cavalry-proto` reads and writes wire structs through `zerocopy`, its
  first dependency, so that a struct is the layout table.
- `docs/explanation/should-zigbolt-inform-cavalry.md` answers whether ZigBolt's
  design should inform Cavalry's: no, except two shared-memory layout points,
  proposed as ADR 0010 (validate every peer-written shared-memory field before
  using it) and ADR 0011 (pad peer-shared hot fields to 128 bytes). The data
  frame header of ADR 0008 is unchanged.
- `cavalry-proto` reads and writes the data frame header. `DataFrameHeader::parse`
  runs the spec's four checks in order and names the reason it rejects a frame,
  `write` puts a header in place, and every error message says what to do and ends
  with the docs anchor. The 18 spec vectors run on every `just test` and alone under
  `just conformance`, a no-panic test feeds the parser generated bytes on every
  commit, and `just fuzz` runs cargo-fuzz over it (ADR 0008, ADR 0009).
- Dependabot moves Cargo dependencies as it moves action pins: one grouped pull
  request a week for the workspace and one for the fuzz crate, reviewed by the agent
  and merged when CI is green (ADR 0009).
- `docs/how-to/implement-a-feature.md`: the four steps from a card to commits on
  `master`, for people and agents, with the data frame header (card 80) as the worked
  example, commit by commit. The README's "Start here" table and the writer agent's
  brief point at it.
- `spec/counters-file.md` specifies the counters file at version 0: a 128-byte header
  written magic-last, Aeron's counter metadata records at 256 bytes, a 128-byte value
  slot per counter, a writer, reader and bound for every field, the checks a reader
  runs in order, and what happens when the file is full. Its 21 vectors come from
  `spec/vectors/counters-file.py`, and the book includes the page (ADR 0012).
- `counters.toml` is the single source of every counter, starting with
  `counters.allocated` and `counters.allocation_failed` (ADR 0012).
- `/next-task` says what the next task is, read from the repository alone: `master`'s
  CI, open pull requests and issues, the Cavalry list, the slice's feature order (ADR
  0007), the gates that print `not built yet`, Proposed ADRs, fired Revisit clauses and
  the active sessions. `/next-task 3` lists three, in order, with what each waits on,
  and every task names its source.
- `cavalry-shm` has its first public items: the counters file. `CountersFile::create`
  lays out the file of `spec/counters-file.md`, allocates every static counter and
  publishes the header magic-last; `allocate` hands out a `Counter` whose `increment` is
  one relaxed add, and fails with `full` when every record is in use;
  `CountersReader::open` runs the spec's checks in order and names the reason it
  rejects a file. `scripts/gen-counters.py` writes `docs/reference/counters.md` and the
  Rust constants from `counters.toml`; `just lint-counters` fails when they are stale and
  is part of `just lint`. `just loom` checks the publish protocols and `just miri` the
  layout layer (ADR 0012).
- `spec/log-buffer.md` specifies the log buffer at version 0: Aeron's three term
  buffers and metadata page, with Cavalry's magic-last header, a 128-byte slot for every
  field written after creation, the claim, commit, abort, rotation and reading
  protocols, the driver's duties, and the eleven checks a reader runs in order. Its
  vectors come from `spec/vectors/log-buffer.py`, and the book includes the page
  (ADR 0013).
- The log buffer's positions are counters: `publication.position`, `publication.limit`
  and `subscription.position` in `counters.toml`, written by their owners, the driver or
  a client, with release stores; `spec/counters-file.md` now says so, and the one-writer
  rule of ADR 0012 is amended by ADR 0013. `publication.unblocked` and three
  `log_buffer.rejected_*` counters count the driver's unblocks and refusals.
- `DataFrameHeader::parse_within` runs the frame checks for a reader that holds the
  header's bytes and knows how many bytes the frame may occupy, which is how the log
  buffer's reader checks a frame in a term (ADR 0013).
- `cavalry-shm` builds the log buffer. `LogBuffers::create` lays out the file of
  `spec/log-buffer.md` and `open` runs the spec's checks in order; a `TermAppender`
  claims a frame in place with one fetch-and-add and commits it with one release store,
  offers a message by copy with fragmentation, pads and rotates at the end of a term and
  refuses a claim by name at the limit; a `TermReader` reads committed frames where its
  position says after the checks of `spec/frame.md`; the driver's duties, the producer
  position, cleaning, unblocking, the connected flag and the end of stream position, are
  methods on the log. A position counter is stored with release and loaded with acquire
  through `Counter::store_release` and `CountersReader::read_acquire`. The 51 spec
  vectors run on every `just test`, `just loom` checks the claim protocol and the tail,
  and the README example is the log buffer (ADR 0013).
- `just bench` is real: `cavalry-bench` under `bench/` drives a publisher, a
  subscriber and a conductor thread over a real log buffer and counters file and
  reports HdrHistogram p50, p99, p99.99 and max of claim and commit, of publish to
  delivery and of a slot-padding microbench, with zero allocations per message asserted;
  `just bench-compare` fails a p99.99 regression over 5 percent against
  `bench/baseline.json` for the runner's profile, and CI runs both and keeps the result
  as an artifact. `docs/how-to/run-the-bench.md` is the page (ADR 0015).
- `just bench-unchecked` runs the harness with the data-plane checks built out, under
  the profile `<profile>-unchecked`, so that what ADR 0010's checks cost is measured on
  every CI run; the slot-padding microbench now times the store side as well as the
  load side (ADR 0011).
- ADRs 0010 and 0011 are Accepted on the bench's evidence from both CI runners: the
  per-frame and per-claim checks cost nothing measurable at p99.99, and a field 64 bytes
  from a hot store pays for the prefetcher's pair while 128 bytes does not. The data-plane
  rules and the explanation page now say 128 bytes for a peer-shared hot field, and
  `just lint` refuses an atomic in an `align(64)` wrapper. `bench/baseline.json` holds
  the runners' baselines, checked and unchecked, so `just bench-compare` now fails a
  regression in CI. Both specs say that whoever branches on a position, the driver or
  the publisher, loads it with an acquire load.
- The latency gate runs on a dedicated bench host, a self-hosted runner with the label
  `bench-linux`, tuned by `bench/host/setup-ubuntu.sh` and checked by
  `bench/host/check.sh` on every run; the shared CI runners still run `just bench` and
  keep the table, but their numbers are not compared. `just bench --pin P,S,C` binds
  the publisher, the subscriber and the conductor to those CPUs on Linux,
  `just bench --repeat N` runs every benchmark N times and reports the median run by
  p99.99 with every run's on record, the warm-up is now one rotation through all
  three terms so that no measured message first-touches a page, and the files go under
  `/dev/shm` on Linux. `docs/how-to/set-up-a-bench-host.md` is the page (ADR 0016).
- `just bench-compare` fails the gate on the `claim_commit` and `one_way` rows only;
  the `timer` and `slot_padding` rows, which run no Cavalry code and sit within one
  10 ns clock step of their baseline, are printed as `reported` (ADR 0017).
- `spec/command-ring.md` specifies the command ring at version 0: the many-to-one ring
  through which every client sends the driver its commands, with Cavalry's magic-last
  header, a 128-byte slot per trailer field, the claim, commit, abort, read, unblock and
  heartbeat protocols, and the seven checks a reader runs in order. Three counters,
  `command_ring.unblocked`, `command_ring.rejected_tail` and `command_ring.rejected_record`,
  count the driver's unblocks and refusals. Its vectors come from
  `spec/vectors/command-ring.py`, and the book includes the page (ADR 0014).
- `spec/broadcast-buffer.md` specifies the broadcast buffer at version 0: the one-to-many
  ring through which the driver sends every client its events, lossy by design and so
  control plane only; a receiver that falls a ring behind is told `lapped`. Its vectors
  come from `spec/vectors/broadcast-buffer.py`, and the book includes the page (ADR 0014).
- `cavalry-shm` builds the command ring and the broadcast buffer. `CommandRing::create`
  and `open` run the checks of `spec/command-ring.md` in order; a `Producer` takes
  correlation ids and claims a record in place with one compare-and-swap, or writes one by
  copy, and a `Claim` commits with one release store or aborts, on drop too; the driver's
  `Consumer`, one per ring and only from the ring `create` returned, reads in place,
  clears what it read, unblocks a claim a dead client left once the head has stood still
  for the unblock timeout it was given, stores the heartbeat and closes, and stops the ring by name at a corrupt record or tail.
  `BroadcastBuffer::create` and `open` do the same for `spec/broadcast-buffer.md`; the
  driver's `Transmitter`, one per buffer and only from the buffer `create` returned, never
  waits for a receiver, and a `Receiver` copies each event
  out, validates the copy against the tail intent, and reports `lapped` rather than a torn
  record. The 56 and 54 vectors run on every `just test`, six loom models check the claim,
  read, unblock, transmit and receive protocols, and the layout layers run under Miri
  (ADR 0014).
- `just tidy` retires branches whose work is on master, locally and on origin: every
  branch whose pull request merged, or the one named, with its worktree when the
  worktree is clean. `CLAUDE.md`'s working rules name it as the step after a merge.
- The command ring is version 1: every contended change to a record's word is a
  compare-and-swap from the value its writer last loaded, the claim from 0, the commit
  and the abort from the claim word, the driver's padding from the word it unblocked,
  so a producer whose claim the driver unblocked gets `unblocked` instead of leaving a
  command nobody sent behind the head, and a padding never covers a record committed in
  the same instant; the claim's message write checks the claim first. `Claim::commit`,
  `Claim::abort` and `Claim::body` return a result (ADR 0018, GitHub issue 8).
- CI's two `gates` jobs run `just conformance`, `just loom` and `just miri` after
  `just test`, on Linux and macOS, with the nightly toolchain and Miri installed by the
  pinned toolchain action; a pull request now proves the three cavalry-shm gates
  instead of claiming them (GitHub issue 9).
- The client protocol is specified: `spec/client-protocol.md` version 0 is Aeron's
  control protocol as command ring and broadcast buffer records, with the driver
  directory, the seven commands and eight events of IPC publish/subscribe, their
  bounds and rejections, the timeouts, the file sizes and the registries' capacities,
  and 36 vectors; five counters, `client.timed_out`, `client_protocol.rejected_command`,
  `client_protocol.errors`, `driver.registry_full` and `driver.log_create_failed`
  (ADR 0019).
- `cavalry-proto` reads and writes the client protocol: `client_protocol::Command`
  and `client_protocol::Event` decode a command ring or broadcast buffer record's
  message with the spec's checks in order and write one in place; every vector of
  `spec/vectors/client-protocol.json` runs on every `just test` and under
  `just conformance`, and random bytes under every type id never panic.
- `cavalry-driver` runs: `Driver::start` keeps a directory as `spec/client-protocol.md`
  says, refusing one whose driver is alive and remaking one whose driver is gone, creates
  the counters file, the command ring and the broadcast buffer, and runs the conductor on
  its own thread with a busy-spin, yield or backoff idle strategy. The conductor answers
  every command of the protocol, shares concurrent publications, joins images at the
  producer's position, keeps each publication's positions, limit, cleaning, unblocking
  and closing per `spec/log-buffer.md`, times clients out, and counts every refusal;
  `Driver::close` leaves −1 in the heartbeat for `doctor`.
- `cavalry-control` explains a driver: `doctor::examine` opens a driver directory's
  counters file and command ring read-only and reports whether the driver is running,
  closed or stopped, the command ring's and the counters file's fill, every static
  counter whose `doctor` text applies, and on Linux the host's huge pages, governor,
  isolated CPUs and `/dev/shm`; every finding names the docs anchor that explains it.
- The `cavalry` binary runs: `cavalry run` starts a driver on the default directory
  and stops it on Ctrl-C or SIGTERM, leaving the directory for doctor; `cavalry doctor`
  prints the report and exits 0 when no check failed; `just doctor` runs it; no command
  prints the usage and exits 2. `cargo add cavalry` gives `cavalry::Driver` and, behind
  the `cli` feature, `cavalry::doctor`. The how-to is `docs/how-to/run-the-driver.md`.
- The driver records the conductor's idle strategy in a new static counter,
  `conductor.idle_strategy` (0 backoff, 1 yield, 2 busy-spin), and `IdleStrategy` has
  a name, `backoff`, `yield` or `busy-spin`, and parses one, so that `doctor` can say
  which strategy a running driver uses from the counters file alone.
- Configuration keys exist, two of them: `dir` (`CAVALRY_DIR`, `--dir` on `cavalry run`
  and `cavalry doctor`) and `conductor.idle.strategy` (`CAVALRY_CONDUCTOR_IDLE_STRATEGY`,
  `backoff`, `yield` or `busy-spin`). Every key lives in `config.toml` with its
  `why_not_default` and its `doctor` check; `docs/reference/config.md` and the key table
  in `cavalry-control` are generated from it, and `just lint-config`, now in `just lint`,
  fails a key without them. `doctor` warns when the driver directory is not on a
  filesystem in memory and when the conductor busy-spins with no isolated CPU, and notes
  isolated CPUs a backoff conductor is not using. A value a key does not take stops the
  command before anything starts, with what to do (ADR 0020).
- `cavalry-shm` gains `spsc::queue`, a bounded lock-free queue between two threads of
  one process with one producer and one consumer, for the client's conductor to hand
  images to the user's thread; `offer` returns the item when the queue is full, and the
  protocol has a `loom` model and runs under Miri.
- `cavalry-shm`'s `CountersReader::value` gives a reader a handle on one counter's
  value, resolved once, so a publisher reads its limit with one acquire load per claim
  and no check in its hot loop.
- `cavalry-client` attaches: `Cavalry::connect()` finds the driver through `CAVALRY_DIR`
  or the default directory, `connect_to(dir)` names one, and the client conductor thread
  keeps the client alive with a keepalive every 500 ms, closes it when the driver closes
  or its heartbeat goes stale, when the driver times the client out, when the
  broadcast buffer laps it, and when an event fails a decoding check or the command ring
  or broadcast buffer fails a check of its page (`corrupt`), and `closed()` says which; `close()` tells the driver. A
  directory with no driver, or a driver that is gone, is refused with what to do
  (ADR 0021).
- `cavalry-shm`'s `CountersWriter` opens the counters file writable for a client, so a
  subscriber stores its `subscription.position` through the owner's `Counter` handle
  without being able to allocate or free a record; `spsc` halves report
  `peer_dropped()`, so a conductor sweeps a registration whose subscription is gone.
- `cavalry-client` publishes: `add_publication` and `add_exclusive_publication` wait for
  the driver's answer up to the response timeout and return a `Publication`, whose
  `try_claim` writes a frame in place and `offer` copies a message in, each against the
  limit the driver maintains with one acquire load; the refusals are `Offer`, Aeron's
  codes as a result (`not-connected`, `back-pressured`, `admin-action`,
  `max-position-exceeded`, `closed`, `too-long`); `close` sends `remove publication`.
  After the client closes, `is_closed` is true and a refusal is `closed`. A command the
  driver refuses is `ClientError::Refused` with the error code and message (ADR 0021).
- `cavalry-client` subscribes: `add_subscription` returns a `Subscription` whose `poll`
  hands each fragment to a handler as a view into the log, across one `Image` per
  connected publication in turn, storing each image's `subscription.position` with a
  release store after its share; images join and go as the driver announces them,
  opened and unmapped by the conductor, never by the poll; a frame that fails a check
  retires its image and is reported as a `Fault`, and a poll after the client closes is
  `Fault::Closed`. A subscription the client's conductor cannot hold is
  `ClientError::SubscriptionsFull` and is removed again. The README example is now the
  ten-minute path's program: publish and subscribe in one process (ADR 0021).
- The driver's conductor copies each command into scratch sized once at start, so its
  command read allocates nothing, keepalives included; a record longer than any command
  of this version is not copied past that bound, and is refused by the spec's checks in
  their order on the copy: `type` with code 6, `string` or `length` with code 7.
- `just bench` gains the client rows, `claim_commit_32_client` and `one_way_32_client`:
  publish to delivery through `cavalry-client` and a real driver in the harness's
  process, with the zero-allocation assertion over that path too, so the gate holds the
  path a user takes to the budget. The client rows are in the bench host's baseline,
  so a p99.99 regression over 5% on the client path fails CI. A client subscriber that fails or stops reading
  fails the run with its reason instead of leaving the publisher retrying.
- The ten-minute path exists: `cargo add cavalry` gives `cavalry::Cavalry`,
  `Publication` and `Subscription`, the README's five commands build the binary and the
  example, start a driver, publish 1,000 messages and read them back, read doctor and
  stop the driver, and `just quickstart` runs them end to end in CI on Linux and macOS;
  `docs/tutorials/publish-and-subscribe.md` is the tutorial (ADR 0007's acceptance test).
- ADR 0021, the client API shaped after Aeron's with a conductor thread and no lock, is
  Accepted on `master`'s evidence: the README doctest, fourteen end-to-end tests against
  a real driver, zero allocations per message on the client rows, `just quickstart` green
  on both runners, and the public API within 10 to 40 ns of the raw log on the bench
  host. Its text now says what landed: `poll` returns a count or a `Fault`, the handler
  receives the data frame header, a full image queue closes the client with `overrun`
  and is not counted until clients have counters, and the image queue holds 64 events.
- ADR 0022: the driver loads a publication's producer position at most once per
  millisecond, by `spec/log-buffer.md`, so the publisher's cache line is no longer read
  on every duty cycle; `publication.position` is at most a millisecond behind the
  publisher, and closing and a subscription joining still load it afresh, for the end
  of stream position and the join position. On the bench host the conductor's loads
  were the exclusive row's p99.99 band and a third of the publisher's p50; the driver,
  the harness and the baseline follow in their own commits.
- The driver's conductor loads a publication's producer position at most once per
  millisecond, as ADR 0022 and `spec/log-buffer.md` say, and a move of it is no longer
  work for the idle strategy, so a backoff conductor parks while only the publisher
  moves; closing and a subscription's join still load it afresh. `publication.position`
  says it is at most a millisecond behind the publisher.
- `just bench`'s conductor thread loads the publisher's producer position about once per
  millisecond, the driver's cadence from ADR 0022, counted in spins rather than read
  from the clock, so the raw rows measure the log buffer and not the harness's loads of
  the publisher's cache line. On the bench host the raw `claim_commit` rows fall by
  about half at p99.99 and the raw `one_way` rows by about a tenth, in every process
  measured; reading the clock on every spin of the conductor instead cost the raw rows
  about 100 ns at p99.99 on that host. The host's baseline is re-recorded from
  this change's run in a commit of its own, a person's decision.
- The bench host's baseline is re-recorded from the harness at ADR 0022's cadence, both
  profiles: at p99.99 `claim_commit_32_exclusive` 380 to 220 ns with its band of 21 runs
  at 210 to 250 instead of 270 to 430 between processes, `one_way_32_exclusive` 660 to
  600, and the client rows 460 to 280 and 670 to 600. The gate still compares a
  result's median with the baseline's slowest run, now as sharp on that row as on the
  others, which closes ADR 0016's Revisit; the how-to says why the median rule ADR 0022
  named is not taken.
- ADR 0023: the second slice is the durable log and replay on one host, in four
  features (cards 92 to 95): the on-disk format specified first and accepted by a person,
  recording in the driver, replay with `cavalry replay` and `just replay-check`, then
  retention and the ten-minute path. UDP is the third slice, once a dedicated second
  bench host exists (card 91).
- ADR 0024, Proposed: the durable log's on-disk format, a fixed-slot catalog whose slot
  index is the recording id, never reused, with a state written last, one CRC32 over the
  fields written once and another over the stop fields, and a recovery rule for a crash
  that rebuilds only the stop fields, stops an empty recording at its start and never
  re-signs damage; Aeron's segment files of raw term bytes with no header, aligned as
  Aeron aligns them; and a time index of one entry per
  millisecond for `cavalry replay --from <instant>`. The spec page and its vectors land
  with the acceptance, which is a person's, since the format cannot be undone.
- ADR 0024 is Accepted and `spec/durable-log.md` version 0 is the durable log's on-disk
  format, with its vectors; `cavalry-log` gains the layout types, a catalog writer that
  creates and stops recordings in the page's order and a reader that retries a stop in
  progress, the conformance test under `just conformance`, a no-panic test over generated
  input, and its first README example. A state-2 slot reads as `invalid`, a missing
  middle segment is damage, and recovery's actions get counters in the recorder's feature.
  The catalog writer writes the header's magic last and writes again a header torn while
  it was created, a descriptor reports its first fault in one order (state, `invalid`,
  `checksum`, then the fields), and `CatalogWriter::rewrite` refuses a slot it does not
  hold, an empty or invalid one, or another recording's descriptor. A descriptor's stop
  position is −1 exactly when its stop timestamp is −1, and a reader rejects a mix of the
  two as `position`. The writer's only state writes are a delete of a stopped recording
  and an invalidation of a damaged one, and a stop is written once; any other transition
  is refused as `transition` with nothing written, and opening the catalog truncates the
  bytes a crash left past its last whole slot.
- ADR 0025, Proposed: the driver records a stream to the durable log. `start recording`
  and `stop recording` join the client protocol as version 1 with `recording ready` and
  `recording stopped`, under type ids from ranges of Cavalry's own that keep Aeron's
  numbers free for Aeron's features; a recorder thread writes committed frames in blocks
  straight from the log buffer into segment files, holds a `recording.position` the publisher's limit
  respects so that a slow disk back-pressures and never loses, and syncs once per second
  behind a `recording.synced_position`; the log directory is the key `log.dir`, with an
  absolute per-user default and an advisory lock on the catalog that moves the Rust floor
  to 1.89; a recording belongs to the driver and outlives the client that started it.
- `cavalry-shm`: `TermReader::block` scans a block of committed frames with their
  headers, up to a limit, as Aeron's `TermBlockScanner` does, taking a padding frame as
  a block of its own with its header alone, so that the recorder writes each block from
  the log buffer into a segment file with one positional write (ADR 0025); a loom model
  checks the one read the scan adds, of the frame word's own bytes after its acquire.
- `cavalry-log`: `dir::LogDir` opens a log directory under an exclusive advisory lock on
  the catalog, through the standard library's `File::try_lock`, so a second driver on
  the same directory is refused with `locked` and a crash never leaves a stale lock, and
  runs the recovery pass of `spec/durable-log.md` before any recording is added: a torn
  catalog is truncated and an empty last slot dropped, an empty slot before the last or a
  creation checksum that fails is set to state 2, a recording live at a crash or with a
  torn stop gets its stop fields from its files with its time index repaired, segments
  wholly past the stop are removed, and a deleted recording's files are removed again;
  every action is reported for the driver to count. The workspace's Rust floor moves
  from 1.85 to 1.89 for the lock (ADR 0025).
- `cavalry-log`: the recorder's side of a recording. `LogDir::start` takes the catalog
  slot in the page's order and creates the time index with its start entry and the
  first segment file at its full length, synced; `recorder::RecordingWriter::write`
  writes one block of committed frames with one positional write, a padding block as its
  header alone, appends a time index entry at most once per millisecond with timestamps
  that never go back, and rolls into the next segment file at the segment's end, syncing
  the one it closes; `sync` syncs the segment and the index once per second while bytes
  are unsynced and moves `synced_position` up to `position`; `LogDir::stop` writes the
  stop entry, syncs, removes a segment wholly past the stop and then writes the
  catalog's stop fields, so a crash between the two leaves a live slot recovery repairs.
  A block that is empty, unaligned, longer in bytes than in length or that would cross
  its segment is refused with nothing moved (ADR 0025). A roll that fails, a full disk
  at the next segment, is `io` and is retried at the next write; on any error, the block
  was taken exactly when `position()` moved, so the caller compares it before and after.
  A start that fails after its slot is written stops the slot at its start, as recovery
  would. The README
  example is this path, from open to read-back.
- A weekly release-notes email (ADR 0026): the `send release notes` workflow runs on
  Mondays, covers the commits on `master` since the last email, has a writer agent draft
  what a reader can now do and what was decided, refuses a draft that names a recipe,
  command, key, counter, ADR, path or link that does not exist in the checkout, has a
  reviewer agent read it against the sources, and sends only an approved draft, `bcc`, to
  the list in the secret `RELEASE_NOTES_RECIPIENTS`. A run by hand is a preview to one
  address; `draft_run` sends what a preview approved. `docs/how-to/send-the-release-notes.md`
  is the page, and `just test-scripts`, which `just test` now runs first, tests the scripts.
- Two counters for the counters file's own transitions: `counters.allocations` adds one
  for every record allocated, the static counters' own records included, and
  `counters.freed` for every record freed, so that the `counters.allocated` gauge's moves
  are counted and allocations minus freed is the gauge, as invariant 4 asks of every state
  transition (`spec/counters-file.md`, Metadata record).
- `spec/client-protocol.md` version 1: `start recording` (type id `0x0101`) and `stop
  recording` (`0x0102`), the first commands of Cavalry's own, in a type id range beside
  Aeron's; `recording ready` (`0x1001`) with the recording id, the session id and the two
  position counter ids, and `recording stopped` (`0x1002`) with the stop position, sent to
  every client; error codes 20 (no publication to record), 21 (unknown recording) and 22
  (already recording); a `recordings` registry of 1024; 22 new vectors, among them one
  reserved id per range the page names. A version 0 client closes on the first recording
  event, so an upgrade restarts the clients before the driver (ADR 0025, accepted).
- The driver records a stream: a second thread, the recorder, with the conductor's idle
  strategy, takes blocks of committed frames from the publication's log buffer and writes
  them into the log directory with one positional write each, syncing once per second;
  the conductor never touches the disk and hands the recorder its starts and stops
  through two bounded queues. `recording.position` bounds the publisher's limit and the
  cleaning, so a slow disk back-pressures the publisher and never loses a byte; a
  recording alone connects its publication; a closed publication's log goes only once
  its recording has stopped; a start the counters file refuses after the recorder took
  the slot is answered with code 10 and stopped, and the limit holds at the start
  position until that stop is written; a `start recording` while stops and attaches wait
  for room in the recorder's queue is answered with code 10. `Driver::start` takes the log directory, opened under a
  lock and recovered at start, with `Driver::default_log_dir()` as the rule until the
  `log.dir` key arrives; counters `recording.started`, `recording.stopped`,
  `recording.write_failed`, `recording.start_failed`, `recording.catalog_full`, the six
  recovery counters, and the gauges `recording.position` and `recording.synced_position`
  per recording (ADR 0025).
- `cavalry-log`: `CatalogWriter::delete` leaves until card 95 brings the purge with its
  counter and its caller in the driver, so that every catalog transition on `master` is
  one the driver performs and counts (CLAUDE.md, invariant 4); the catalog's truncation
  of a torn tail at open is now synced.
- `just lint-counters` checks the state machines of the data plane: a `[[state_machine]]`
  in `counters.toml` names its states, every `[[transition]]` names the counter of kind
  `counter` it adds one to, whose generated constant some crate's code names, and the
  enum marked `// state machine: <name>` has those states as its variants; the counters
  reference page lists each machine with its transitions. It fails closed: an enum named
  `*State`, `*Status` or `*Phase` with neither that mark nor `// not a state machine:
  <why>` fails it. Three machines register with it: the counters file's metadata record
  (`counters.allocations`, `counters.freed`), the recording catalog's descriptor
  (a slot taken in `recording.started`, `recording.start_failed` or
  `driver.registry_full`, a slot set to invalid in `recording.invalidated`) and the
  driver's view of a recording
  (`recording.started`, `recording.stopped`, the failure counters); the two ring record
  states are exempt with their reasons. This is the gate CLAUDE.md names for a transition
  with no counter (ADR 0024, ADR 0025, card 93).
- The `log.dir` configuration key: the log directory, `--log-dir` on `run` and `doctor`
  or `CAVALRY_LOG_DIR`, defaulting to `$XDG_DATA_HOME/cavalry/log`, else
  `~/.local/share/cavalry/log`, and `/var/lib/cavalry/log` as root; with neither a home
  nor root `cavalry run` refuses to start and names the key. Its doctor check,
  `log-directory-backing`, warns on a directory in memory, on an overlay or network
  filesystem, not writable by this user (the directory's mode bits, before any driver
  has used it, and the catalog once there is one), or under 1 GiB free (ADR 0025).
- `cavalry record <stream id>` records a stream's live publication to the durable log and
  prints the recording id; `cavalry record --stop <id>` stops it. `Cavalry::start_recording`
  and `Cavalry::stop_recording` are the same in the client API, with a `Recording` handle
  whose two positions read from the counters file and whose `stop` stops it; dropping it
  stops nothing. `cavalry
  doctor` lists every recording's position and synced position, and adds
  `recording-stalled` (a recorder a full window behind its publisher),
  `recording-write-failed` and `recording-invalid`, which names the catalog slots recovery
  could not repair when run with `--log-dir`. The how-to page is
  `docs/how-to/record-a-stream.md` (ADR 0025).
- Bench rows with a recording on: `claim_commit_<body>_recorded` and
  `one_way_<body>_recorded` repeat the client rows while the driver's recorder writes the
  stream to the durable log under the bench directory, and the run fails unless the
  recording reached the publisher's final position; new rows, reported until the bench
  host records their baseline (ADR 0025, ADR 0023's measurement with recording on).
  The rows found the one allocation the recorder made after a start, at a segment
  roll, and the roll now formats the next file's name on the stack into a path the
  writer keeps, so the bench's zero-allocation gate holds with a recording on.
- The writer and the reviewer of the pull-request workflows run on Fable 5.1 at xhigh
  effort, in `claude.yml`, `review-pull-request.yml` and their briefs' frontmatter, so that
  the agents that gate and write this repository have the highest reasoning available. The
  release-notes agents stay on Opus 5.5: the account's Fable usage limit was reached inside
  a preview run and nothing was sent, and a weekly email that fails at a limit is worse than
  one reviewed by Opus.
- The second bench host, `kip` (card 91), a Minisforum MS-01 with an Intel Core
  i9-13900H, tuned on three P-cores, joined to `bench` by a direct cable with hardware
  timestamps at both ends and chrony across it. `bench/host/setup-ubuntu.sh` now turns
  turbo off through intel_pstate's `no_turbo` and limits Intel's idle driver to C1 with
  `intel_idle.max_cstate=1`, both inert on AMD; `bench/host/check.sh` prints the turbo
  switch and the idle states and fails when turbo is on or a state deeper than C1 is
  enabled; the how-to covers hybrid parts, a second host's runner label and the link.
- Every shared mapping is pre-touched when it is made (ADR 0027): the log buffers, the
  counters file, the command ring and the broadcast buffer are made resident in `create`
  and `open`, in the driver, every client, the recorder, `doctor` and the bench, so the
  data plane never takes a page fault; on Linux the kernel's populate advice does it in
  one call. Found on the second bench host, where the first touch of every page of every
  term faulted inside the measured loops and the threads woke each other across CPUs
  (card 136). A driver directory that cannot hold a log now fails at creation with an
  error that names `--dir` and the `driver-directory-backing` doctor check, instead of a
  bus error in a publisher later; and a log buffer's memory is allocated when the
  publication or subscription is added, not as the stream reaches it, so a host holds as
  many live publications as its driver directory holds whole logs, 192 MiB each for the
  spec's three 64 MiB terms, and the next `add_publication` is refused by name.
- The bench's third pin holds the driver's conductor alone: under `--pin P,S,C` the
  driver's recorder and the client's conductor run on the lowest CPU outside the pins,
  through `Driver::start_with_threads`, a hook each driver thread runs first, which
  `cavalry` re-exports with its `DriverThread` and `ThreadStart`, and every histogram's
  counts are written once before the measured loop, so a rare sample never first-touches
  a page of them. Both came from the second bench host's trace (card 136). A pin a
  driver thread could not take, or a process with no CPU outside the pins, fails the run
  with the next step in the message; `docs/how-to/run-the-bench.md`, "When it does not
  work", is the page.
- A reviewer session that ends without submitting a review is run again once by the
  `review pr` workflow itself, with a comment on the pull request saying so; the second
  miss fails the run. Every run is named with its pull request number and attempt, and
  its job summary carries the session's result, the tool calls the settings denied and,
  on a miss, the session's final text. A push by the agent to a pull request branch now
  gets its review without a person: GitHub raises no `dispatch review` run for it, so
  the `claude` workflow adds the `review` label after the push. `docs/how-to/ask-the-agent.md`,
  "When it does not work", says what to do when a review is fifteen minutes late (card 139).
- The bench harness stamps every sample with the time stamp counter, calibrated once
  against the monotonic clock through `quanta` (ADR 0028), instead of the OS clock
  through the vDSO, which the timekeeper's tick rewrites a thousand times a second and
  which was the harness's own tail: 280 ns at p99.99 on the bench host and 1.4 µs on the
  second, in clusters spaced exactly 1 ms apart. `one_way` p99.99 reads 330 ns on `bench`
  and 350 ns on `kip` with the counter, against 610 and 1,650 with the OS clock; every
  baseline moves down and is re-recorded by a person (card 138, card 109).
- The latency gate fails a row only when its p99.99 is more than 5 percent and more than
  one step of the harness's clock above the baseline's slowest run (ADR 0029): the
  harness measures its clock's step at start, prints it in nanoseconds and in the
  counter's ticks, and writes it into the result as `clock_step_ns`; `bench-compare`
  prints `within a step` for a row the clock cannot tell from an unchanged commit, 10 ns
  of a 160 ns row on the bench host, and `clock step unknown` for a result from before
  the field, which the 5 percent rule alone judges. The rule has its tests,
  `scripts/bench-compare-test.py`, under `just test-scripts` (card 143).
- The latency gate's floor is the smaller of the result's clock step and the baseline's:
  `--update` writes the step into the baseline's entry, the two bench hosts' entries
  carry it (10 ns on `bench`, 1 ns on `kip`), and `bench-compare` says which step it took
  when the two differ, so a change to the harness that over-read its step cannot widen
  the gate by itself. A result without the step is told so on the failure line too, in
  place of a step of 0 ns. `cargo test -p cavalry-bench`, now under `just test`, holds
  the step reading over the two hosts' advance histograms (ADR 0029, card 146).
- `TermReader::poll`, the subscriber's per-message loop in `cavalry-shm`, is always
  inlined into its caller. Whether the compiler inlined it was the whole of a 60 ns move
  at p99.99, 20 ns at p50, of the bench's `one_way` rows on the bench host, flipped by an
  unrelated change to the harness's code size and blamed on a 16 KiB heap allocation;
  the rows no longer move with the harness's code, and the bench how-to says what to
  check when they move with a change outside the data plane (card 144).
- `/pr`, a Claude Code skill for a session that lands a card through a pull request: the
  four gates one command at a time with each exit code read, CI's `lint-docs` range run
  before the push, the review record as the pull request's body with the `auto-merge`
  label, then the review loop from the author's side, with the wait each stage needs and
  the one manual step each stall needs, and `just tidy` after the merge. `/commit` now
  says to run each gate as its own command. `docs/how-to/ask-the-agent.md` gains the
  rule that cost pull request 57 two fix runs: push nothing while a fix run is working,
  one writer per cycle.
- `cavalry doctor --log-dir` warns when the log directory's path is over 337 bytes, the
  most under which a segment roll opens its file on the standard library's stack buffer
  instead of allocating on the recorder thread; the bound is
  `cavalry_log::recorder::LOG_DIR_PATH_LIMIT`, the log crate's README and the `log.dir`
  key state it exactly, and the record how-to says what to do (card 108).
- The bench host's two baselines, `bench-linux` and `bench-linux-unchecked`, are
  re-recorded from master's run at 96fb91a (CI run 40787078643): the unchecked
  `claim_commit_32_recorded` row's slowest run rises from 300 to 320 ns, where master
  itself had read 300 to 310 against it, and eight rows tighten, `claim_commit_32_concurrent`
  and `_exclusive` from 170 to 150 and the `one_way_32_concurrent` and `_exclusive` rows
  from 300 to 230 and 210 unchecked, the gain card 144 made
  ([Run the bench](docs/how-to/run-the-bench.md), step 4).
- Replay: the client protocol is at version 2 with `start replay` and `stop replay`,
  `replay ready` and `replay stopped`, and error code 23, position out of range; the
  driver plays a recording from a position or its start, for a length, to its extent
  or following it live, into a replay publication a subscription on the replay stream
  reads as any stream, on a third driver thread, the replayer, that plays only what the
  recorder has synced and no faster than the slowest subscriber, plays whole frames, so
  a length that ends inside a frame ends at that frame's end, and parks while it has
  nothing to play; counters
  `replay.started`, `replay.stopped`, `replay.ended`, `replay.not_connected`,
  `replay.failed`, `replay.start_failed` and the gauge `replay.position`
  ([ADR 0030](docs/decisions/0030-replay-a-recording-into-a-subscription.md),
  [the client protocol](docs/reference/client-protocol.md)). The client API, the
  `cavalry replay` command and `just replay-check` follow.
- `Cavalry::start_replay` and `stop_replay` with the `Replay` handle, whose `ended()`
  says where and why a replay ended; `Image::position_of` and `Image::positions`, a
  frame's position in the recording from its header, which a padding frame moves
  without a handler seeing it; `cavalry::index::position_at` and `position_after`, an
  instant to a position through the time index; and `cavalry replay --recording <id>
  --from <position or instant> [--to ...] [--stream <id>] [--follow]`, which prints
  one line per message, the position from the frame's header, and exits 1 naming the
  positions it did not print when the replay's log went before it had read to the end
  ([Replay a recording](docs/how-to/replay-a-recording.md); ADR 0030). The client
  follows the end of every replay it started while the handle lives, as many as the
  driver allows live at once; one more is `ClientError::ReplaysFull` and stopped again.
- `just replay-check` is built, the gate CLAUDE.md names for invariant 1: a driver on a
  directory of its own records 100,000 hash-chained messages, with a padding frame a
  quarter of the way, while a live subscriber prints them, `cavalry replay` plays the
  recording twice from the start and once from an instant, and the three outputs must
  hash the same and the replay from the instant start where the time index says; CI
  runs it on Linux and macOS. With it, the second slice's third feature is done
  (ADR 0023, ADR 0030).
- A baseline profile records the date its numbers were measured: `bench-compare.py
  --update` writes the result's `date_unix` as a UTC date-time under `date`, where it
  read a field the harness never wrote and left `null`; the four profiles carry their
  runs' dates (card 158; [Run the bench](docs/how-to/run-the-bench.md), step 4).
- The bench harness's conductor loads the producer position once per millisecond by
  the clock, which it reads every 256 spins, instead of every 10,000 spins, which was
  a millisecond on the bench host and half of one on kip; the loads per run are
  printed and written to the result as `producer_position_loads`, so each host's
  cadence is on record. `cavalry-bench --only NAME` runs only the benchmarks with a row
  whose name contains NAME, for a profile of one row, under the profile with `-only`
  appended; a NAME no row contains is an error that lists the rows. The result carries
  the allocation count as `allocations` beside the ratio, which `bench-compare` judges
  when no message row ran, and `bench-compare` picks the newest result by the time in
  its name, not by name (card 161;
  [bench/README.md](bench/README.md)).
- CI runs the latency gate on the second bench host too: the `bench (kip)` job in
  `.github/workflows/ci.yml` runs on `kip` (`[self-hosted, kip-linux]`, pinned to CPUs
  6,8,10) against the `kip-linux` and `kip-linux-unchecked` baselines, on every push and
  on pull requests from this repository, beside `bench (linux)`
  ([Set up a bench host](docs/how-to/set-up-a-bench-host.md), step 6).
- The ten-minute path records and replays: the quickstart program starts a recording of
  its stream before it publishes and stops it after, and `cavalry replay --recording 0
  --from 0` plays the 1,000 messages back from the durable log as the path's fourth
  command, which `just quickstart` checks on Linux and macOS: the second slice's
  acceptance test (card 95, ADR 0023;
  [the tutorial](docs/tutorials/publish-and-subscribe.md)).
- The driver loads a publication's producer position at most once every two
  milliseconds, where it was one: at ten million claims a second the transfer the
  publisher's next store paid after each load landed on one claim in ten thousand, the
  gate's percentile, so `claim_commit_32_exclusive` on kip read 107 or 156 ns by the
  process; now one in twenty thousand, and the first runs at two milliseconds read 119
  to 152 on kip inside one process, within its spread. The harness follows the same
  constant; `publication.position` is at most two milliseconds behind the publisher;
  neither bench host's baseline moves
  ([ADR 0032](docs/decisions/0032-load-the-producer-position-every-two-milliseconds.md)).
- A bench host's baseline is recorded from every process the host has run at the
  baseline's tree, three at least, and the gate compares a row's median with the
  slowest run over all of them: `scripts/bench-compare.py --update` takes the result
  files and writes `processes: N`, `--join` adds a later process of the tree that read
  a row above the baseline, and the comparison's first line says how many. `kip`'s two
  baselines are re-recorded that way at the tree of 4a09b6e, eight and five processes,
  where master's own runs had failed against one process's band: the exclusive claim
  row's slowest run moves from 132 to 162 ns and the concurrent row's from 154 to 169
  (card 168 owns the spread), the unchecked `_recorded` row's from 234 to 257 (card
  169), and four rows tighten. Both bench jobs run the unchecked profile after a failed
  checked comparison, so every run yields both profiles' results
  ([ADR 0033](docs/decisions/0033-record-a-bench-baseline-from-every-process-at-one-tree.md)).
- ADR 0033 is Accepted on three consecutive green `bench (kip)` runs at the tree of
  4a09b6e with no process joining, and its text says what a joining process is not: a
  run whose row moved as a whole, p50 and p99 with p99.99 and the rate down against the
  tree's other processes, is a disturbed host, joins no baseline and is re-run; the
  bench how-to's troubleshooting list reads it that way
  ([ADR 0033](docs/decisions/0033-record-a-bench-baseline-from-every-process-at-one-tree.md)).
- `bench`'s two baselines are re-recorded from every process the host has run at the
  tree of 4a09b6e, twelve, as ADR 0033 defines a baseline, so both hosts' baselines now
  hold a band; the one process the host's `sysstat-collect` timer disturbed on
  2026-10-30 joins nothing (card 170). Every process's medians sit where the
  one-process baselines of 96fb91a were, and what the entries gain is the band's edge:
  the compared value, the slowest run over the processes, moves on eight gated rows by
  more than 5 percent, the checked concurrent claim row 150 to 169, exclusive 160 to
  170 and `_recorded` 320 to 339, the unchecked concurrent 150 to 180, exclusive 150 to
  180, `_recorded` 320 to 350, one-way client 300 to 340 and one-way exclusive 210 to
  240 (card 168 owns the claim rows' spread); four rows move by one 10 ns step, the two
  one-way `_recorded` rows by two, and the two client claim rows hold
  ([ADR 0033](docs/decisions/0033-record-a-bench-baseline-from-every-process-at-one-tree.md)).
- ADR 0031, Proposed: a stopped recording is truncated to a position, purged of the
  whole segments before a position, or deleted, with Aeron Archive's three commands,
  `truncate recording`, `purge segments` and `purge recording`, as the client protocol's
  version 3, answered with `operation success` or a new error, `recording busy`; the
  catalog moves to version 1 for a retained position in the descriptor, which a purge
  moves forward and a replay's −1 starts at, read beside version 0's slots with no
  migration pass, a slot moving to the new form at its first purge, and a truncate
  reopens the stop to −1 while it cuts the files so that a crash at any point is the
  torn stop recovery already rebuilds; the checks that read the disk and the file work
  run on the replayer thread, the checks before any catalog write, so a refused command
  writes nothing; there is no `log.retain` key, since no default
  can be right, so retention is the operator's command or script and never a schedule
  the user did not set
  ([ADR 0031](docs/decisions/0031-truncate-purge-and-delete-a-stopped-recording.md)).
- The latency gate's second host is `grip`, a Dell with an Intel Core i7-10700 tuned on
  CPUs 4 to 7, as the `bench (grip)` job in `.github/workflows/ci.yml`
  (`[self-hosted, grip-linux]`, pinned to CPUs 4,5,6, profiles `grip-linux` and
  `grip-linux-unchecked`), and `kip` becomes the soak host (card 159): pinned at
  stock, grip's exclusive claim row held its p99.99 median within 2 ns over three
  processes where kip's moved 46 ns over eight
  ([ADR 0034](docs/decisions/0034-run-the-gate-on-grip-and-the-soak-on-kip.md),
  Proposed; `bench (kip)` runs beside grip's job until grip's has been green over a
  handful of pushes). `bench/host/setup-ubuntu.sh` now stops and masks the twelve
  timers a stock Ubuntu Server starts jobs from, `sysstat-collect` every ten minutes
  among them, and `bench/host/check.sh` prints how many timers are active and fails
  when one of the twelve is; applied on `grip` at its set-up and on `bench` and `kip`
  on 2026-10-31 (card 170; [Set up a bench host](docs/how-to/set-up-a-bench-host.md),
  "Updating the host").
- A replay of a recording that stopped at its start position, from that position,
  ends at once: `replay ready`, then `replay stopped` at the position with reason 0,
  and `cavalry replay` prints `0 messages`, where the driver refused it with error 12
  (storage space) because the recorder keeps no segment file for an empty recording.
  The replayer opens no file for a replay with nothing to play
  ([ADR 0035](docs/decisions/0035-end-a-replay-with-nothing-to-play-at-once.md); card
  165, found by `just quickstart` on 2026-10-29).
- `bench/host/setup-ubuntu.sh` installs `cavalry-bench-irqs.service`, which binds every
  device interrupt a driver bound itself to the housekeeping CPUs once the network is
  up, and `bench/host/check.sh` lists those interrupts and fails when one names an
  isolated CPU. On `grip`, Intel's Wi-Fi driver had given each receive queue one CPU,
  the isolated ones included, so the runner's own traffic during a job interrupted the
  publisher's and the subscriber's CPUs and one one_way run in about twenty read 17 to
  64 µs at p99.99; bench's and kip's isolated CPUs took no such interrupts. Applied on
  all three hosts on 2026-10-31 (card 159; [Set up a bench host](docs/how-to/set-up-a-bench-host.md),
  "Updating the host").
- A replay of a live recording ends on the recorder's word: when the recorder reports
  the stop, the conductor hands the replayer the stop position and the replay ends
  there with reason 0 in the replayer's next cycle, where it read the catalog once a
  second and, standing at a segment boundary, could trust a freed and reused counter
  past the stop and fail with reason 3. The replay publications, live or lingering
  after their replay ended, are bounded at 128, twice the replays registry; a `start
  replay` past it is refused with error 10 naming them. `cavalry replay --from start`
  names a recording's start position without knowing it, alone or with `--follow`, and
  is refused with `--to`, whose length needs the position; the client's full replay
  start queue is `ClientError::ReplayQueueFull` (reason `replay-queue-full`, explained
  in the replay how-to), not an error that blamed the driver; and the replays bound has
  one definition, in `cavalry-proto`
  ([ADR 0036](docs/decisions/0036-hand-the-replayer-the-stop-and-bound-the-replay-publications.md);
  card 160).
