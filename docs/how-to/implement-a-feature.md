# Implement a feature, from work item to commit

A how-to page: one task, a feature from a work item on the Brigade Board to its commits
on `master`, for a person with write access or for the agent. The rules are in
`CLAUDE.md` and the gates enforce them; this page is the order of work. Why the first features are the ones they
are is [ADR 0007](../decisions/0007-build-ipc-pub-sub-first.md). The last section is a
feature that followed this page, commit by commit.

## Prerequisites

- A work item on the team's Brigade Board, tagged `cavalry`, one per feature. A work
  item is the board's unit of work, with a number, a title, a description and a list.
  Cavalry's work moved there from Trello on 2026-11-13
  ([ADR 0113](../decisions/0113-track-cavalrys-work-on-the-brigade-board-and-map-the-trello-cards-it-carried.md)).
  `/next-task` says which feature is next and whether it has a work item.
  `brigade work-item add --tag cavalry`, with the title, a blank line and the
  description on stdin, makes one. Its number prefixes every commit of the feature.
  `/work-item-start <number>` takes a work item through every step below.
- The four gates pass on a clean `master`: `just lint`, `just test`, `just docs` and
  `just lint-docs`. `just docs` needs `mdbook` 0.5.4 (`brew install mdbook`), and the
  scripts need Python 3. The last line of `just lint-docs` ends with `ok`.
- `CLAUDE.md`, read once. For the crate you will touch: its page of `spec/` first, then
  its `README.md`, then its code.

## Steps

1. **Write the work item as the brief.** The title is the commit subject: imperative, under
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
      commit message. A decision that still waits, on a prototype not yet measured,
      on another session or on the owner's own hand, is not written as a file. Put it
      on the Brigade Board as a work item tagged `decision` and `cavalry`, with the
      question, the options and the evidence in the template's sections. When the
      ruling is the owner's, ask for it with `brigade decision --to <the owner's
      roster label> --card <work item>`, the question on stdin in a quoted heredoc
      (`<<'EOF'`); Brigade refuses an ask without `--to`. Stop here until it is
      ruled. The owner answers where they choose, on Slack, by mail or in a
      conversation, and Brigade copies the answer nowhere: the session that receives
      it records the ruling on the work item with `brigade work-item comment <work
      item>`, the owner's words on stdin. The asking session, or another session of
      the member who asked, then closes the ask with `brigade decision --close <the
      ask's id>`, the ruling as its outcome on stdin in a quoted heredoc; a session
      of another member that receives the answer records it and tells the asker.
      `just lint-docs` refuses an ADR
      whose Status is Proposed, and a change to `spec/` without an Accepted ADR in the
      same commit. Name the work item in the ADR's References (ADR 0119).
   2. Write the `spec/` page and its vectors as [`spec/README.md`](../../spec/README.md)
      says, and the one-line page under `docs/reference/` that includes it into the
      book, listed in `docs/SUMMARY.md`. Add a row to `docs/glossary.md` for every new
      term and the change's changelog entry, a file of its own under `changelog.d/`
      ([how](../../changelog.d/README.md)).
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
   3. Add a counter to `counters.toml` for every state transition, and register a state
      machine there as a `[[state_machine]]` with its `[[transition]]` rows, with the
      comment `// state machine: <name>` on its enum, so that `just lint-counters` can
      refuse a transition with no counter. An enum named `State`, `*State`, `*Status` or
      `*Phase` that is not a state machine carries `// not a state machine: <why>`
      instead; `just lint-counters` fails on one with neither. If `counters.toml`
      does not exist yet, the counters file feature of ADR 0007 is still open, and a
      feature with a state transition waits for it.
   4. Tests: the full case before the happy path; one test per `spec/` vector; a
      no-panic test over generated input for any parser.
   5. Docs in the same commit: the README example and its reading list, a how-to or
      tutorial page when a user does something new, glossary rows, a changelog entry
      under `changelog.d/`. Reference pages are generated, never written by hand.
   6. If a gate prints `not built yet` and would check this change, building it is
      part of this commit. ADR 0007 names which feature builds which gate.
   7. One crate per commit; data plane and control plane in separate commits. Under a
      data-plane crate, run `just bench` before and after and put both histograms in
      the message; while `just bench` prints `not built yet`, write that instead.
   8. One pull request per crate and per plane as well, when the work goes through the
      review loop, and none over about a thousand changed lines, generated and recorded
      files aside. The reviewer reads a pull request whole and the fixer applies every
      blocker of a review in one run, so the blocker count of a first review must fit
      one run. Card 95's driver change went as one pull request of about 3,000 lines
      (82); its first review found seven blockers, and the fixer applied all seven and
      ended without committing them. The other rounds that day found five or fewer, on
      pull requests under 1,600 lines. Split by crate, then by command or by file within
      a crate, so that each pull request passes the four gates on its own; the rest
      waits on its own branch until the first merges.

4. **Run the gates and commit.**
   - First, on a macOS laptop, where the gates compile macOS alone: a change that adds
     or touches a `cfg(target_os = "linux")` block, or its `not` twin, is type-checked
     for Linux with `cargo check --workspace --all-targets --target
     x86_64-unknown-linux-gnu`, after `rustup target add x86_64-unknown-linux-gnu`
     once. It prints `Finished` and exits 0, or the errors `gates (linux)` and both
     bench hosts would find; a check needs no linker.
   - A `cfg(target_os)` ladder with more than two arms is checked on a third target as
     well, `cargo check -p <crate> --target x86_64-unknown-freebsd`, since on Linux and
     macOS exactly one arm is live and an arm that is dead or doubled there shows only
     elsewhere.
   - A person: `/commit <work item>` runs the four gates, then `git add`, `git commit`,
     `git pull --no-edit` and `git push` to `master`. The message is the review record.
     Subject: `<work item>: <Imperative subject>`. Body: what changed, which invariant by
     number, and the evidence: the gate output, the ADR number, both histograms or the
     words `just bench` is not built yet. Merges only, never a rebase.
   - The agent: open an issue with the work item's title and body and label it `claude`;
     [Ask the agent to make a change](ask-the-agent.md) says what happens next.
   - A Claude Code session on a branch: `/pr <work item>` runs the four gates one command
     at a time, commits, merges `master` in, pushes and opens the pull request with the
     review record as its body and the `auto-merge` label, then follows the review
     loop from the author's side; `/pr watch <number>` follows one that is open. The
     pull request's title and body become the commit on `master`.
   - When the feature's last commit is on `master`, comment the commit hashes on the
     work item (`brigade work-item comment <number>`, the text on stdin) and move it to
     Done (`brigade work-item move <number> done`). Do not close it: a finished work
     item stays in Done, and closing it is the owner's word. Then run `just tidy`: it
     retires every branch whose pull request merged, locally and on origin, with its worktree; `just tidy <branch>`
     retires a working branch that never needed a pull request. It matches a merged
     pull request to what is here by the head's name, and by the upstream of a local
     branch that was pushed under another name, `git push -u origin <local>:<head>`.
     It prints a line for each thing it removed, and for each thing it left with the
     reason: a worktree with uncommitted changes; a branch under another name that
     was made after its pull request was opened, which is a branch started from that
     head, or that holds a commit its pull request did not; a branch on origin that
     was pushed to again after its pull request merged; and a branch whose upstream
     is gone and that no merged pull request accounts for. `just tidy <branch>`
     refuses a branch that has an open pull request, under its own name or the name
     it was pushed to, and refuses when `gh` cannot say whether it has one, not
     installed or signed out: install it or run `gh auth login`, then run it again.

## How to verify it worked

- `git log --oneline --grep '^<work item>:' origin/master` lists the feature's commits,
  each with the prefix.
- `gh run list --branch master --workflow ci --commit <full sha>` shows the `ci` run
  green, with `gates (linux)` and `gates (macos)` both passed. `gates (macos)` runs for
  `master`'s commit and not for the pull request, so this is where a change that
  breaks macOS alone shows: fix it forward in the same session.
- If that run reads `cancelled` and `gh run view <run id>` lists no jobs, nothing
  failed: another commit landed on `master` while your run waited. Runs on `master`
  go one at a time with one waiting, and a newer push takes the waiting run's place
  ([Run the bench](run-the-bench.md), step 3, "When the bench jobs run"). Read the
  next run on `master` that started instead. In `gh run list --branch master
  --workflow ci --limit 5` it is the oldest run newer than yours that does not read
  `cancelled`. Its commit is later on `master`, so its tree holds yours. When it is
  green with both gates passed, `master` is green with your change in it, and that is
  the proof for this step. Name that run on the work item with the commits it covers,
  because a run that did not happen has no link.
- `just docs`, then open `target/book/index.html`: the new ADR and pages are in the
  book.
- `brigade work-item show <number>` shows the work item in Done, with the commit hashes
  in its last comment and, for a commit whose own run was replaced, the run that
  covered it.

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

Trello card 80, "Adopt Aeron's data frame header", is the first feature that followed
this page. A feature was a Trello card then; it is a work item now (ADR 0113). It was
chosen because it is the smallest spec-first change in the first slice (ADR 0007).
`git log --oneline --grep '^80:'` lists its commits.

| Step | What Trello card 80 produced | Commit |
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
