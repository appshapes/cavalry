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
- GitHub Actions runs `just lint` and `just test` on Ubuntu and macOS for every push
  to `master` and every pull request. Actions are pinned to commit SHAs; Dependabot
  opens a weekly pull request to move them.
