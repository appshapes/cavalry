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
- `just lint-counters` checks the state machines of the data plane: a `[[state_machine]]`
  in `counters.toml` names its states, every `[[transition]]` names the counter it adds
  one to, and the enum marked `// state machine: <name>` has those states as its
  variants; the counters reference page lists each machine with its transitions. It
  fails closed: an enum named `*State`, `*Status` or `*Phase` with neither that mark nor
  `// not a state machine: <why>` fails it, and so does a transition whose counter's
  constant no code in the enum's crate names. One machine registers with it: the
  counters file's metadata record, whose allocation and free `counters.allocations` and
  `counters.freed` count; the two ring record states are exempt with their reasons. The
  recording catalog's descriptor fails it until the code that writes its state word
  moves a counter for every transition (ADR 0024). This is the gate CLAUDE.md names for
  a transition with no counter (card 93).
