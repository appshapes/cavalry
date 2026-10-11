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
