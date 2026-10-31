---
name: trello-start
description: Start a Trello card in this session - read it, agree the scope with the user, then work it on a branch as docs/how-to/implement-a-feature.md says, land it on master through /pr with CI green, comment on the card and mark it Completed and archived, pausing only for the user's input; with a second argument, lead one of the user's other sessions as its follower over Brigade.
argument-hint: "<card number | id> [<collaborator session name>]"
---
Take one Trello card from reading it to Completed and archived, landed on `master` with CI green, in
this session. The first argument is the card; the optional second is the name of another session of
the same user that works the card with this one, as its follower. This skill is the card's
bookkeeping around the path cavalry already fixes: the order of work is
`docs/how-to/implement-a-feature.md`, the rules are `CLAUDE.md`'s and the gates enforce them, and
the landing is the `pr` skill's. Nothing here restates them, so nothing here can contradict them.

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
   attachments, and the ADR, `spec/` page or how-to it points at. The card is the brief
   (`implement-a-feature.md`, step 1): what changes, what done looks like in its "Done when" section,
   and which invariant it serves, by number. Keep the long id you resolved: the last step needs it.
   Set this session's roster line: `brigade doing` with one sentence naming the card. Name the session
   after the card: `brigade rename <<'EOF'`, then `<number>-<slug>` as the card's URL spells it
   (`95-truncate-purge-and-delete-a-stopped-recording`), then `EOF`; Claude Code applies it at the
   user's next prompt, usually their answer in step 2, and the roster follows.
2. **Review it.** Say in a few lines what the card asks for, where it lands (a crate, `spec/`, the
   book, a gate it builds because that gate prints `not built yet`), what is decided for now and what
   is still to confirm. Then interact with the user only where it is necessary: the things `CLAUDE.md`
   says to ask before (the wire format, a shared-memory layout, the on-disk log format or the C ABI;
   `unsafe` outside `cavalry-shm` and `cavalry-ffi`; a configuration key; a dependency of a data-plane
   crate; weakening a gate), or a reading of the card that would lead to materially different work.
   Everything else is a routine call: make it, say so, and go on. Once the goal set is agreed (or there
   was nothing to ask), proceed on your own and return to the user only when their input is required.
3. **Work the card** as `docs/how-to/implement-a-feature.md` says, on a branch. First check who else is
   in this checkout (`brigade sessions --here`) and that `git status` is clean; then `git switch
   master`, `git pull --no-edit` (a plain merge, never a rebase) and `git switch -c <number>-<slug>`: a
   Claude Code session works on a branch and lands through a pull request (`CLAUDE.md`, Working rules
   for agents; ADR 0004), and `/commit` straight to `master` is a person's path. The how-to's steps 2
   and 3 are the build, and its "How to verify it worked" is the proof, held against every "Done when"
   item of the card on the running thing: a green gate alone proves nothing. Note how each item was
   checked, for the landing comment. Get the agent review before anything reaches origin (the
   `code-review` skill on the branch's diff against `origin/master`) and fix what it finds.
4. **Land it** with the `pr` skill and the card's number (`/pr <number>`) and follow it to the squash
   merge exactly as that skill says: every wait there is the loop's own work in progress, and the one
   manual step each stall needs is written there. **Deployed to development** means this: the squash
   merge is on origin's `master` and the `ci` run on it is green (`gh run list --commit <full sha>`).
   Cavalry has no development environment, and a release is not part of this skill. A red job is part
   of the card until it is fixed.
5. **Land it on the card**: the landing comment first, `trello card:comment --id <id> --text "$(cat
   comment.md)"`, saying where the work landed (the commit on `master` and the pull request number),
   what changed, every existing file touched and why, how each "Done when" item was checked, and what
   is still open and who owns it, with the session stamp as its last line (`Session stamp:
   <team>/<session name>, <YYYY-MM-DD>`: the team is `team_name` and the session name the
   `session_name` of the row whose `session_id` equals `self_session_id` in `brigade sessions --here
   --json`; no secrets, no addresses, no local paths). Then mark the card Completed and archive it, one
   call: `python3 -I "<base directory>/close.py" <id>`, where the base directory is the one shown at
   the top of this skill. It reads the CLI's credentials from `~/.trello-cli/default/config.json`,
   sends one PUT with `dueComplete=true` and `closed=true`, and prints `closed: <name> |
   complete=true archived=true`; credentials never go on argv and are never echoed. Then `just tidy`.
   Leave the checkout ready for the next session: on `master`, up to date, `git status` clean, and the
   roster line changed to say the card is completed and archived.
6. **Report**: the commit on `master` and the pull request, CI's state on them, the card's state, and
   what was left open.

## Collaborating (only when a collaborator is named)

The collaborator is another session of the same user, in another checkout of this repository or in
another repository the card touches. This session leads; the collaborator follows. Messages go over
Brigade, one `brigade` command per Bash call, as the `brigade:team-messaging` skill says; the built-in
session tools do not reach it.

- **Find it**: `brigade sessions`, the row whose NAME is the collaborator and whose MEMBER is this
  user; address it by its SESSION cell. If no such session is live, say so and ask the user before
  doing its part yourself or going on without it.
- **Leader** means this session owns the card: it does steps 1 and 2 with the user, decides what each
  side builds and the contract between them (a wire shape, a command and its flags, which of the
  files everyone appends to each side touches, which side pushes first), and does step 5 alone. The
  follower never closes the card or comments on it. When both sessions work in one checkout, only this
  session runs the gates and the pull request, so the follower leaves its edits in the tree and
  reports them.
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
