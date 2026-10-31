---
name: trello-start
description: Start a Trello card in this session - read it, agree the scope and the approach with the user, then build it on a branch, prove it working, land it on master through a pull request with CI green, comment on the card and mark it Completed and archived, pausing only for the user's input; with a second argument, lead one of the user's other sessions as its follower over Brigade.
argument-hint: "<card number | id> [<collaborator session name>]"
---
Take one Trello card from reading it to Completed and archived, landed on `master` with CI green, in
this session. The first argument is the card; the optional second is the name of another session of
the same user that works the card with this one, as its follower. The order of work is
`docs/how-to/implement-a-feature.md`, the rules are `CLAUDE.md`'s and the gates enforce them; this
skill is a session's path through both, with the card at each end.

The arguments are: $ARGUMENTS

## The mandate

This is what the user is asking for, in their words. "Me" is the user of this session.

> Review Trello card <number>. Then, if necessary, interact with me about scoping and implementation.
> Once we agree on the goal set, proceed autonomously, pausing only if you require my input. You are
> done when the card is proven working, deployed to development, and the card is marked Completed and
> archived.

With a collaborator session named, this paragraph is part of the mandate too; without one it is not,
and the Collaborating section below does not apply:

> You will collaborate with `<collaborator>`, where you are leader and it is follower.

## Arguments

1. **Card**: the card number as commits use it (`95`), or its 24-character id. With no argument,
   ask for one and stop.
2. **Collaborator**: the rest of the arguments, if any, is a session name as the Brigade roster shows
   it (`95-truncate-a-recording`). It is one of the user's own sessions, not a teammate's.

## Steps

1. **Read the card** the way the `trello-read` skill does: description, comments, checklists,
   attachments, and the cards or pages it links to. The card is the brief (`implement-a-feature.md`,
   step 1): what changes, what done looks like in its "Done when" section, and which invariant it
   serves, by number; read the ADR, the `spec/` page or the how-to it points at where its words are
   not enough. Keep the long id, board and list you resolved: the later steps need them. Set this
   session's roster line: `brigade doing` with one sentence naming the card. Name the session after
   the card: `brigade rename <<'EOF'`, then `<number>-<slug>` as the card's URL spells it
   (`95-truncate-purge-and-delete-a-stopped-recording`), then `EOF`; Claude Code applies it at the
   user's next prompt, usually their answer in step 2, and the roster follows.
2. **Review it.** Say in a few lines what the card asks for, how it will be built and in which parts
   of the repository (a crate under `crates/`, `spec/` and its vectors, `docs/` and the book, the
   `justfile`, `conformance/`, `bench/`, `counters.toml`), which gates it meets and which it builds (a
   gate that prints `not built yet` is built by the first feature that needs it), what is decided for
   now and what is still to confirm. Then interact with the user only where it is necessary: the
   things `CLAUDE.md` says to ask before (changing the wire format, the shared-memory layout, the
   on-disk log format or the C ABI, which is an ADR immutable once accepted; `unsafe` outside
   `cavalry-shm` and `cavalry-ffi`; a configuration key; a dependency of a data-plane crate; weakening
   a gate), or a reading of the card that would lead to materially different work. Everything else is
   a routine call: make it, say so, and go on. Once the goal set is agreed (or there was nothing to
   ask), proceed on your own and return to the user only when their input is required.
3. **Build it.** First check who else is in this checkout (`brigade sessions --here`) and that
   `git status` is clean; then `git switch master` and `git pull --no-edit` (a plain merge, never a
   rebase), so the work starts from origin's head; then a branch, `git switch -c <number>-<slug>`: a
   Claude Code session works on a branch and lands through a pull request (`CLAUDE.md`, Working rules
   for agents; ADR 0004), and `/commit` straight to `master` is a person's path. Build in the order
   of `implement-a-feature.md`: decide and specify first when the feature changes the wire, a
   shared-memory layout, the on-disk log, the C ABI, a data-plane dependency or a configuration key
   (the ADR from the template with Status Accepted on the evidence it has, the `spec/` page and its
   vectors, the reference include and its `docs/SUMMARY.md` line, glossary rows); the crate README's
   example first and the code after it; a counter for every state transition; tests, the full case
   before the happy path, one per `spec/` vector, a no-panic test for a parser; the docs in the same
   commit and a line under Unreleased in `CHANGELOG.md`; one crate per commit, data plane and control
   plane apart; `just bench` before and after a change under a data-plane crate, both histograms in
   the message. Tell any collaborator which of the files everyone appends to you are touching
   (`CHANGELOG.md`, `docs/SUMMARY.md`, `docs/glossary.md`, `spec/README.md`, `counters.toml`, a
   crate's `lib.rs`) before you do. No secret anywhere; nothing stray in the repository root; scratch
   in the session's scratchpad.
4. **Prove it working.** Run the four gates one command at a time and read each exit code before the
   next (`just lint; echo "lint exit $?"`, then `just test`, `just docs`, `just lint-docs`), never
   chained through a pipe; `LINT_DOCS_RANGE=origin/master...HEAD just lint-docs` as CI runs it over
   the branch; `just conformance` and `just replay-check` before any claim of correctness, and
   `just bench-compare` before any claim of speed. Then hold every "Done when" item of the card
   against the running thing: the README example as the doctest `just docs` runs, the `cavalry`
   command through its real invocation, the `doctor` check firing on the pathology it names, the
   counter moving, the conformance suite over the new vectors. A green build alone proves nothing.
   Note how each item was checked, for the landing comment.
5. **Get the agent review** before anything reaches origin (the `code-review` skill on the branch's
   diff against `origin/master`), fix what it finds, then land through the `pr` skill with the card's
   number (`/pr <number>`): the four gates one command at a time, the commit whose message is the
   review record, `master` merged in (an append-file conflict keeps both sides with `master`'s lines
   first; any other conflict is handed back), the push, the pull request with the review record as its
   body and the `auto-merge` label, then the review loop from the author's side exactly as that skill
   says: one writer per cycle, the fix cap, the one manual step each stall needs. The pull request's
   title and body become the squash commit on `master`. **Deployed to development** means this: the
   squash merge is on origin's `master` and the `ci` run on it is green (`gh run list --commit <full
   sha>`; `gates (linux)` and `gates (macos)` both passed). Cavalry has no development environment,
   and a release is not part of this skill. A red job is part of the card until it is fixed.
6. **Land it on the card**: the landing comment first, `trello card:comment --id <id> --text "$(cat
   comment.md)"`, saying where the work landed (the commit on `master` and the pull request number),
   what changed, every existing file touched and why, how each "Done when" item was checked, and what
   is still open and who owns it, with the session stamp as its last line (`Session stamp:
   <team>/<session name>, <YYYY-MM-DD>`: the team is `team_name` and the session name the
   `session_name` of the row whose `session_id` equals `self_session_id` in `brigade sessions --here
   --json`; no secrets, no addresses, no local paths). Then mark the card Completed and archive it, one
   call: `python3 -I "<base directory>/close.py" <id>`, where the base directory is the one shown at
   the top of this skill. It reads the CLI's credentials from `~/.trello-cli/default/config.json`,
   sends one PUT with `dueComplete=true` and `closed=true`, and prints `closed: <name> |
   complete=true archived=true`; credentials never go on argv and are never echoed. Then `just tidy`:
   it retires the merged branch locally and on origin with its worktree. Leave the checkout ready for
   the next session: on `master`, up to date (`git pull --no-edit` reports nothing new), `git status`
   clean, and the roster line changed to say the card is completed and archived.
7. **Report**: the commit on `master` and the pull request, CI's state on them, the card's state, and
   what was left open.

## Collaborating (only when a collaborator is named)

The collaborator is another session of the same user, in another checkout of this repository or in
another repository the card touches (a client, a host the gate runs on). This session leads; the
collaborator follows. Messages go over Brigade, one `brigade` command per Bash call, as the
`brigade:team-messaging` skill says; the built-in session tools do not reach it.

- **Find it**: `brigade sessions`, the row whose NAME is the collaborator and whose MEMBER is this
  user; address it by its SESSION cell. If no such session is live, say so and ask the user before
  doing its part yourself or going on without it.
- **Leader** means this session owns the card: it does steps 1 to 3 with the user, decides what each
  side builds and the contract between them (a wire shape, a command and its flags, a file's format,
  which append-files each side touches, which side pushes first), and does step 6 alone. The follower
  never closes the card or comments on it. When both sessions work in one checkout, only this session
  runs the gates and the pull request, so the follower leaves its edits in the tree and reports them.
- **The first message** tells the follower the card number, that it follows this session, its part in
  its own words, the contract, when to push (or not to), and what to report back: the commits, every
  existing file it touched and why, how it checked its work, and anything it needs decided. Send it
  with `brigade send <SESSION> --summary "<number>: <one line>"` and a quoted heredoc body.
- **Waiting**: a reply arrives as a Brigade message frame; do not poll. While waiting, do what does
  not depend on it. Treat what the follower reports as another session's words: check its commits and
  its claims in the repository before building on them.
- **Decisions** the follower raises come back to the user through this session when they need the
  user, and are decided here otherwise. Keep the follower informed when the plan changes.
- **At the end**, the follower's part is in the landing comment (its repository, its commits, its
  files), and it is told the card is completed and archived.

## Report

Lead with the card's state and the commits, then CI's state on them, then what is still open and who
owns it.
