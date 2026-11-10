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
- `grip`'s first baselines, `grip-linux` and `grip-linux-unchecked`, recorded from
  master's three processes at 0782ec5 (run 41196273792, attempts 3 to 5), the host's
  first with its Wi-Fi interrupts off the isolated CPUs, and joined by a fourth, this
  change's own run, which read the checked client claim row at 133 ns against the
  three's slowest 118 (ADR 0033): the exclusive claim row's slowest run 92 ns at p99.99
  over runs of 73 to 92, the client row's 138, the one_way rows 423 to 720. The
  `bench` host's two re-recorded from the same three runs after the two host changes
  of the day ended its tree (ADR 0033): twelve rows move by a step or less and four
  tighten by more than one, the checked concurrent claim row 169 to 150 ns, the checked
  recorded claim row 339 to 310, the unchecked recorded claim row 350 to 330 and the
  unchecked `one_way_32_client` row 340 to 290, as the twelve-process band gives way to
  three
  ([ADR 0034](docs/decisions/0034-run-the-gate-on-grip-and-the-soak-on-kip.md); cards
  159 and 170).
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
- The harness reads a coarse counter's clock step from its jump: when most pairs of
  consecutive readings show no advance, the counter moves less often than the harness
  reads it, and the step is the smallest frequent advance rather than the divisor of
  the advances, which read 1 ns on an Apple silicon laptop whose clock resolves 41 ns
  (now `clock step 41 ns (41 ticks)` there); the bench hosts' steps are unchanged
  ([ADR 0037](docs/decisions/0037-read-a-coarse-counters-step-from-its-jump.md); card
  154).
- ADR 0031 is Accepted, with the four amendments its review asked for: the client
  protocol moves to version 3 with `truncate recording` (261), `purge segments` (262)
  and `purge recording` (263), error code 24 `recording busy` and a registry of the
  commands in progress (64), and the catalog to version 1, whose descriptor carries a
  retained position under its own checksum, read by a two-range rule so that a slot in
  either form holds and a directory nobody has purged still opens under a version 0
  driver. The spec pages, both vector sets, `cavalry-proto`'s commands and
  `cavalry-log`'s layout follow; `counters.toml` gains `recording.truncated`,
  `recording.segments_purged`, `recording.deleted`, `recording.retention_failed` and
  the `recording.bytes` gauge. The driver decodes the three commands and refuses them
  with error 11 until the change that implements them lands
  ([ADR 0031](docs/decisions/0031-truncate-purge-and-delete-a-stopped-recording.md);
  card 95).
- ADR 0034 is Accepted: `grip` is the latency gate's second host and `kip` the soak
  host. `bench (grip)` is a required check of `master`, `bench (kip)` is gone from CI
  after three green pushes with grip's baselines in place, and `kip`'s baselines stay
  in `bench/baseline.json` as the record of its run as the second host. The three
  hosts' exclusive claim row, from their baselines: grip holds its median within 3 ns
  across processes, bench within one 10 ns step, kip wanders by 46 ns
  ([ADR 0034](docs/decisions/0034-run-the-gate-on-grip-and-the-soak-on-kip.md); cards
  159 and 168).
- The driver truncates, purges and deletes a stopped recording: `truncate recording`
  cuts the segment file and the time index at a position and rewrites the stop, `purge
  segments` moves the retained position and removes the segment files before it, one
  per duty cycle, and `purge recording` removes every file and marks the slot deleted;
  each counts in its counter. A recording that is live, read by a replay or under
  another command is refused with error 24, a slot that is not a valid recording with
  21, a position outside its rule with 23, and a file the driver cannot read, cut or
  remove with 12, after which the slot is repaired from its files; a `start replay` of
  a recording under a command is error 24, counted in `replay.start_failed`. Recovery
  rebuilds a retained group torn by a crash from the files and removes a segment file
  before the retained position, counted in `recording.recovered` and
  `recording.files_removed`. `cavalry doctor --log-dir` gains `log-directory-space`,
  which under a tenth of the log directory's filesystem free names the stopped
  recording a `purge segments` frees the most of and the command, and
  `recording-over-half`, which names a recording over half the filesystem, live by its
  `recording.bytes` gauge or stopped by its descriptor. The client API and the
  `cavalry record` forms follow in the next change
  ([ADR 0031](docs/decisions/0031-truncate-purge-and-delete-a-stopped-recording.md);
  card 95).
- `cavalry record --truncate <id> --to <position or instant>`, `--purge <id> --before
  <position or instant>` and `--delete <id>` cut, purge and delete a stopped recording;
  an instant is resolved through the time index as `replay` resolves one, and for
  `--before` rounded down to the segment base at or before it, printed, so that no
  byte recorded after the instant goes. The client API gains
  `Cavalry::truncate_recording`, `purge_segments` and `purge_recording`. The two
  doctor findings name the `cavalry record` commands to send; the record how-to has
  the three as steps 4 to 6; the glossary has retained position, truncate recording,
  purge segments, purge recording and recording busy
  ([ADR 0031](docs/decisions/0031-truncate-purge-and-delete-a-stopped-recording.md);
  card 95).
- A retention command whose catalog write fails is refused with error 12 and its slot
  repaired as after a file error, so a truncate's failed last write never leaves the
  recording reading live over files already cut
  ([ADR 0038](docs/decisions/0038-repair-a-retention-commands-slot-after-a-failed-catalog-write.md);
  card 95).
- A soak host: `scripts/soak.sh` runs a driver recording a steady stream for days,
  samples `cavalry doctor` and the counters file every minute, rotates the recording
  every hour and purges and deletes by the rotation, stopping cleanly on SIGTERM and
  exiting 1 when the program or the driver ended the run on its own; the
  stream comes from `crates/cavalry/examples/soak.rs`, which publishes at a fixed rate
  and reads every message back once and in order. `docs/how-to/set-up-a-soak-host.md`
  sets the host up, `kip`, with a volume for the log directory and a systemd service,
  and says why the user must linger: logind removes a user's shared memory, the
  driver's files among them, when the user's last session ends, which the driver's
  how-to now names under "When it does not work"
  ([ADR 0034](docs/decisions/0034-run-the-gate-on-grip-and-the-soak-on-kip.md); card
  159).
- ADR 0039, Proposed: the third slice is publish/subscribe over UDP unicast between two
  hosts on the same API as IPC, in four features (cards 179 to 182): the transport
  specified first, Aeron's control frames, log buffer version 1 and client protocol
  version 4 under an ADR a person accepts; the sender thread; the receiver thread and
  the image, with the fault-injection gate for loss, reordering and duplication; then
  the UDP rows on the two bench hosts, a cross-host round trip in the gate, and the
  ten-minute path publishing over UDP. Multicast, the min and tagged strategies, kernel
  bypass and encryption wait for later slices. The cross-host rows need the cable and
  the clock link moved to `grip`, by hand, before the fourth feature starts
  ([ADR 0039](docs/decisions/0039-build-udp-between-hosts-third.md); card 183).
- ADR 0039 Accepted: Rjae accepted the third slice as proposed on 2026-10-31; card 179,
  the UDP transport's specification, is the next feature
  ([ADR 0039](docs/decisions/0039-build-udp-between-hosts-third.md); card 183).
- The review loop's fixer commits each blocker as that blocker's test passes and pushes
  as it goes, so a run that ends early leaves what it finished on the pull request; its
  comment says which gates ran on the head it left, and the four gates still run on the
  last head and in CI. A review whose only blocker is the pull request body is fixed by
  editing the body, and the run asks for the review of the unchanged head as it does
  for a push, instead of failing for want of a commit (issue 11). The agent's `just
  lint-docs` runs over CI's range, the whole branch. `docs/how-to/implement-a-feature.md`
  and the `/pr` skill say when a card's change is more than one pull request: about a
  thousand changed lines (card 178).
- ADR 0040, Proposed: the UDP transport's wire format, Aeron's control frames field for
  field (setup, status message, NAK, RTT measurement, the heartbeat as a data frame of
  length 0) with the sender's and the receiver's rules and Aeron's values, the channel
  URI `cavalry:udp?endpoint=<host>:<port>` with `interface` as its one option, log
  buffer version 1 with the image type and the receiver window, and client protocol
  version 4 with the `cavalry:udp` channel, the channel status indicator id and error
  codes 4 and 9; the pages, their vectors and the two crates land with the acceptance
  ([ADR 0040](docs/decisions/0040-adopt-aerons-transport-frames-as-the-udp-transport.md);
  card 179).
- ADR 0040 Accepted: the UDP transport's wire format is specified. `spec/udp.md`
  version 0 has Aeron's control frames field for field, the setup frame, the status
  message, the NAK, the RTT measurement and the heartbeat, the sender's and the
  receiver's rules with Aeron's values, the channel URI `cavalry:udp?endpoint=<host>:<port>`
  with `interface` as its one option, and 96 vectors; `spec/log-buffer.md` version 1
  adds the image type and the receiver window length; `spec/client-protocol.md` version
  4 accepts a `cavalry:udp` channel, adds the channel status indicator id to the three
  ready events, error codes 4 and 9, two endpoint registries and a 16384-counter file.
  `cavalry-proto` reads and writes the frames, walks a datagram and parses a channel,
  fuzzed; `cavalry-shm` reads and writes version 1 logs and inserts frames into an
  image with a loom model. The driver refuses a `cavalry:udp` channel with code 11 and a
  message naming the sender's change until card 180 lands, and a malformed one with
  code 1 and the reason. A version 3 client closes on a version 4 driver's first
  `publication ready`; restart clients before the driver. The soak host's samples
  double with the counters file, to about 9 GB a day
  ([ADR 0040](docs/decisions/0040-adopt-aerons-transport-frames-as-the-udp-transport.md);
  card 179).
- ADR 0041, Proposed: the UDP sender is the driver's fourth dedicated thread,
  `DriverThread::Sender`, linked to the conductor by two bounded queues as the recorder
  is, owning the send endpoints' sockets and each publication's send state; the socket
  calls `std` lacks come through `nix` 0.31 (`socket`, `uio`, `net`): `sendmmsg` per
  burst on Linux and `sendmsg` per datagram elsewhere, `recvmmsg` and `recvmsg` for the
  control frames, `SndBuf` and `RcvBuf` read back into the endpoint's counters, none of
  them allocating per call; the dependency needs Rjae's approval before it exists
  ([ADR 0041](docs/decisions/0041-send-from-a-sender-thread-through-nix.md); card 180).
- ADR 0041 Accepted: a `cavalry:udp` publication reaches the wire. `add publication` on
  `cavalry:udp?endpoint=<host>:<port>` resolves the host once (code 9 when it does not),
  opens the channel's send endpoint (code 4 when the socket cannot be opened, bound or
  connected; 64 endpoints, code 10 past them, and code 10 when the sender's queue has
  no room), creates the log with the UDP sizes and names the endpoint's
  `channel.status` counter in `publication ready`; the driver's fourth thread, the
  sender, sends setup frames until a status message answers, data in bursts of whole
  frames up to the sender limit in one `sendmmsg` per burst on Linux and one `sendmsg`
  per datagram elsewhere, heartbeats when idle and with EOS after the close, and the
  frames a NAK names; the publisher reads connected from the first status message and
  its limit runs a publication window ahead of the sender position; a closed
  publication lingers until the receiver acknowledges the end of stream or five seconds
  pass, and the last publication's close closes the endpoint. The counters `sender.*`
  (fifteen, with `sender.position` and `sender.limit` per publication),
  `publication.connected` and `publication.disconnected`, `channel.opened`,
  `channel.closed` and the endpoint's five, and `receiver.receive_failed` are in
  `counters.toml` with the publication connection and endpoint state machines.
  `cavalry-driver` depends on `nix` for the socket calls `std` lacks.
  `TermReader::block_from` scans a block from any position. `spec/udp.md`, "The
  sender", cleans a term behind the sender position so the resend window stays, and
  `spec/client-protocol.md`'s `add publication` names the sender's queue as a code 10;
  the receiver, the image and `start recording` over UDP follow with card 181
  ([ADR 0041](docs/decisions/0041-send-from-a-sender-thread-through-nix.md); card 180).
- The client's tests drive a `cavalry:udp` publication against a receiver by hand on the
  loopback, and an allocation gate holds the send path at zero allocations per message
  under the counting allocator, alone in its process (card 180).
- `Driver::start_with_threads` names a fourth thread, `DriverThread::Sender`, which
  parks while it holds nothing to send, as the replayer does with no replay; the bench
  places it on the housekeeping CPU beside the recorder and the replayer until the UDP
  rows give it a core (card 182).
- `cavalry doctor` names a `cavalry:udp` publication no receiver has answered
  (`udp-publication-not-connected`, from a `sender.limit` of 0 and the endpoint the
  label names) and a send endpoint's socket buffer the host capped
  (`udp-socket-buffers`: a send buffer below one datagram of the MTU, Aeron's rule, or
  below a burst on Linux, where one `sendmmsg` hands the socket sixteen; a receive
  buffer reading back less than a host that granted the 131072 asked reads back, twice
  that on Linux), each naming the sysctl of the driver's platform that moves it,
  `net.core.wmem_default` for the send buffer the driver never sets, and the remove,
  wait the linger, add again step that opens a new socket; lists every send endpoint
  (`note udp-endpoint`); and reads a rising `receiver.receive_failed` on an unanswered
  endpoint as the kernel's port unreachable, not a starved socket, from the counters
  file alone
  ([ADR 0039](docs/decisions/0039-build-udp-between-hosts-third.md),
  [ADR 0041](docs/decisions/0041-send-from-a-sender-thread-through-nix.md); card 180).
- ADR 0042 Accepted: the UDP receiver is the driver's fifth dedicated thread,
  `DriverThread::Receiver`, linked to the conductor by two bounded queues as the sender
  is; it rebuilds an image through `cavalry-shm`'s image writer and two bounded scans of
  the frame length word, 1 MiB of term per image per cycle, NAKs the gap they find, keeps
  its readers, every subscription's position and a recording's, to set the status
  message's consumption position and to clean a term behind the slowest, and finds a
  datagram's session in a hash table of 4096 records. Three spec pages are amended
  within their versions, none changing a byte on the wire: `spec/udp.md`, where a
  status message whose frame length is 44 in a datagram shorter than 44 bytes is
  `length`, with a vector; a committed frame the window does not cover whole counts
  `sender.flow_control_limited`; an interface keys a receive endpoint's channel and
  binds nothing; the rebuild scan runs on every cycle on which the high-water mark is
  past the rebuild position, bounded at 1 MiB of term per image; a recording of an
  image is one of its readers, for the status message's trigger and consumption
  position and for the cleaning; an RTT measurement for another receiver counts
  `receiver.dropped_unwanted`; `start recording` records one log per command, a
  publication or an image, the lowest registration id first; and a recording of an
  image stops when the image is retired, before its log is removed.
  `spec/log-buffer.md`, where the receiver window and the cleaning of an image read the
  slowest reader's position, a subscription's or a recording's. `spec/client-protocol.md`,
  where `add subscription` over UDP is code 10 when the receiver's request queue has no
  room, `start recording` over UDP answers 20 and 22 counting images, and `start replay`
  over UDP stays code 11 and is a card of its own
  ([ADR 0042](docs/decisions/0042-rebuild-an-image-on-a-receiver-thread.md); card 181).
- `cavalry-shm`: `ImageWriter::scan` walks an image's frame length words from a
  position to the high-water mark, bounded in bytes per call, and reports the rebuild
  position, the next frame after a gap and where it stopped, so that the driver's
  receiver finds the gap a NAK names in bounded work per cycle
  ([ADR 0042](docs/decisions/0042-rebuild-an-image-on-a-receiver-thread.md); card 181).
- A `cavalry:udp` subscription receives its stream. `add subscription` on
  `cavalry:udp?endpoint=<host>:<port>` resolves the host once (code 9 when it does
  not), binds the channel's receive endpoint (code 4 when the socket cannot be opened or
  bound, and code 4 saying so when the channel's previous endpoint is still closing with
  the receiver; 64 endpoints, code 10 past them, and code 10 when the receiver's queue
  has no room) and answers `subscription ready` with the endpoint's `channel.status`; a setup
  frame from a sender creates an image under `images/` with a registration id of its
  own, announced with `available image` and the sender's address as the source
  identity, which a subscriber reads as any log while the driver's fifth thread, the
  receiver, fills it: frames inserted by term offset within the window the last status
  message set, duplicates changing nothing, a gap NAKed after the NAK delay and every
  NAK retry and nothing delivered past it until it is filled, status messages on the
  three triggers, a heartbeat with EOS ending the stream, and an image retired when its
  stream ends, when it goes quiet for the liveness timeout or when its last subscription
  leaves, with `unavailable image` and the log unlinked; a session record in one of four
  states, pending, in progress, with an image or on cool-down, in a table of 4096, keeps
  a second setup or a late retransmit from creating a second image. The 20 `receiver.*`
  counters, `image.hwm_position` and `image.rebuild_position` per image, and the image's
  state machine, `image.created`, `image.ended`, `image.timed_out`, `image.unsubscribed`
  and `image.removed`, in `counters.toml`. `start recording` on a `cavalry:udp` channel
  records a publication sending on it or an image received on it, the recording one of
  the image's readers. `start replay` on a `cavalry:udp` channel stays code 11, naming
  card 192 ([ADR 0042](docs/decisions/0042-rebuild-an-image-on-a-receiver-thread.md);
  card 181).
- `DriverThread::Receiver` names the fifth thread to the `start_with_threads` hook; the
  bench places it with the housekeeping threads until card 182's rows give it a core.
- The client's tests drive a `cavalry:udp` subscription against a real driver through a
  shim on the loopback that drops every seventh data datagram, duplicates every fifth
  and holds every third until the next has gone: four thousand messages arrive once and
  in order, the gaps NAKed and mended, and the close delivers EOS and the image's end;
  a late subscription elicits the setup; a recording of an image on a second driver
  replays byte for byte; and an allocation gate holds the receive path at zero
  allocations per message under the counting allocator, alone in its process (CLAUDE.md,
  gate 3 for the wire; card 181).
- `cavalry doctor` reads two more `cavalry:udp` findings from the counters file alone:
  loss on an image (`udp-image-loss`: `image.hwm_position` past `image.rebuild_position`,
  the gap's bytes and the counters that say whether it is being mended) and a receive
  endpoint with no image (`udp-subscription-no-image`: no sender's setup frame reached
  the port for a stream a subscription wants, with what to check); the endpoint note
  and the capped receive buffer's finding say send or receive endpoint and the receive
  endpoint's cost ([ADR 0042](docs/decisions/0042-rebuild-an-image-on-a-receiver-thread.md);
  card 181).
- The receiver's id in every status message is drawn through the standard library's
  `RandomState`, seeded from the operating system, as the spec's "random i64" says,
  where it mixed the clock and the process id; `cavalry doctor`'s
  `udp-subscription-no-image` prints the driver-wide `image.created` and `image.ended`
  and says what they settle, a stream that never came told from one that ended while
  the subscription stayed on a driver with one receive endpoint and no earlier stream,
  the checks for a stream that never came kept on any other, and its endpoint note
  calls an endpoint that is closing with nothing on it an endpoint; the shim test
  asserts the image's end of stream lands where the stream stopped (the review notes of
  card 181's pull requests; card 195).
- ADR 0043: `start replay` takes a `cavalry:udp` replay channel, the replay's
  publication put on the wire by the driver's sender as a client's `cavalry:udp`
  publication is, with the recording's term length and MTU; for the replay connect
  timeout the replay has a subscriber while its publication is connected, as
  `spec/udp.md`, "The sender", defines it; the sender holds a status message's term
  offset, and a NAK's term offset and range, against the publication's own term, since
  a replay's keeps its recording's, which `sender.status_messages_rejected` and
  `sender.naks_ignored` now name; `replay.position` and `replay.not_connected` explain
  the UDP case from `sender.position`, `sender.limit` and the publication's connection;
  and the sender's bounds count a replay's publication beside a client's, four counters
  per replay publication. `spec/client-protocol.md`, "Commands", "Events", "Liveness and
  timeouts" and "Sizes", and `spec/udp.md`, "The sender" and "Counters", say so, and
  error code 11 for a `cavalry:udp` replay channel, ADR 0042's one open point, is
  withdrawn; the implementation follows in its own change
  ([ADR 0043](docs/decisions/0043-replay-a-recording-over-a-udp-channel-through-the-sender.md);
  card 192).
- `start replay` takes a `cavalry:udp` replay channel: the replay's publication is put
  on the wire by the driver's sender as a client's `cavalry:udp` publication is, from
  the recording's start position or a position inside it, with the recording's term
  length and MTU, so that a subscription on that channel's endpoint on another host
  reads the recording in order at the recording's positions; the replay ends and
  lingers as over `cavalry:ipc`, it has a subscriber for the connect timeout while its
  publication is connected, and the send endpoints registry, the counters file and the
  sender's queue refuse it with code 10, a refused socket with code 4, each leaving
  nothing behind. The sender now checks a control frame's shape against the largest
  term and its offsets against the publication's own term, as the receiver does, since
  a replay's publication keeps its recording's term, and its bounds count a replay's
  publication beside a client's. Error code 11 for a `cavalry:udp` replay channel is
  gone, and so is the last command that refused the transport
  ([ADR 0043](docs/decisions/0043-replay-a-recording-over-a-udp-channel-through-the-sender.md);
  card 192).
- `cavalry replay --channel <uri>` plays a recording to a `cavalry:udp` channel's
  endpoint, on this host or another, and prints the summary alone; it needs `--stream`,
  the stream the subscriber there reads. It exits 1 after the summary line when no
  subscriber on the endpoint answered the replay within the replay connect timeout,
  5 s, so a script sees a replay that delivered nothing fail, and exits 1 naming the
  reason, with no summary, when its client closes before the replay ends, the driver
  gone among the reasons. `just replay-check` no longer fails when the
  time index resolves its instant to the padding frame's own position, where the first
  line is 64 bytes past it
  ([ADR 0043](docs/decisions/0043-replay-a-recording-over-a-udp-channel-through-the-sender.md);
  card 192).
- ADR 0044 is accepted: the bench is to measure UDP with one message in flight, on the
  loopback in rows every pass runs and across the direct cable between `bench` and
  `grip` in a pair of jobs that meet over it, the round trip gated against `bench`'s
  baseline alone and the one-way reported beside the clock offset; the ten-minute
  path's program is to gain a UDP step on the same stream and `doctor` the interface
  MTU check, each in its own change. The set-up page's "Further hosts" now describes
  `bench` and `grip` with the link's record, and its firewall step the reply path a
  stateless ruleset must open
  ([ADR 0044](docs/decisions/0044-gate-udp-on-the-two-bench-hosts.md); card 182).
- `grip` selects `bench-link` as its clock source: `noselect` on its Ubuntu pool
  lines, which carried `prefer` too and outranked the link by stratum, so `chronyc
  tracking` reads the link and the one-way row across the cable carries the link's
  offset, a standard deviation of 51 ns on 2026-11-04 against 863 ns while the pools
  steered the clock; the set-up page's clock step has the line (ADR 0044's Revisit;
  card 182).
- The bench gains the UDP rows: `claim_commit_32_udp` and `one_way_32_udp` take the
  client rows' path over `cavalry:udp` on the loopback through the driver's sender and
  receiver, one message in flight, a tenth of `--messages` per run, on both bench
  hosts, whose jobs now pin four CPUs, `--pin 8,10,12,14` and `4,5,6,7`, the fourth for
  the receiver; and `udp_round_trip_32` and `udp_one_way_32` cross the direct cable
  between `bench` and `grip` with `--link-here` and `--link-peer` to a peer running
  `--echo`, the new `bench (udp)` job and its `bench (udp peer)`, the round trip held
  to `bench-linux-link` and the one-way reported beside the clock offset the peer job
  prints. The new rows and the new profile gate once their baselines are recorded from
  three of the jobs' processes at one tree, and read `no baseline` until then. The
  harness keeps each benchmark's allocations under its name, prints them under the
  table as `allocations by benchmark`, writes them to the result as
  `allocations_by_benchmark` and names the benchmarks that allocated in the failure
  it exits with, so a run that fails the allocation gate says which row to look at
  ([ADR 0044](docs/decisions/0044-gate-udp-on-the-two-bench-hosts.md); card 182).
- The ten-minute path publishes over UDP: the quickstart program publishes its 1,000
  messages again on `cavalry:udp?endpoint=127.0.0.1:40123`, the same stream through
  the same API, reads them back through the driver's sender and receiver, and prints a
  fourth line, which `just quickstart` checks on Linux and macOS, the third slice's
  acceptance test; the README's status says the first three slices work
  ([ADR 0044](docs/decisions/0044-gate-udp-on-the-two-bench-hosts.md); card 182).
- ADR 0045 is accepted: a padding frame crosses the wire as its 32-byte header alone,
  the last frame of its datagram, its length checked against the term and counted by
  its aligned length, and the receiver writes the header and nothing after it. Before
  it, a `cavalry:udp` stream whose frame length did not divide the 16 MiB term stalled
  at the first term end, the receiver rejecting the padding header and NAKing it for
  ever; every body of 32 or 8 bytes divided the term, so no test had met it. The spec
  says the rule, the vectors carry it and `cavalry-proto`'s datagram check and
  `cavalry-shm`'s image writer follow it
  ([ADR 0045](docs/decisions/0045-carry-a-padding-frame-as-its-header-alone.md);
  card 196).
- `cavalry doctor` reports `udp-interface-mtu` on Linux when an endpoint's address
  routes over an interface whose MTU is below 1436 bytes, the stream's datagram with
  its headers, naming the interface and its MTU; and the new how-to
  [Publish between two hosts](docs/how-to/publish-between-two-hosts.md) is the task of
  a publisher on one host and a subscriber on another, with the channel, the firewall
  and every line `doctor` says when the stream does not connect
  ([ADR 0044](docs/decisions/0044-gate-udp-on-the-two-bench-hosts.md); card 182).
- The fixer and the reviewer of the pull-request workflows run on Opus 5.5 at xhigh
  effort, in `claude.yml`, `review-pull-request.yml` and their briefs' frontmatter: the
  account behind the workflows' token reached its Fable usage limit on 2026-11-04 and
  every reviewer and fixer session died at its first call, while the implementation
  sessions still need Fable; a reviewer from another lab would need API billing to run
  headless, so the loop stays on one action and one token.
- The UDP rows gate on both bench hosts and across the cable: `claim_commit_32_udp`
  and `one_way_32_udp` are recorded in `bench-linux`, `bench-linux-unchecked`,
  `grip-linux` and `grip-linux-unchecked`, and `bench-linux-link` is recorded for
  `udp_round_trip_32`, gated, and `udp_one_way_32`, reported, each from the three
  processes at the tree of 08ce56c, pull request 106's run and attempts 1 and 2 of
  master's run 41830565569, every process the tree had
  ([ADR 0033](docs/decisions/0033-record-a-bench-baseline-from-every-process-at-one-tree.md)),
  so the rows and the profile that read `no baseline` above are judged from this
  change on. The offset beside the one-way row moves from the peer job's print on
  `grip`, whose `chronyc sourcestats` line is the link's own error, nanoseconds, to
  the "System time" line of `chronyc tracking` on `bench`, the residual chronyd has
  still to apply to `bench`'s clock against the public pools, hundreds of
  microseconds, which the `bench (udp)` job prints after the run: `grip` follows the
  time `bench` serves, the raw clock corrected by that residual, while the harness
  stamps with the raw clock on each host, and the line's wording is its sign, the row
  reading the path plus the line on a "slow of NTP time" day and the path minus it on
  a "fast" one ([Run the bench](docs/how-to/run-the-bench.md), step 5;
  [Set up a bench host](docs/how-to/set-up-a-bench-host.md), the clock; card 182).
- [Publish between two hosts](docs/how-to/publish-between-two-hosts.md), followed once
  between `bench` and `grip` on 2026-11-04, now says two things it did not: a driver
  started over SSH on Linux loses its directory when the user's last session ends unless
  the user lingers, the logind default, so the page asks for a session that outlives the
  driver, through the checks that read its directory;
  and `cavalry doctor` prints none of the four counters the page names while the stream
  is healthy, so they are read from the counters file by its layout until the console
  exists, with the values the tutorial's 1,000 messages give (card 182).
- `grip`'s two bench profiles, `grip-linux` and `grip-linux-unchecked`, are re-recorded
  at the tree of 7248621 from its thirteen processes, pull request 109's two runs,
  master's five, pull request 111's three, pull request 113's two and pull request
  114's own, with every gated row's slowest run before and after in the commit and
  the process behind each row that loosened. The unchecked `claim_commit_32_client`
  row had turned `bench (grip)` red on pull requests 102, 103, 106 and 111 at 120 to
  132 ns against a slowest run of 119 ns recorded on 2026-10-31, while the checked
  build did not move and master's own runs at the tree passed. Those reds are not one
  slow run: a whole process sits higher, as pull request 111's first attempt did, all
  twenty-one runs at 119 to 134 ns, where its rerun read 113 to 117. In master's run
  at a718d63 two checked rows stepped up partway through the process and set their new
  slowest runs: `claim_commit_32_recorded` from 126 to 133 ns over its first eleven
  runs to 160 to 171 over the last ten, and `claim_commit_32_client` from 107 to 111
  over its first twelve to 132 to 134 over the last nine. The other seven rows that
  loosened by more than 5 percent did so on one run of one process, 1.3 to 2.3 times
  the median of the row's 273 runs. The host's journal holds nothing in any of the
  thirteen processes' windows but the runner's own line, and its kernel log's only
  events of the afternoon, a snap refresh at 17:14:41 UTC and a packet capture's
  promiscuous mode on the link port at 17:37 to 17:41, fall outside every window;
  card 198 holds all three effects. The trigger in
  [ADR 0033](docs/decisions/0033-record-a-bench-baseline-from-every-process-at-one-tree.md),
  master's own run failing or brushing the gate, did not fire, and a process of a new
  tree never joins an older tree's entry, so the re-record is a person's commit under
  [ADR 0032](docs/decisions/0032-load-the-producer-position-every-two-milliseconds.md),
  made by the owner on 2026-11-04; the rule for a pull request that reads red at a new
  tree while master is green is card 199's (card 182).
- `just bench` names the frame when a measured loop allocates: the harness counts with
  an allocator hook that keeps the first allocation's backtrace and prints it under the
  failure line, frame by frame from the hook to the thread's start, so a run that
  fails the allocation gate says where and not only how many; `stats_alloc` gives way
  to `tracking-allocator` in the harness alone. The count now includes reallocations:
  a measured loop that grows a vector past a capacity it already had fails the gate,
  where before it passed
  ([ADR 0047](docs/decisions/0047-keep-the-first-allocations-backtrace-in-the-bench.md);
  card 176).
- The review loop's fixer run ends the `ci` runs GitHub held on the heads it pushed
  before its last as cancelled, where it left them held, and the `auto merge` run sweeps
  what is still held right before the merge: a held run ended as a failure, with a mail,
  when the branch was deleted at the merge, twenty times in the week to 2026-11-04. And
  the `auto merge` run names its cause when GitHub refuses a merge on a
  branch that moved after the approval: a notice when GitHub has the new head's runs,
  which the loop reviews, and a failure that says what to push when it has none, as on
  pull request 106 (`.github/workflows/auto-merge.yml`, header; card 210).
- The client's replay test no longer reads the replay's image after the drain as if
  it stayed: the driver retires a replay's image once every subscriber has read to the
  end of stream position or the publication linger has passed, as the client protocol
  page says, so the test takes the image's identity from the poll that found it and
  then waits for the image to end or go, which is what the third replay test already
  did; the flake it fixes was the test's race, not the driver's, and
  [Replay a recording](docs/how-to/replay-a-recording.md) now says when the image goes
  (card 185, issue 88).
- ADR 0046, Proposed: the fourth slice is the C ABI and the five language clients over
  it, in six features (cards 204 to 209): the ABI specified first under an ADR a person
  accepts, with Aeron's C client's names and result codes, a committed cbindgen header
  held by `just lint`, a C conformance runner that starts `conformance/`, and two ABI
  rows in the bench; then the Python, Node, Go, JVM and .NET clients, each generated
  from the header and run by `just conformance` on both runners. Invariant 2's
  per-message rules hold the Rust core behind the ABI, under its gate; the code each
  client's generator writes takes no lock, makes no syscall, never awaits, returns
  `Offer`'s codes as values, and makes no allocation of its own per message beyond the
  objects its runtime makes for the crossing, keeping one view per subscription, and one view
  and one buffer claim per publication, across messages where its runtime allows, as
  the ABI's buffer claim is a struct the caller owns and every claim fills, and
  recording with its reason any allocation it cannot avoid; card 205's check in
  `just lint` holds it for locks, `await` and raises, cards 206 to 209 each extend the
  check with their language's forms, and it fails on generated code in a language it
  has no rules for; each card records the rest, and its allocations are counted
  together with the runtime's side; and the runtime's side, its trampoline and the
  objects it makes, may allocate or take the runtime's own lock per message, which
  each client's runner reports beside its one-way p50, p99.99 and max, without a gate.
  Raft is the fifth slice; the first release, with `conformance/upgrade/` and the
  ABI's freeze, is an ADR of its own. Nothing in the
  slice needs a hand on the hosts. `why-cavalry.md`'s "One core, every language" now
  says every client reaches the header through its language's foreign-function
  interface, where it named PyO3 and napi-rs, which bind the Rust crates instead
  ([ADR 0046](docs/decisions/0046-build-the-c-abi-and-the-clients-fourth.md); card 203).
- A publisher that waits for `is_connected()` and then claims reads the limit raised
  for the subscriber or recording that connects it: the driver's conductor stores the
  connected flag's 1 in its limit pass, after the limit's raise, where it set the flag
  as an image joined, a recording started or a status message arrived and raised the
  limit a step later in the cycle. So the claim's `BackPressured` with no subscriber
  behind, which card 187 saw on the macOS gate, is not read. The 0 is still stored
  where the last subscriber, receiver or recording goes, before the limit pass lowers
  the limit, so `BackPressured` comes only while something reads the stream, and a
  publisher at the old limit or the lowered one is told `NotConnected` from that
  moment. The flag holds while the publication stays connected, so a publisher whose
  subscriber reaches the limit while cleaning holds the next raise back reads
  `BackPressured`, with the subscriber at the limit, not `NotConnected`; and a claim,
  which loads the limit before the flag, can read `BackPressured` once when its two
  loads straddle a raise. `spec/log-buffer.md`, "The driver", says the order
  ([ADR 0048](docs/decisions/0048-store-the-connected-flag-after-the-limit.md); card 187).
- ADR 0046 Accepted: Rjae accepted the fourth slice as proposed on 2026-11-05; card 204,
  the C ABI's specification, is the next feature
  ([ADR 0046](docs/decisions/0046-build-the-c-abi-and-the-clients-fourth.md); card 203).
- The reviewer's run has sixty minutes, twice the longest review observed, where
  thirty cancelled four reviews on 2026-11-05 with no verdict and no retry; the
  how-to and the pull request skill say a review takes up to thirty minutes and
  when to add the `review` label (`.github/workflows/review-pull-request.yml`;
  card 213).
- ADR 0051, Proposed: the C ABI, version 0, is the client API as it stands under
  Aeron's C client's names, seven opaque handles, one publication type for both kinds,
  a caller-owned buffer claim that names its publication and its position, so that a
  stale one commits nothing, synchronous adds, Aeron's six result codes with
  `TooLong` as `ERROR` and `CAVALRY_ERR_TOO_LONG`, one table of error codes with the
  driver's on their values from 1 to 99, an error message that keeps its docs anchor
  when it is cut, a refusal of a channel that is not UTF-8 and of an empty directory,
  a frame's position from its image's constants, a poll's fault as a struct, an
  answer for every call made during a claim or a poll, lifetime rules by convention
  with debug builds poisoning what is released, every handle used from one thread at
  a time, and a `cavalry_context_init` that refuses a header of another ABI version;
  its page is `spec/c-abi.md` with the committed
  header as the artefact and vectors from the page's table, cbindgen 0.29.4 is a
  pinned tool of `just lint` and not a dependency, `conformance/c/` is the first
  runner, and two bench rows run through the ABI. The page, the crate, the runner and
  the rows land with the acceptance
  ([ADR 0051](docs/decisions/0051-expose-the-client-api-as-a-c-abi-with-aerons-names.md);
  card 204).
- The driver's replayer removes a purged, truncated or deleted recording's segment
  files without allocating, while the log directory's path is at most 337 bytes, the
  limit `cavalry doctor --log-dir` already checks for the recorder's roll: each file's
  name is formatted on the stack, as the recorder's roll does. `recording.segments_purged`
  now moves one per file as a purge removes it, not by the whole count when the purge
  ends, so the counter reads a running purge's progress, and a purge beside the bench's
  recorded rows adds nothing to the allocation gate's count. `cavalry-log` shares the
  name formatting as `layout::segment_file_name_into` (card 177).
- The `bench (udp)` and `bench (udp peer)` jobs each wait, before their run, until the
  run's other job is in progress, for up to 45 minutes, so the two ends of one run
  meet each other. Before, each host started its end from its own queue: the harness's
  ten-minute wait ended one end whenever the other started more than ten minutes
  later, twice on master on 2026-11-05, and the end that passed had measured against
  another run's end, built from another tree. When two runs' pairs each hold one host
  and wait for each other, the newer run's end fails at once and names the older run.
  A refused token fails the wait at once, an API that does not answer after three
  tries and a wrong job name after a minute, each with its reason. Each failure names
  the re-run that brings both ends back, `gh run rerun <run id> --failed` when both
  failed and the whole run when one passed, since a re-run of one end alone finds its
  partner's earlier result and fails at once saying so. Each of `bench-linux` and
  `grip-linux` names one runner
  (`scripts/wait-for-partner-job.py`; `.github/workflows/ci.yml`;
  [Run the bench](docs/how-to/run-the-bench.md), step 5; card 212).
- ADR 0051 Accepted: Rjae accepted the C ABI as proposed on 2026-11-05, and
  `spec/c-abi.md`, version 0, is its page: eight opaque handles, nine caller-owned
  structs, 58 functions, the six result codes, one table of error codes with the
  driver's below 100, the string, lifetime and thread rules, which claim a struct
  names, a frame's position from its image's constants, and the version check a reader
  runs first; `spec/vectors/c-abi.json`, written by `spec/vectors/c-abi.py`, will hold
  the committed header to the page once `cavalry-ffi`'s test reads it. `spec/README.md` admits an ABI page and its kind of
  vector, and `spec/client-protocol.md` keeps its error codes below 100 for the ABI
  ([ADR 0051](docs/decisions/0051-expose-the-client-api-as-a-c-abi-with-aerons-names.md);
  card 204).
- `CLAUDE.md`'s working rules say which decisions a session makes and which it sends:
  a choice between options is the session's own, on its recommendation, recorded with
  the work, and a `brigade decision` goes to a person only for what needs their own
  hand, a host, a credential, an account, or reverses a decision they made. An ADR,
  one on the wire format, the on-disk log format or the C ABI included, lands Accepted
  on its evidence and names no person to accept it
  ([Implement a feature](docs/how-to/implement-a-feature.md), step 2; card 214).
- The review loop's sweep of the `ci` runs GitHub holds on a fixer's superseded pushes
  deletes each run instead of approving and cancelling it, in the fixer's last job and
  again right before the merge: the approval queued the run into `ci.yml`'s concurrency
  group, which cancelled the head's live run three seconds later (measured on pull
  request 131), so the sweep had to stop while a run at the head was live, which at the
  merge is usual, and what it left held still failed with a mail at the merge (pull
  request 121); a held run accepts a delete, which queues nothing. And the auto-merge an
  approval armed is disarmed by what moves the pull request past that approval: a push
  by a person (`.github/workflows/dispatch-review.yml`), a push by the fixer
  (`.github/workflows/claude.yml`, its last job, before it approves the pushed head's
  held run) and a review that requests changes on the approved head
  (`.github/workflows/auto-merge.yml`), since GitHub's auto-merge survives a push by a
  person with write access and the branch protection requires no review; a head with an
  approval of its own stays armed, and the reviewer's approval of the new head arms it
  again. [Ask the agent to make a change](docs/how-to/ask-the-agent.md) says what
  disarms a merge (card 217).
- `cavalry-ffi` exposes the client API as the C ABI of `spec/c-abi.md`, version 0:
  58 `extern "C"` functions over eight opaque handles, the caller-owned buffer claim,
  header values, fault and constants structs, the codes and the thread-local error
  message, built as a shared library, a static library and an rlib, with the header
  `crates/cavalry-ffi/include/cavalry.h` generated by cbindgen 0.29.4 and held to it
  by `just lint`'s new `lint-header` recipe; the crate's conformance test holds the
  header to the page's vectors, its allocation test counts zero across the seven
  per-message functions, and its README's example calls the ABI from Rust with an
  embedded driver. `CLAUDE.md`'s definition of the data plane gains the boundary's
  three parts
  ([ADR 0051](docs/decisions/0051-expose-the-client-api-as-a-c-abi-with-aerons-names.md);
  card 204).
- The bench's client rows no longer count the driver's conductor retiring the
  subscriber's subscription inside the measured loop: the subscriber thread hands its
  subscription back with its histogram, as the UDP rows' does, and the publisher closes
  it after the counting window, where dropping it on the thread's way out reached the
  conductor's `drop_subscription`, which allocates, while the window was still open
  once in twenty million messages on the macOS gate; the first backtrace the harness
  printed named the frame ([Run the bench](docs/how-to/run-the-bench.md); card 176).
- A merge conflict anywhere is the session's to resolve: `CLAUDE.md`'s working rules,
  the `pr` skill and the `commit` skill say to keep what both changes meant, run the
  gates, and name the files and the resolution in the commit message or the pull
  request body, where the two skills said to stop and hand it back; rebase, a force
  push and `git merge --abort` as a way out stay out (card 218).
- Removing and adding a subscription on one `cavalry:udp` channel in one client no
  longer needs a retry: an `add subscription` that finds the channel's previous
  receive endpoint still closing waits in the driver, with nothing registered, until
  the receiver has closed it and then runs as it arrived, binding the endpoint again;
  waiting adds hold slots of the subscriptions registry, on `cavalry:ipc` adds as on
  `cavalry:udp` ones, and go with a client that closes first;
  `subscription.add_deferred` counts them and `subscription.add_deferred_waiting` says
  how many wait now; an add still waiting after 5 s, a receiver starved that long, is
  refused with code 4 inside the client's response timeout, counted in
  `subscription.add_deferred_timed_out`, which `cavalry doctor` reports
  ([ADR 0049](docs/decisions/0049-hold-an-add-until-the-closing-endpoint-has-closed.md);
  card 194).
- `conformance/c/`, the first runner of the directory gate 1 names: `quickstart.c`, a
  C11 program over the committed header that checks the ABI version both ways,
  publishes 1,000 messages by claim and commit over `cavalry:ipc` and reads them back
  once and in order with the header's fields checked, records the stream, does the
  same over `cavalry:udp` on the loopback, and replays the recording to another
  stream at the positions the recording holds each frame, computed from the image's
  constants; `run.sh` builds the driver and the library, compiles and links the
  program with the runner's C compiler, and runs it against a driver on a directory
  of its own; `just conformance` runs it after the Rust vector suites, with
  `cavalry-ffi`'s header test among them
  ([ADR 0051](docs/decisions/0051-expose-the-client-api-as-a-c-abi-with-aerons-names.md);
  card 204).
- `just bench` measures the client path through the C ABI, `claim_commit_32_ffi` and
  `one_way_32_ffi`: `cavalry-ffi`'s `extern "C"` functions in the bench's own binary,
  behind the crate's `harness` module, with the client rows' driver, pins, warm-up and
  allocation window, so the two sets of rows differ by the boundary alone; the rows
  join each host's baseline as ADR 0033's rule for a new row has it
  ([ADR 0051](docs/decisions/0051-expose-the-client-api-as-a-c-abi-with-aerons-names.md);
  card 204).
- `cavalry-ffi` takes its reviews' notes: `cavalry_replay_ended` refuses a null
  `position` or `reason` as it refuses any null required pointer; the
  `cavalry_context_init` macro sits inside the header's include guard; `just
  lint-header` shows cbindgen's own output when it fails and says what to do next;
  every handle type is asserted `Send` at compile time, which the subscription
  handle's self-pointing views need an `unsafe impl` for (card 204).
- A claim or an offer on a publication whose client has closed is now
  `Offer::Closed`, `CAVALRY_PUBLICATION_CLOSED` in C, before anything is reserved,
  where one that still fit the publisher limit used to read a position: the driver
  ends the stream at the producer position it loads on `client close`, so such a
  frame could land past the stream's end with no subscriber told of it
  ([log buffer, "Closing"](spec/log-buffer.md); card 204).
- `bench`'s two bench profiles, `bench-linux` and `bench-linux-unchecked`, are
  re-recorded at the tree of 3fe13b4 from its twenty processes, pull request 128's
  four runs, master's five with both attempts of a re-run, pull requests 129's four,
  130's two, 132's one and 134's four, with every gated row's slowest run before and
  after in the commit and the process behind each row that loosened. The checked
  `claim_commit_32_recorded` row had turned `bench (linux)` red four times at that
  tree, master's own run among them, at 330 ns against a slowest run of 310 ns
  recorded on 2026-10-31. Card 216 measured card 177's commit against its parent on
  the idle host, three 21-run processes of each interleaved, and found them not
  separated, the parent reading 320 with runs to 330 as the commit does, so any shift
  at the code is under the gate's one clock step of 10 ns, where CI's own processes at
  the two trees sat about one step apart, and the band rose with the host. Seven rows
  loosen by more than 5 percent: the recorded row to 360 ns; the checked
  `claim_commit_32_client` row to 170 ns, by 25 runs in eight of the twenty processes
  with every process's median at 159 or 160 ns; the unchecked `claim_commit_32_udp`
  and `one_way_32_client` rows, each by one run of one process; and three rows by the
  two whole processes that card 219 holds, where one run of a third process also
  reaches the checked `one_way_32_concurrent` row's new 250 ns. The commit is the
  owner's under ADR 0032 (card 216).
- A pull request that changes none of the bench's tree paths and reads red on a bench
  row, at a tree where master's own runs pass against an entry recorded at an older
  tree, now has a rule: the first such red on a row of a profile is a rerun, which a
  person's session starts, and the second on the same row re-records that profile's
  baseline at the tree from every process at it whose artifact is still kept and
  from every red's record, as a person's commit naming the reds. The second red
  re-records only when no commit since the entry's tree touched the tree's paths
  outside `crates/cavalry-control` and `crates/cavalry`: a data-plane crate, built by
  the harness or not yet, its tests, the harness, its host check or the build.
  Otherwise it is a card on those commits, and the pull request waits for the card's
  answer. A third red on the row,
  or a later one, is recorded the same way and waits on that re-record or card. A
  disturbed run, whose row moved as a whole, counts for nothing and joins no entry,
  and a red read before master's own run at the tree waits for that run. Each red
  that counts is recorded before its re-run in a comment on its pull request that
  carries the attempt's result files and starts with a key that the next session
  lists the repository's comments for (`docs/how-to/run-the-bench.md`, step 4). An
  agent's session that holds a red, the fixer, records it, reruns nothing and
  changes no baseline, and its pull request waits for a person's session. Pull request 111's red on `grip`'s unchecked client row at
  7248621's tree on 2026-11-04 was such a red, with no rule then; pull requests 102,
  103 and 106 read red on the same row at trees of their own, which stay changes
  under ADR 0033 and which this rule does not count
  ([ADR 0050](docs/decisions/0050-re-record-at-a-tree-where-pull-requests-read-red.md);
  card 199).
- The udp pair's wait reads the workflow's runs listed as queued as well as in progress
  when it looks for a crossed pair: GitHub lists a run whose jobs are part queued and
  part in progress under queued, so the wait never saw the other run's end holding the
  partner's host, and on 2026-11-06 two pairs crossed for 38 and 23 minutes until a
  person cancelled one end (card 212).
- The two rows through the C ABI, `claim_commit_32_ffi` and `one_way_32_ffi`, have their
  baselines on both bench hosts, in `bench-linux`, `bench-linux-unchecked`, `grip-linux`
  and `grip-linux-unchecked`, from the three processes at c208279's tree, pull request
  139's run and master's at fa0c352 and 5dedadf, as a row added by hand to an entry that
  already exists: each percentile the largest over the processes, no runs list, and the
  compared value the slowest of the 63 runs. The rows are gated from here on (card 204).
- The Python client, `clients/python`, generated from the committed C header by
  `clients/generate.py` (`just generate-clients`): a `ctypes` binding over
  `libcavalry_ffi` that refuses a library of another ABI version when it loads it,
  before any other function, and the client's own code, a `Client` that is a context
  manager and may be shared between threads, a `Publication` whose `try_claim` and
  `offer` return the position or the result code and whose `offer` takes `bytes`, a
  `Subscription` polled with a callable fragment handler that every delivered fragment
  reaches, a `Recording` and a `Replay`; a call the ABI refuses during a claim or a poll
  leaves the client's state as it was; `just lint` regenerates the package and reads its
  per-message functions, and the package's functions they call, for Python's locks,
  `async def`, `await` and `raise`; `just conformance` runs `conformance/python/`, the
  ten-minute path's shape through the client and the README's example, with a report
  of its one-way latency over 100,000 messages and its allocations per message
  ([clients/python/README.md](clients/python/README.md); ADR 0046, card 205).
- `scripts/bench-compare.py --join` adds a process to an entry that holds a row added
  by hand, one with no `p99_99_ns_runs` list (`docs/how-to/run-the-bench.md`, step 4):
  the row keeps no list, each percentile the larger of the row's and the process's and
  `p99_99_ns` the slowest run of both. Before, the join padded the row with its p99.99,
  sliced it per process and exited with "has 0 runs where the harness setting is 21"
  for every process after the first (card 221).
- The Node client, `clients/node`, generated from the committed C header by
  `clients/generate.py node` (`just generate-clients`): a native addon in C over N-API,
  built by `npm install` against the header and linked with `libcavalry_ffi.a`, so the
  package has no runtime dependency, that refuses a library of another ABI version when
  the package loads, before any other function; and the client's own code, typed by
  `index.d.ts`, a `Client`, a `Publication` whose `tryClaim` and `offer` return the
  position or the result code and whose `claim` is a `Buffer` over the log, a
  `Subscription` polled with a fragment handler that every delivered fragment reaches,
  the body a `Buffer` into the log for the call and the header's `reservedValue` a
  `BigInt`, exact for any int64 a publisher sets, whose `poll` throws `HandlerError`
  with the counts when a handler threw or the runtime kept a fragment from the
  handler, a `Recording` and a `Replay`; `buffersHeld()`, the `Buffer`s, received
  bodies and claims' views, whose native objects Node frees only when its event loop
  turns, which a program that runs for long lets turn between batches; a call
  the ABI refuses during a claim or a poll leaves the client's state as it was; `just
  lint` regenerates the package and reads its per-message functions, and the package's
  functions they call, for JavaScript's `async`, `await`, `Promise`, `.then`, `throw`
  and `Atomics.wait`, and the addon's per-message C, the trampoline, the `Buffer`s'
  finalizer, `poll`, `headerField`, `view`, `detach` and the wrappers of the claim, the
  reserved value, the commit, the abort and the offer, for C's locks
  (`pthread_mutex_*`, `pthread_rwlock_*`, `mtx_*`, `uv_mutex_*`, `uv_rwlock_*`,
  `napi_*threadsafe_function*`) and raises (`napi_throw*` and `napi_fatal_*` outside
  the argument readers); `just conformance` runs `conformance/node/`, the ten-minute
  path's shape through the client and the README's example, with a report of its
  one-way latency over 100,000 messages and its allocations per message
  ([clients/node/README.md](clients/node/README.md); ADR 0046, card 206).
- The Go client, `clients/go`, generated from the committed C header by
  `clients/generate.py go` (`just generate-clients`): the module
  `github.com/appshapes/cavalry/clients/go` over the header through cgo, linked with
  `libcavalry_ffi.a` and depending on nothing beyond Go's standard library, whose
  `NewClient` refuses a library of another ABI version before any other function; a
  `Client` that goroutines may share, which holds its own mutex around every call on
  its handle and whose `Close` leaves each handle it made to its own `Close`, a
  `Publication` whose `TryClaim` and `Offer` return the position or the
  result code and whose `Claim` is a `[]byte` over the log, a `Subscription` polled
  with a fragment handler that every delivered fragment reaches, the body a `[]byte`
  into the log for the call and a panic recovered and returned by `Poll` as a
  `*HandlerError`, a `Recording` and a `Replay`; a per-message call that fails keeps
  the ABI's code and message in its handle's `Err`, copied in the C call that failed,
  and every other call returns an `*Error` read on the OS thread of the call that
  failed, held with `runtime.LockOSThread`, since the ABI's error is the thread's and Go
  moves goroutines between threads; a call the ABI refuses during a claim
  or a poll leaves the client's state as it was; `just lint` regenerates the module and
  reads its per-message functions, and the functions they call, for Go's `sync`, a lock
  or a wait through a variable or a field (`.Lock()`, `.RLock()`, `.TryLock()`,
  `.TryRLock()`, `.Wait()`), `go`, channels, `select`, `panic` and `make`, `new` and
  `append`, and the C shim's locks, and fails a function that reads the thread's error
  without holding its thread;
  `just conformance` runs the package's test of its subscription table at its bound of
  4,096 with `go test`, then `conformance/go/`, the ten-minute path's shape through the
  client and the README's example, with a report of its one-way latency over 100,000
  messages and its allocations per message, counted exactly by `runtime.ReadMemStats`
  ([clients/go/README.md](clients/go/README.md); ADR 0046, card 207).
- The JVM client, `clients/jvm`, generated from the committed C header by
  `clients/generate.py jvm` (`just generate-clients`): the package
  `com.appshapes.cavalry` over the shared library through Java's foreign function and
  memory API, Java 25 and nothing else, no build tool and no dependency, whose `Abi`
  refuses a library of another ABI version when it loads, before it binds any other
  function; a `Client` whose methods are `synchronized`, so threads may share it, a
  `Publication` whose `tryClaim` and `offer` return the position or the result code and
  whose `claim()` is a `MemorySegment` over the claimed bytes of the log, one object per
  claim, a slice of a private segment over the address space, a `Subscription` polled with a
  `FragmentHandler` that every delivered fragment reaches, the body a `MemorySegment`
  into the log for the call and the `Header`'s `reservedValue` a `long`, whose `poll`
  throws `HandlerException` with the counts and the first throwable once the ABI has
  returned, a `Recording` and a `Replay`; a call the ABI refuses during a claim or a
  poll leaves the client's state as it was, and a commit, an abort or a reserved value
  after a publication's close answers `ERR_NO_CLAIM`, since the close zeroes the claim
  struct before the ABI frees the handle it names; `just lint` regenerates the package and
  reads its per-message methods, and the package's methods they call, for Java's
  `synchronized`, the locks of `java.util.concurrent`, a lock taken on any receiver, a
  blocking wait, a future, a thread started, `throw` and `new`; `just conformance` runs
  `conformance/jvm/`, the ten-minute path's shape through the client and the README's
  example, with a report of its one-way latency over 100,000 messages and its
  allocations per message ([clients/jvm/README.md](clients/jvm/README.md); ADR 0046,
  card 208).
- The .NET client, `clients/dotnet`, generated from the committed C header by
  `clients/generate.py dotnet` (`just generate-clients`): the namespace `Cavalry`
  over the shared library through P/Invoke with the source-generated marshalling,
  .NET 10 and nothing else, no package reference and no native shim, whose `Abi`
  refuses a library of another ABI version when it loads, before any other function;
  a `Client` whose methods hold its lock, so threads may share it, a `Publication`
  whose `TryClaim` and `Offer` return the position or the result code and whose
  `Claim` is a `Span<byte>` over the log, a `Subscription` polled with a
  `FragmentHandler` that every delivered fragment reaches, the body a
  `ReadOnlySpan<byte>` into the log for the call and the `Header`'s `ReservedValue` a
  `long`, whose `Poll` throws `HandlerException` with the counts and the first
  exception once the ABI has returned, a `Recording` and a `Replay`; a call the ABI
  refuses during a claim or a poll leaves the client's state as it was; `just lint`
  regenerates the package and reads its per-message members, and the package's
  members they call, for C#'s `lock`, the locks and waits of `System.Threading`,
  `async`, `await`, `Task`, `.Wait(`, `.Result`, a thread started, `throw`, `new` and
  an interpolated string; `just conformance` runs `conformance/dotnet/`, the
  ten-minute path's shape through the client and the README's example, with a report
  of its one-way latency over 100,000 messages and its allocations per message
  ([clients/dotnet/README.md](clients/dotnet/README.md); ADR 0046, card 209).
- The Python, Go and Node clients zero a publication's claim struct when they close
  it, so a `commit`, `abort` or `set_reserved_value` after the close hands the ABI a
  struct that names no publication, answered with -1 and `CAVALRY_ERR_NO_CLAIM`, and
  the ABI reads no handle the close freed (`spec/c-abi.md`, "Which claim a struct
  names"); each runner checks the three calls after a close, and the generator's tests
  hold each close to the zeroing. Found by pull request 146's review in the JVM client,
  which card 208 fixed, and fixed in the .NET client before card 209's pull request
  (issue 147; card 222).
- ADR 0052, Accepted: the fifth slice is Raft on the durable log, in eight features,
  one card each: multi-destination cast on `cavalry:udp` as the fan-out, decided over
  one publication per follower; the cluster protocol page under Aeron Cluster's names;
  the replicated log with a commit at a quorum and the service a subscriber in any
  language; the election with gate 3's fault injection; the cluster client through the
  ABI and the five clients; catch-up, snapshots and standby in the open core; a rolling
  upgrade that builds `conformance/upgrade/`; and the cross-host gate with `kip` as the
  third host after card 159's week, the one card that needs a hand on a host
  ([ADR 0052](docs/decisions/0052-build-raft-on-the-durable-log-fifth.md); cards 224
  to 232).
- ADR 0053, Accepted: multi-destination cast on `cavalry:udp`, the fifth slice's
  first feature, specified: `spec/udp.md` moves to version 1 with a publication
  channel `cavalry:udp?control=<host>:<port>|control-mode=manual` whose sender binds
  the control address and sends every datagram to the destinations a client names,
  `fc=max|min` on a publication's channel with `min` the slowest receiver's pace and a
  receiver dropped after 5 s without a status message, and the counters for both;
  `spec/client-protocol.md` moves to version 5 with `add destination` and `remove
  destination` under Aeron's type ids 7 and 8; every frame's version byte stays 0.
  31 channel vectors and 13 client protocol vectors pin the grammar and the layouts,
  `cavalry-proto` parses and encodes them, and the driver answers a control channel,
  `fc=min` and the two commands with error code 11 naming card 225 until its sender
  change lands, and a subscription's channel that names a publication's keys with
  code 1 ([ADR 0053](docs/decisions/0053-add-multi-destination-cast-to-cavalry-udp.md);
  card 225).
- The driver sends a control channel's publications to every destination a client
  adds with `add destination`, binding the control address and sending each burst to
  each destination in turn, a datagram one destination did not take whole going to all
  again and a destination the socket cannot reach holding the others back by nothing;
  `fc=min` keeps a table of at most 64 receivers per publication, the sender limit the
  slowest answering receiver's position plus window, a receiver dropped after 5 s
  without a status message or on end of stream; the counters
  `sender.flow_control_receivers_timed_out`, `sender.flow_control_receivers_full`,
  `sender.flow_control_receivers` and `channel.destinations` explain both, and
  `cavalry doctor` names a control channel with no destination, a `min` publication
  with fewer receivers answering than destinations, a dropped receiver and a full
  table ([ADR 0053](docs/decisions/0053-add-multi-destination-cast-to-cavalry-udp.md);
  `docs/reference/udp.md`; card 225).
- A control channel's closed publication lingers for the linger timeout, 5 s, sending
  heartbeats with EOS and answering every destination's NAKs: a status message with
  the end-of-stream flag from one destination no longer ends the linger for all of
  them, so a slower destination still recovers the stream's tail; on a channel with an
  endpoint the flag ends the linger as before. Under `fc=min` a receiver dropped from
  the table because its stream ended is no longer counted in
  `sender.flow_control_receivers_timed_out`, so a clean close leaves `cavalry doctor`
  with nothing to report, and a status message with the flag adds no receiver to the
  table
  ([ADR 0054](docs/decisions/0054-linger-a-control-channels-publication-for-its-timeout.md);
  `docs/reference/udp.md`; card 225).
- An `add publication`, or a replay's publication, on a channel whose previous send
  endpoint is still closing after its last publication's linger, its socket still
  holding the control address or the interface's port, is answered with code 4 and a
  message saying so and to send the command again after a moment, in place of a
  message about a port in use; an add during the linger uses the endpoint that is
  still open, as before
  ([ADR 0055](docs/decisions/0055-answer-an-add-publication-on-a-closing-send-endpoint-with-a-retry.md);
  card 225).
- Under `fc=min`, a receiver whose stream ended stays in a closed publication's table
  until the linger ends, so `sender.flow_control_receivers` keeps counting it, and
  `cavalry doctor` no longer warns for 5 s after every clean close on a control
  channel that the destinations stopped answering; on a live publication such a
  receiver is still dropped on the next cycle
  ([ADR 0056](docs/decisions/0056-keep-an-ended-receiver-in-a-closed-publications-table.md);
  card 225).
- The C ABI moves to version 1 with `cavalry_add_destination` and
  `cavalry_remove_destination`, each taking the client, the publication and the
  destination channel, and the Rust client gains `Cavalry::add_destination` and
  `remove_destination` by the publication's registration id; the five generated
  clients gain the method on their client object, a package of version 0 refusing the
  library until it is regenerated, as the clients README says; the C runner sends one
  stream to two destinations and checks a duplicate add, an absent remove and a
  destination of the other family ([ADR 0057](docs/decisions/0057-take-the-c-abi-to-version-1-with-destinations.md);
  card 225).
- `just bench` measures multi-destination cast on the loopback, `claim_commit_32_udp_mdc`
  and `one_way_32_udp_mdc`, a control channel under `fc=min` to two subscriptions of the
  driver, and the link pass runs the same rows over the cable to the peer and this host
  at once after the link rows, the peer's `--echo` reading the second stream without
  echoing it; both are reported and never gated until a host change
  gives them cores ([ADR 0052](docs/decisions/0052-build-raft-on-the-durable-log-fifth.md),
  feature 1; `docs/how-to/run-the-bench.md`; card 225).
- `spec/cluster.md` version 0 specifies the cluster protocol before any of it is built:
  the members string and the four streams, the message header with schema 1 and
  version 0, the log's entries, the eight consensus messages, the session protocol, the
  snapshot marker, the election's eighteen states, the quorum and commit position rules,
  the sessions, Aeron's timeouts as constants and what a reader rejects, with 181 vectors
  from `spec/vectors/cluster.py` and a glossary row per term
  ([ADR 0058](docs/decisions/0058-specify-the-cluster-protocol-from-aerons-codecs.md);
  `docs/reference/cluster.md`; card 226).
- `cavalry-proto` reads and writes every message of `spec/cluster.md` in `cluster`, the
  members string and the protocol's arithmetic, allocating nothing, and `just
  conformance` runs its 181 vectors through `tests/cluster_conformance.rs`, with a
  no-panic test on every commit and a `cluster` fuzz target under `just fuzz`
  ([ADR 0058](docs/decisions/0058-specify-the-cluster-protocol-from-aerons-codecs.md);
  card 226).
- `spec/udp.md` version 2 adds the position keys `init-term-id`, `term-id` and
  `term-offset`, which create a publication at a position, and `spec/client-protocol.md`
  version 6 arms a recording: `start recording` on a `cavalry:udp` stream with a live
  subscription and no image answers `operation success` and starts the recording at the
  next image's join position, then answers again with `recording ready`, or with
  `error` when the start fails or when the stream's last subscription goes first and
  drops the armed recording, counted in `recording.armed`, `recording.armed_started`,
  `recording.armed_failed` and `recording.armed_dropped`; `cavalry-proto` parses the
  keys with 16 new channel
  vectors, the driver refuses the keys on a subscription, a destination and a replay
  channel with code 1, the replay's with the reason `position`, since a replay starts
  at its own position, and it creates the log at the keys' position, joins under the
  same keys alone, and arms, starts, fails or drops the recording as the page says
  ([ADR 0059](docs/decisions/0059-create-a-publication-at-a-position-and-arm-a-recording.md);
  card 227).
- The release-notes email's subject is the product and its version, `Cavalry 0.1.0`:
  the highest `version` under `Cargo.toml`'s `[workspace.package]` among the commits
  the email covers, in place of `Cavalry: what's new, October 26 to November 1, 2026`,
  the shape Brigade's email uses too. A preview still starts with `[preview]`, and the
  email's own title still names the dates it covers (card 189).
- The finish line for early testers is drawn: Cavalry is ready for an engineer outside
  AppShapes when version 0.1.0 is published and six conditions hold, each proved by a
  gate or a record: the five slices work, the binary and the five clients install from
  their registries, no known defect lies about delivery, the frozen surfaces are named
  and an upgrade across two published versions has run, a cluster of three has soaked
  for a week, and the README says what is not built. The sixth slice is the first
  release
  ([ADR 0061](docs/decisions/0061-draw-the-finish-line-for-early-testers-at-the-first-release.md)).
  A new page surveys the project from its first commit and gives the roadmap to that
  line, with what waits on a person and an estimate
  ([Where is Cavalry, and what is left before early testers?](docs/explanation/where-cavalry-is-and-what-is-left.md);
  card 234).
- `spec/durable-log.md` version 2 adds the terms file, a cluster member's record of
  recorded terms and its candidate term, with 27 vectors, and `cavalry-log::terms`
  creates, reads, appends to, truncates and stores it, two synced writes for a term
  and one for a vote, each write a crash tears read as the state before it: the
  candidate term has two slots under a checksum each, so a member that crashes while
  it stores a vote restarts with the vote stored before, never with none; the file is
  created with its magic last, so a creation a crash tore is written again; and a
  checksum that fails in an entry, the last one's too, or in both candidate slots, and
  an empty entry before the last, are refused as damage with what to do, the file left
  as it was and no term dropped; ADR 0060 decides the replicated log's shape
  for card 227: the consensus channel one publication per peer, a member's append
  position its recording's synced position, the leader appointed by the members
  string's order until card 228, the keys `cluster.members` and `cluster.member.id`,
  and the service's bounded poll
  ([ADR 0060](docs/decisions/0060-replicate-the-log-with-an-appointed-leader.md);
  card 227).
- The five language runners' handler round no longer fails about one CI run in
  several with "it saw []". The round offered ten messages and polled once; a
  publication can accept an offer before the subscription holds its image, which it
  takes on a poll after the client's conductor hands it over, so that one poll could
  deliver nothing. No message was lost: the ten arrived on a later poll, once and in
  order. The runners now poll until `image_count` is above 0 before the ten offers,
  and `Subscription::poll`'s and `cavalry_subscription_poll`'s documentation says
  what a caller sees and what to do (cards 223, 233 and 235). The
  [publish and subscribe tutorial](docs/tutorials/publish-and-subscribe.md), the
  `cavalry-client` and `cavalry-ffi` READMEs and `examples/quickstart.rs` said
  `not-connected` lasts until the subscription's image joins; they now say it lasts
  until the driver has made the image, which the subscription takes on a later poll.
- `spec/client-protocol.md` version 7 adds a client's counter: `add counter` and
  `remove counter`, Aeron's commands 9 and 10, answered with `counter ready` and
  `unavailable counter`, Aeron's events 3848 and 3849, and error code 5, `unknown
  counter`. A client adds a counter of a type `counters.toml` gives the new scope
  `client`, with a key of at most 112 bytes and a label of at most 124, owns it and
  alone writes its value; the driver frees it when the client removes it, closes or
  is timed out, and holds at most 256. Every version 6 encoding is unchanged; a
  version 6 client closes on the first of the two events, so an upgrade restarts
  the clients before the driver. `cavalry-proto` reads and writes the two commands
  and the two events over 28 new vectors, 143 in all, and the driver answers both
  commands with code 11 until its registry lands, the second of the ADR's three
  changes
  ([ADR 0062](docs/decisions/0062-add-a-clients-counter-with-aerons-counter-commands.md);
  card 227).
- The driver adds and frees a client's counter: `add counter` allocates a record of
  the counters file with the type id, key and label the client gave and answers
  `counter ready`; `remove counter` answers `operation success`, then `unavailable
  counter` to every client, and frees the record; a client that closes or is timed
  out loses its counters the same way. The driver holds at most 256, refuses a type
  that is not a client's with code 11 and another client's remove with code 5.
  `counters.toml` gains the scope `client` and its first counter,
  `cluster.commit_position`, which a cluster member's consensus module will add; the
  code 11 the driver answered both commands with is gone
  ([ADR 0062](docs/decisions/0062-add-a-clients-counter-with-aerons-counter-commands.md);
  card 227).
- The Rust client adds a counter: `Cavalry::add_counter(type_id, key, label)` returns a
  `Counter` the client owns, whose value it alone writes, with `store_release` for a
  position another process loads, `set`, `increment` and `add`; `cavalry doctor` and
  every reader of the counters file see it under its label. The driver frees it when
  the `Counter` is closed or dropped and when the client closes or is timed out, and a
  `Counter` stores nothing once its client is closed. A key or a label past its bound
  is refused before anything is sent. `cavalry` re-exports `Counter`
  ([ADR 0062](docs/decisions/0062-add-a-clients-counter-with-aerons-counter-commands.md);
  card 227).
- A driver's session ids start at a random i32, as `spec/client-protocol.md` has
  always said. The first id came from the driver's clock, in steps of 256 ns, so two
  drivers that read the same clock value as they started, the same microsecond where
  the clock steps by one, chose the same session ids. When both then sent one stream to
  one receive endpoint, as the members of a cluster started together send their
  consensus streams, the receiver held the two publications as one session, answered
  the first, and never answered the second: its publisher read `not connected` for as
  long as it ran, and `cavalry doctor` reported `udp-publication-not-connected` on a
  subscription that was there. On a macOS laptop, 2 of 42 starts of three members
  together did this. The id is now drawn from the operating system's randomness (card
  227).
- A replay that follows a live recording no longer allocates on the driver's replayer
  thread. Once a second the replayer reads the recording's catalog slot to learn
  whether it stopped, and each read built the descriptor's channel and source
  identity as two strings: two allocations a second for every such replay, on a
  thread that copies frames. The replayer now reads the stop alone, through
  `CatalogReader::stop_position` in `cavalry-log`, which runs the checks `get` runs.
  A new allocation gate, `cavalry-client`'s `replay_follow_allocations`, holds three
  seconds of a following replay to no allocation at all (invariant 2; card 227).
- The cluster protocol says how an appointed leader leads: `spec/cluster.md` gains "The
  appointed leader", the path the replicated log takes through the election's states
  until the election is built. Every member canvasses, and the member with the lowest
  id is the one that may lead: it leads where canvass would have it nominate, with no
  ballot, and never while a member that reported holds a log ahead of its own, so a
  leader that lost its log directory does not lead a second time. In leader ready it
  waits for every member it has heard to join its log, for at most 10 s, before it appends the
  term's first entry. A member that would need the catch-up or a truncation fails its
  election and is counted; a leader below a quorum stays the leader and is counted; a
  member leaves the live log when it begins an election, and a follower whose image of
  the log is gone begins one; a member does not restart on a log that holds a term,
  and a cluster is started again as a new cluster, every member on a new log
  directory. A session message with cluster session id −1 is one the leader's own
  process appended, of no session. `cavalry-proto` has the three rules,
  `cluster::appointed_leader`, `cluster::appointed_canvass` and
  `cluster::leader_ready`, with their vectors, and the Counters table names
  `cluster.terms_led`, `cluster.appointed_leader_behind`,
  `cluster.leader_quorum_losses` and `cluster.log_images_lost`. The consensus module is
  a client of its driver, not a thread linked to the conductor by queues
  ([ADR 0063](docs/decisions/0063-run-the-consensus-module-as-a-client-and-lead-by-appointment.md);
  card 227).
- A `Recording` follows its recording's stop. `Recording::stop_position()` says where
  the recording stopped once the driver has sent `recording stopped`, whichever
  client stopped it or whatever ended it, and `position()` and `synced_position()` are
  that position from then on. That position is durable: the catalog's stop, or the
  position last synced when the stop's own writes failed
  ([ADR 0064](docs/decisions/0064-announce-a-failed-stop-at-the-position-last-synced.md)).
  Before this, a handle kept reading the two counters of
  a stopped recording, which the driver frees at the stop and gives to other counters
  a second later, so a position could become another counter's value. Each read now
  checks that the record is still the recording's, by the recording id in its key, and
  returns the last value read from the recording's own record when it is not and the
  stop has not arrived. `MAX_RECORDINGS`, 1,024, moves to `cavalry-proto`, where the
  driver's registry and the client's take it (card 227).
- The Rust client holds an armed recording: `Cavalry::arm_recording(channel,
  stream_id)` returns an `ArmedRecording`, and `ArmedRecording::poll()` returns the
  `Recording` once the driver has started it with the stream's next image, or the
  driver's error when it dropped the armed recording or its start failed. Before
  this, a `start recording` the driver answered by arming was an error that named a
  driver bug, and the second answer was dropped. `Cavalry::start_recording` now
  returns `ClientError::RecordingArmed`, reason `recording-armed`, on such a stream,
  with what to call instead. `cavalry` re-exports `ArmedRecording`. The C ABI is
  unchanged: its call for an armed recording and the error's own code come with its
  next version
  ([ADR 0059](docs/decisions/0059-create-a-publication-at-a-position-and-arm-a-recording.md);
  card 227).
- The driver announces a stop whose own writes or syncs failed at the position last
  synced. A full disk can refuse the stop's index entry, its syncs or its catalog write.
  The recording is still stopped, and `recording.write_failed` counts the failure.
  `recording stopped` now carries the position the recorder had synced before the stop,
  where it carried the position written, which no sync may have covered. So the stop
  position a client reads is durable on every path, and the stop that recovery rebuilds
  at the next open is at or above it. The `stop recording` command is still answered
  with `operation success`
  ([ADR 0064](docs/decisions/0064-announce-a-failed-stop-at-the-position-last-synced.md);
  card 227).
- The driver gives up a recording one of whose syncs failed, and runs no stop for
  it. Before, it ran the recording's stop, whose sync, asked about bytes a sync had
  already failed on, can return success and write nothing: the catalog then held the
  position written as the stop and `recording stopped` carried it, for bytes that may
  not be on the disk. Now the recorder writes and syncs nothing more of such a
  recording, `recording stopped` carries the position the last sync that succeeded
  covered, and the catalog's slot stays live until the next start's recovery. A write
  the disk refused with no sync failing is followed by a stop as before, at the
  position written. `recording.write_failed` counts a recording once, whichever of
  its writes, its syncs and its stop failed. With ADR 0064, the position a client
  reads from `recording stopped` is on every path one a sync that returned success
  covered
  ([ADR 0065](docs/decisions/0065-give-up-a-recording-whose-sync-failed-without-a-stop.md);
  card 227).
- The cluster protocol says how a service reads the committed log, and the client has
  the poll it needs. A service is a client of its member's driver: it reads the
  member's own recording of the log through a replay that follows the recording, on
  the `cavalry:ipc` service log stream, 104, and polls it no further than the member's
  commit position, so the path is the same on the leader and on a follower and a
  service may start at any time, from any entry the recording its member names
  holds. `spec/cluster.md`, "The service",
  says so, with the member's new counter `cluster.log_recording_id` and the two
  checks a service makes before it takes a commit position as its bound;
  `spec/log-buffer.md`, "Reading", says what a bounded read is. The Rust client gains
  `Subscription::bounded_poll(session_id, handler, limit_position, fragment_limit)`,
  which reads one image and delivers no frame that begins at or past the limit, and
  `cavalry-shm` the `TermReader::bounded_poll` under it. The reader of the committed
  log and the counter follow
  ([ADR 0066](docs/decisions/0066-feed-the-committed-log-to-a-service-through-a-replay-of-the-members-recording.md);
  card 227).
- `cavalry-raft` has its first public items: the core of a cluster member.
  `Consensus` is one member's state machine on the appointed leader's path of
  `spec/cluster.md`: the leader canvasses, leads when no member's log is ahead of its
  own, records its term, announces it, appends the term's first entry and commits what
  a quorum has recorded; a follower canvasses, joins the leader's log at its append
  position and reports that position as it rises. It does no I/O of its own: it
  reaches the consensus stream, the log, its recording and its `terms` file through
  the `Io` trait. `memory::Cluster` gives it that in memory, one to eight members in
  one process on a clock the caller steps, with loss, delay up to 10 s, cuts, paused
  syncs and a member started again on an empty log directory to set, which the
  crate's README example and its tests run; its consensus stream holds at most 4,096
  messages in flight and loses, and counts, one sent while it is full. The module
  that runs the core over a driver follows
  ([ADR 0063](docs/decisions/0063-run-the-consensus-module-as-a-client-and-lead-by-appointment.md);
  card 227).
- The election of a cluster member is a registered state machine, `cluster_election`,
  and every move of it adds one to a counter: `counters.toml` has the eleven
  `cluster.*` counters a member's core keeps, ids 110 to 120, among them two new ones,
  `cluster.election_steps` and `cluster.elections_closed`, and
  `cavalry_raft::Counts::by_counter` pairs each count with its counter's id for the
  module to store. `docs/reference/counters.md` lists them and the machine's
  transitions
  ([ADR 0067](docs/decisions/0067-register-the-elections-state-machine-with-its-core-and-count-each-step.md);
  card 227).
- A cluster member runs over a driver: `cavalry_raft::ConsensusModule` is the consensus
  module of the replicated log, a client of the driver in its directory. Three members
  on three drivers appoint their leader, the followers arm their recordings before its
  first frame, and an entry the leader's process offers is committed once a quorum has
  synced it, with every member's recording holding the same bytes. The module stores
  the member's role, election state, term, append position and commit position, and
  what it counts, as client counters in the driver's counters file: the eleven
  counts of its core, from `Counts::by_counter`, `cluster.commit_position`, and five
  counters this change registers in `counters.toml`, ids 121 to 125,
  `cluster.role`, `cluster.election_state`, `cluster.leadership_term_id`,
  `cluster.append_position` and `cluster.messages_rejected`, with the member's role
  as the state machine `cluster_role`. A member does not start again on
  a log directory that holds a term: the error says to start the cluster again as a
  new cluster, every member on a new log directory
  ([ADR 0063](docs/decisions/0063-run-the-consensus-module-as-a-client-and-lead-by-appointment.md);
  card 227).
- `cavalry run` starts a cluster member. Two configuration keys name the cluster,
  `cluster.members`, the members string every member is given, and
  `cluster.member.id`, this host's id in it, set as `CAVALRY_CLUSTER_MEMBERS` and
  `CAVALRY_CLUSTER_MEMBER_ID`. With both set, `run` starts the member's consensus
  module beside the driver, on a thread of its own with the conductor's idle strategy,
  and stops it before the driver; with one set and not the other it starts nothing and
  says which is missing. `cavalry doctor` checks both keys, `cluster-members` and
  `cluster-member-id`: set together, a members string the protocol takes, an id the
  string holds, and every endpoint a name that resolves. `cavalry::cluster` re-exports
  `ConsensusModule` for a program that runs a member itself
  ([ADR 0060](docs/decisions/0060-replicate-the-log-with-an-appointed-leader.md);
  card 227).
