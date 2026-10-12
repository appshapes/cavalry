# Implement a feature, from card to commit

A how-to page: one task, a feature from a Trello card to its commits on `master`, for a
person with write access or for the agent. The rules are in `CLAUDE.md` and the gates
enforce them; this page is the order of work. Why the first features are the ones they
are is [ADR 0007](../decisions/0007-build-ipc-pub-sub-first.md). The last section is a
feature that followed this page, commit by commit.

## Prerequisites

- A card on the Cavalry list of the AppShapes Trello board, one per feature.
  `/next-task` says which feature is next and whether it has a card, and
  `/trello-create` makes one. Its number prefixes every commit of the feature.
- The four gates pass on a clean `master`: `just lint`, `just test`, `just docs` and
  `just lint-docs`. `just docs` needs `mdbook` 0.5.4 (`brew install mdbook`), and the
  scripts need Python 3. The last line of `just lint-docs` ends with `ok`.
- `CLAUDE.md`, read once. For the crate you will touch: its page of `spec/` first, then
  its `README.md`, then its code.

## Steps

1. **Write the card as the brief.** The title is the commit subject: imperative, under
   72 characters. The body says what changes, what done looks like, and which invariant
   it serves, by number. When the agent will do the work, the same text is the issue
   body of [Ask the agent to make a change](ask-the-agent.md); the agent reads nothing
   else.

2. **Decide and specify, before any code.** Only when the feature changes the wire, a
   shared-memory layout, the on-disk log, the C ABI, a data-plane dependency or a
   configuration key. Otherwise go to step 3.
   1. Copy `docs/decisions/0000-template.md` to the next number and fill every section,
      the alternatives that lost included. Set Status to Accepted: an ADR is accepted
      on the evidence that informed it, and what the implementation proves goes in the
      commit message. If the decision still needs a prototype, leave it Proposed and
      stop here: `just lint-docs` refuses a change to `spec/` without an Accepted ADR
      in the same commit.
   2. Write the `spec/` page and its vectors as [`spec/README.md`](../../spec/README.md)
      says, and the one-line page under `docs/reference/` that includes it into the
      book, listed in `docs/SUMMARY.md`. Add a row to `docs/glossary.md` for every new
      term and a line under Unreleased in `CHANGELOG.md`.
   3. Commit (step 4), with the decision as the subject: `80: Adopt Aeron's data frame
      header`.

3. **Write the example, then the code.**
   1. Write the crate README's Example section first: a complete program of about
      twenty lines that calls the new API. It is the crate's doctest and `just docs`
      runs it, so it fails until the API exists. The API is designed from this side.
   2. Make the example compile, then implement. Under a data-plane crate,
      `.claude/rules/data-plane.md` is the per-message checklist. `unsafe` only in
      `cavalry-shm` and `cavalry-ffi`, every block with a `// SAFETY:` comment and a
      `loom` test in the same commit.
   3. Add a counter to `counters.toml` for every state transition. If `counters.toml`
      does not exist yet, the counters file feature of ADR 0007 is still open, and a
      feature with a state transition waits for it.
   4. Tests: the full case before the happy path; one test per `spec/` vector; a
      no-panic test over generated input for any parser.
   5. Docs in the same commit: the README example and its reading list, a how-to or
      tutorial page when a user does something new, glossary rows, a `CHANGELOG.md`
      line. Reference pages are generated, never written by hand.
   6. If a gate prints `not built yet` and would check this change, building it is
      part of this commit. ADR 0007 names which feature builds which gate.
   7. One crate per commit; data plane and control plane in separate commits. Under a
      data-plane crate, run `just bench` before and after and put both histograms in
      the message; while `just bench` prints `not built yet`, write that instead.

4. **Run the gates and commit.**
   - A person: `/commit <card>` runs the four gates, then `git add`, `git commit`,
     `git pull --no-edit` and `git push` to `master`. The message is the review record.
     Subject: `<card>: <Imperative subject>`. Body: what changed, which invariant by
     number, and the evidence: the gate output, the ADR number, both histograms or the
     words `just bench` is not built yet. Merges only, never a rebase.
   - The agent: open an issue with the card's title and body and label it `claude`;
     [Ask the agent to make a change](ask-the-agent.md) says what happens next.
   - When the feature's last commit is on `master`, comment the commit hashes on the
     card and archive it, and run `just tidy`: it retires every branch whose pull
     request merged, locally and on origin, with its worktree; `just tidy <branch>`
     retires a working branch that never needed a pull request.

## How to verify it worked

- `git log --oneline --grep '^<card>:' origin/master` lists the feature's commits,
  each with the prefix.
- `gh run list --commit <full sha>` shows the `ci` run green, with `gates (linux)` and
  `gates (macos)` both passed.
- `just docs`, then open `target/book/index.html`: the new ADR and pages are in the
  book.
- The card is archived with the commit hashes in its last comment.

## When it does not work

- `just lint-docs` says `spec/ changed with no ADR in the same change whose Status is
  Accepted`: step 2.1.
- `just lint-docs` says `changed with no change under docs/ or to a README`: a public
  item, counter, configuration key or `spec/` page needs its documentation in the same
  commit (step 3.5).
- `just lint-docs` says `was Accepted at ... and changed beyond its Status line`:
  accepted ADRs are immutable. Write a new one that supersedes it.
- `just docs` fails on the README example: the example is the doctest. Fix the code or
  the example, never mark the block `ignore`.
- `just lint` reports a dependency that is not on the allowlist: a data-plane crate's
  dependencies are listed in its `Cargo.toml` metadata, and a new one needs the entry,
  a justification in the commit message and an ADR (step 2).
- A gate prints `not built yet`: step 3.6.

## The worked example

Card 80, "Adopt Aeron's data frame header", is the first feature that followed this
page. It was chosen because it is the smallest spec-first change in the first slice
(ADR 0007). `git log --oneline --grep '^80:'` lists its commits.

| Step | What card 80 produced | Commit |
| --- | --- | --- |
| 1 | The card: the title as the commit subject, what changes, what done looks like, invariants 1, 2 and 6. | |
| 2 | [`spec/frame.md`](../../spec/frame.md), [`spec/vectors/frame.py`](../../spec/vectors/frame.py) and the 18 vectors it writes, [`docs/reference/frame.md`](../reference/frame.md) and its line in the summary, four glossary rows, [ADR 0008](../decisions/0008-adopt-aeron-data-frame-header.md) for the layout, [ADR 0009](../decisions/0009-read-wire-structs-with-zerocopy.md) for the dependency, two changelog lines. The first spec page also wrote [`spec/README.md`](../../spec/README.md), once. | `d3651ec` |
| 3 | The [README example](../../crates/cavalry-proto/README.md) written first; then `crates/cavalry-proto/src/frame.rs`, `zerocopy` on the allowlist, `tests/conformance.rs` over the vectors, `tests/no_panic.rs`, a cargo-fuzz target under `crates/cavalry-proto/fuzz/`, the `conformance` and `fuzz` recipes in the justfile, a glossary row, a changelog line. | `884bef4` |
| 3, once | What the crate's first dependency triggered: the `cargo` ecosystem in `.github/dependabot.yml` and a line in the reviewer's brief. The next dependency triggers nothing. | `c68ad4d` |
| 4 | Three commits on `master` through `/commit 80`, CI green on each, the card archived with the hashes. | |

The first feature in an area pays for the gates that area needs, and the next does not:
the conventions page and the reference include came with the first spec page, and the
conformance test, the no-panic test and the fuzz target came with the first parser.
Two lessons from this one, so the next contributor does not relearn them. `cargo fuzz
init` writes a manifest whose dependency, path and workspace table needed fixing by
hand; copy `crates/cavalry-proto/fuzz/Cargo.toml` instead of running it. And the
vectors are written by a script in another language, so a hex string typed by hand
never enters the repository.
