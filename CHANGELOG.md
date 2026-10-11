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
- ADR 0004 (proposed): bots and agents work on branches; the writer, reviewer and
  auto-merge workflows, which token each job holds and why, squash merges whose
  message is the pull request body, and Blacksmith runners for every job.
- A reviewer agent reviews every same-repository pull request, read-only, and
  approves or requests changes; the job fails if no review lands. The `review` label
  runs it again, and a Dependabot pull request is handed to it with that label. The
  briefs for the reviewer and the writer are in `.claude/agents/` (ADR 0004).
