# 0006. Build the documentation gate from mdBook, rustdoc and a link script

- **Status:** Proposed
- **Date:** 2026-10-11
- **Invariants affected:** 6
- **Supersedes:** none

## Context

`CLAUDE.md` names the gate for invariant 6: `just docs` builds rustdoc and the mdBook
with warnings denied, runs every code block and checks links; `just lint-docs` fails a
commit that changes a public item, counter, config key or `spec/` without touching
`docs/` or a README. `.claude/rules/docs.md` requires both to pass before a commit.
Until this decision both recipes printed "not built yet", while the repository already
held six ADRs, the competitive brief, the glossary and a how-to page with nothing
checking a link, a heading or a page left out of the book. The reviewer brief treats a
missing gate as a blocker for a change that touches what it would check, so the first
documentation-heavy feature would have had to build the gate on the side.

Three facts set the shape. First, the gate must run in seconds on a laptop and on both
CI runners, and must not go red for a reason outside the repository: a link checker
that fetches the web turns a site's outage into a failed commit on `master`. Second,
`mdbook` ships prebuilt binaries for both runner platforms and the `taiki-e/install-action`
catalogue verifies them against its manifest, while `lychee`, the Rust link checker, is
not in that catalogue and compiles for minutes on each runner. Third, rustdoc with
`-D warnings` was already clean on every crate (ADR 0003, Evidence), and mdBook 0.5
builds pages but checks no links and has no deny-warnings switch.

## Decision

`just docs` runs rustdoc with `RUSTDOCFLAGS="-D warnings"`, builds the book from
`book.toml` at the repository root with `src = "docs"`, output under `target/book` and
`create-missing = false`, fails on any `[WARN]` or `[ERROR]` line mdBook prints, runs
`mdbook test` for the book's Rust code blocks, and runs `scripts/lint-docs.py links`,
which checks that every page under `docs/` is listed in `docs/SUMMARY.md` and exists,
that every relative link in `docs/`, `README.md` and the crate READMEs resolves and
every anchor names a heading in its target, and that every ADR is named
`NNNN-title.md` with a matching title and a valid Status. `just lint-docs` runs the same
script in full, which adds the change check over a diff range: in CI the pull request's
merge base or the push's range, on a laptop the working tree against `HEAD` when
anything is modified, else the last commit. The change check fails a change to a public
item (a line with `pub ` under a crate's `src/`), to `spec/`, `counters.toml` or
`config.toml` with no change under `docs/` or to a README; a change to `spec/` with no
ADR in the same change whose Status is Accepted; and an edit to an ADR that was already
Accepted beyond its Status line. CI runs both after `just lint` and `just test`, on both
runners, with `mdbook` 0.5.4 installed by `taiki-e/install-action` pinned by SHA and the
checkout at full depth. External links are `just docs-links`, which runs `lychee` online
and is not a gate. The commit skill and the writer's brief run all four gates before a
commit or a push. `docs/SUMMARY.md` is grouped by the four kinds of page; the root
`README.md` stays GitHub's front page and is not in the book.

## Alternatives considered

- **`mdbook-linkcheck` as a backend.** Lost: a third-party backend that fetches the web
  by default, with its own binary to install, for a check the script does offline.
- **`lychee` as the gate.** Lost: online by nature, so a site going down fails a commit
  on `master`; not in the install action's catalogue, so each runner would compile it;
  and the only offline check it adds over the script is one the script now has. It
  stays as `just docs-links` for the brief's external sources.
- **`cargo install mdbook` on the runners.** Lost: minutes per runner on a cache miss,
  and the cache key moves with `Cargo.lock`. A prebuilt binary verified against a
  pinned manifest installs in seconds.
- **The book's output under `docs/book`, as the `.gitignore` once said.** Lost: mdBook
  copies non-page files from its source directory into the output, so a build directory
  inside `docs/` would copy itself on the second build. `target/` already holds every
  other build product.
- **Allow mdBook warnings.** Lost: a warning today is a broken page tomorrow, and the
  rule for rustdoc is the same.
- **Public items by rustdoc's JSON output or a Rust parser.** Lost for now: the JSON is
  nightly-only, and a parser means the `xtask` crate ADR 0003 defers. A `pub ` line in
  a diff over-approximates in a safe direction: a false hit asks for a doc change, a
  false miss needs a public item with no `pub` on its line, which Rust does not allow.
  Revisited with ADR 0003's `xtask` item.
- **Run shell code blocks too.** Lost for now: the only shell path the docs promise is
  the README's ten-minute path, and `just quickstart` is its harness by name. Rust
  blocks run through `mdbook test` today.
- **The root `README.md` as the book's first page.** Lost: its links are repository
  paths, which do not resolve from inside the book.

## Evidence

- Laptop, 2026-10-11, mdbook 0.5.4 and lychee 0.24.2 from Homebrew: `just docs` and
  `just lint-docs` pass on this commit; the numbers are in the commit message.
- Added at acceptance: the CI run ids on both runners with their times.

## Consequences

Easier: a broken link, a page left out of the book, a malformed ADR, an edit to an
accepted ADR, or a public item that ships without a doc change is caught on the laptop
before the commit and on both runners after it; the book is one `just docs` away; the
writer and the reviewer have a gate to cite instead of a rule to argue.

Harder: a laptop needs `mdbook` installed, and the recipe says how; the `pub ` check
asks for a doc change on some edits that touch no public surface, which a README line
answers; `mdbook` is one more pinned version to move, in two places, and Dependabot
moves only the action.

Revisit: when `counters.toml` and `config.toml` exist, the glossary check
`.claude/rules/docs.md` describes joins the script; when generated reference pages
exist, the script checks they are not hand-edited; when the `xtask` crate exists, the
public-item check moves to a parser; when the README's ten-minute path exists,
`just quickstart` runs its shell blocks.

## References

- `CLAUDE.md`, "Gates" and "Documentation"; `.claude/rules/docs.md`
- `book.toml`, `docs/SUMMARY.md`, `scripts/lint-docs.py`, `justfile`,
  `.github/workflows/ci.yml`, `.claude/skills/commit/SKILL.md`,
  `.claude/agents/developer.md`
- ADR 0003 (the Python lint precedent and the `xtask` item)
- mdBook 0.5.4: <https://github.com/rust-lang/mdBook>; `taiki-e/install-action`:
  <https://github.com/taiki-e/install-action>; lychee: <https://github.com/lycheeverse/lychee>
- AppShapes Trello card 75, "Setup repo and SDLC"
