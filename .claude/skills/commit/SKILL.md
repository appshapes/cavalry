---
name: commit
description: Commit, pull and push the working tree to master after just lint, just test, just docs and just lint-docs, with a commit message that is the review record
argument-hint: "[card number]"
---

Commit the current working tree to `master` with this repository's commit sequence.
Developers commit directly to `master`; only bots and agent workflows use branches and
pull requests (CLAUDE.md, "Working rules for agents"). `master` is green after every
commit, and the commit message is the review record: what changed, which invariant it
serves, and the evidence.

The sequence is the `gcp` script (`git add`, `git commit`, `git pull --no-edit`,
`git push`) with the gates CLAUDE.md and `.claude/rules/docs.md` require in front of it:

```
just lint
just test
just docs
just lint-docs
git add --verbose :/ .
git commit -m "<message>"
git pull --no-edit
git push
```

Merges only. Never `rebase`, never `--force`. On a merge conflict, stop and hand it
back: no `git merge --abort`, no rebase, no force-push.

The optional Trello card number is: $ARGUMENTS

## 1. Analyze changes

Run these in parallel:

- `git status` (never use `-uall`)
- `git diff`
- `git diff --cached`
- `git log --oneline -5`

If the working tree is clean (no staged, unstaged or untracked changes), say there is
nothing to commit and stop.

`git add --verbose :/ .` stages everything in the working tree, so ALL changes will be
included, not just staged files. If something present should not go in (a scratch
file, a local-only config, a second change that belongs in its own commit), say so and
stop; the user removes it or asks for a split.

## 2. Check the change against CLAUDE.md

Check each of these against the diff. A miss is a reason to stop and say so, not to
commit and fix later.

- One crate per commit where possible. Data-plane and control-plane changes go in
  separate commits. If the diff mixes them, propose the split.
- Docs ship in the same commit as the code they describe. A new or changed public
  item, counter, config key or `spec/` file needs a matching change under `docs/` or a
  README. A user-visible change needs a line under `Unreleased` in `CHANGELOG.md`. A
  message that says "docs to follow" gets reverted.
- A change under a data-plane crate needs `just bench` before and after, and both
  histograms go in the commit message. A performance claim without a histogram is not
  a claim.
- A new dependency in a data-plane crate needs a justification in the commit message.
- A change to `spec/`, the on-disk log format or the C ABI needs an accepted ADR in
  `docs/decisions/`.

## 3. Check for secrets

Scan every added line (`+` prefix) in the diff output, in ALL files, not just code
files, for anything that looks like a secret: API keys, passwords, tokens, connection
strings, private keys or credentials. Non-code files (markdown, prompts, config notes)
are just as likely to contain secrets. If found, warn the user and do not proceed until
they confirm the files are safe.

## 4. Run the gates

Run `just lint`, `just test`, `just docs`, then `just lint-docs`. All four must pass
before the commit. `just lint-docs` on a dirty tree checks the change about to be
committed; on a clean tree it checks the last commit.

- A recipe that prints `not built yet` is a gate that does not exist yet, not a pass.
  If the change touches what that gate would check, building the gate is part of the
  change (CLAUDE.md, "Gates"): stop and name the gate. If the change cannot touch it
  (an ADR, a skill, the changelog), note the unbuilt gate in the report and continue.
- If `cargo` fails because there is no workspace yet (no `Cargo.toml` at the root),
  the gate cannot run; say so in the report and continue. Once the workspace exists, a
  failing gate blocks the commit.

## 5. Write the commit message

- Subject: imperative mood, capitalized, under 72 characters, the intent rather than a
  list of files. When a card number was given, prefix it: `<card>: <Subject>`. The
  number is the AppShapes Trello card the work belongs to (see `/trello-read`).
- Body: the review record. What changed. Which invariant it serves, by number. The
  evidence: the gate results, both histograms for a data-plane change, the ADR number,
  and the `just conformance` and `just replay-check` results when the change claims
  correctness.
- Wrap the body at 72 characters. Backticks and quotes are safe inside the quoted
  heredoc below, so use the real names of files, flags and crates.

## 6. Confirm with the user

Show the subject, the body and the exact sequence that will run. Ask the user to
confirm or to edit the message. This pushes to `master`; never auto-execute.

## 7. Execute

Once confirmed, run exactly:

```
git add --verbose :/ .
git commit -m "$(cat <<'EOF'
<subject>

<body>
EOF
)"
git pull --no-edit
git push
```

If `git pull --no-edit` created a merge commit, run the four gates again
before `git push`. If either fails, stop and report; do not push.

## 8. Report the result

- On success, give the commit hash and subject and confirm it was pushed.
- On failure (a gate, a merge conflict, a rejected push), report the error output
  clearly. Do not retry automatically. On a merge conflict, hand it back as it is.
