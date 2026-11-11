# 0076. Let the changelog grow by a fold's lines and a correction's alone

- **Status:** Accepted
- **Date:** 2026-11-11
- **Invariants affected:** 6
- **Supersedes:** ADR 0075 on two points: what `scripts/lint-docs.py` passes when
  `CHANGELOG.md` gains an entry line, and the order `scripts/changelog.py fold` puts
  entries in. The rest of ADR 0075 stands: an entry is a file of its own under
  `changelog.d/`, and a release's fold moves it.

## Context

ADR 0075 keeps a change's changelog entry out of `CHANGELOG.md` with a gate: `just
lint-docs` "fails a change in which `CHANGELOG.md` gains an entry line and no entry
file is removed". The review of its pull request, 195, found that looser than its
purpose in both directions and left it for a later change:

- It passed too much. A change that removed any file under `changelog.d/`, its README
  included, could append any line to `CHANGELOG.md`.
- It refused a correction. A typo fixed in one of the two thousand lines
  `CHANGELOG.md` already has shows as an entry line added, and the message told the
  author to move the line into a file, which is not what a correction needs.

The same review noted that `fold` ordered entries by the date of the commit that
landed each, which a commit made with a wrong clock breaks, where ADR 0075 says "in
the order they landed on the branch's first-parent history".

Three attempts at the gate failed their pre-push reviews, and this decision is what
they taught. The first matched the lines a diff added against the lines it removed
from entry files: a fold that had been staged did not pass, a correction could not
land in any change, and the gate had no test. The second counted, over the whole
file, one added line for each line taken out: an entry deleted in one place excused
one appended in another, and a branch behind `master` was read against `master`'s
newer lines. The third read an entry's text from the commits inside a change, so
that a pull request could correct an entry and then fold it: CI reads a pull request
as one commit, its merge with `master`, and `master`'s run reads it as its squash,
and neither holds the commits between.

## Decision

`CHANGELOG.md` may gain an entry line, one that begins `- ` or two spaces, in two
ways, and `scripts/lint-docs.py` fails a change with the count of every other entry
line it adds. The rule is `grown` in `scripts/changelog.py`.

1. **A fold moves it out of the entry files.** The entry files under `changelog.d/`
   held that very line where the change began, and at its end they do not.
2. **A correction replaces it where it stands.** In each block of lines of
   `CHANGELOG.md` that the change rewrites, it may put in one entry line for each
   entry line it takes out. A line taken out in one place excuses none added in
   another.

The change is read from its two ends and from nothing between them: not from a
diff, and not from its commits. So the working tree, the index and a commit give one
answer, and a pull request reads the same commit by commit, as its merge with
`master`, and as the squash that lands. Its beginning is the commit where its head
left the history of the range's base, their merge base: for a pull request that has
merged `master`, and for a push, that is the base itself, and a branch behind
`master` answers for its own lines alone. A range git cannot read fails the gate
with the reason, and never passes as a change that added nothing.

It follows that the change that folds an entry neither corrects it nor adds it. An
entry is corrected in its file in a change that lands before the fold's, or in
`CHANGELOG.md` in a change after it.

`fold` orders entries by the place on the first-parent history of the newest commit
that added each file, not by that commit's date. It refuses, with nothing changed,
an entry file that is not as `HEAD` holds it, untracked, changed or staged: the gate
reads a fold as the lines that left committed entry files.

## Alternatives considered

- **Match the diff's added lines against the diff's removed ones.** Built first, and
  lost in its review: a diff of a path that is neither in the index nor on disk is
  empty, so a staged fold read as an append.
- **One added line for each line taken out, anywhere in the file.** Built second, and
  lost in its review: a change that deletes an old entry and appends its own passes,
  and the appended line is the one two pull requests conflict in.
- **Read an entry's text from the commits inside the change.** Built third, so that
  one pull request could correct an entry and fold it. Lost in its review: the
  branch passed, and the same tree failed as the merge commit CI checks out and as
  the squash on `master`, which is a red `master` after a green pull request.
- **Pass a correction only in a release's commit.** Lost: a correction is not what
  two pull requests conflict in. They conflict in lines appended at one place; a
  replaced line conflicts only with another change to the same line.
- **Count the lines that left the entry files, not their text.** It would pass an
  entry corrected and folded in one change. Lost: it would also pass a line appended
  beside the removal of an entry that does not hold it.
- **Ask that a correction resemble the line it replaces.** A change can overwrite
  another change's entry with its own and pass as a correction. Lost: a measure of
  likeness would fail true corrections at its edge, and the gate is there to stop a
  habit, the appended line. An entry overwritten shows in the diff as an entry
  removed, which is its reviewer's to refuse.
- **Leave the gate as ADR 0075 has it.** Lost: its exemption was any removal under
  `changelog.d/`, and its refusal of a correction had no way out.

## Evidence

- `scripts/changelog-test.py`, 42 tests where `master` has 19. The 20 of `Grown`
  build each change in a repository of their own, and most read it three ways, in
  the working tree, staged, and as a commit:
  - grows by nothing: a change that leaves `CHANGELOG.md` alone, or edits the
    directory's README; a fold; one line moved out of an entry file that stays; a
    fold whose file's name a new entry takes in the same change; a correction that
    replaces a line, alone and beside a fold; a folded line that is not ASCII, with
    the locale set to `C`;
  - counted: an appended entry, line by line; a line appended beside a fold; an entry
    file removed that does not hold the line; an entry renamed, with its line
    appended; a correction that adds a line; an entry corrected and folded in one
    change, and one added and folded in one change; a folded line corrected after the
    fold; an entry taken out below a release's heading with one appended under
    Unreleased; a line a branch appended while its base folded and gained an entry
    file, read behind the base and with the base merged in; a line under a directory
    of `changelog.d/`, which is no entry at either end;
  - the same three ways: a pull request that folds, and one that corrects an entry
    and folds it, each read as its branch, as its merge with `master` and as its
    squash, give 0 three times and 1 three times;
  - no answer, and an error that says why: a base or a head that names no commit, two
    ends that share no commit, and a commit git cannot list.
- The same file, `Fold`: three entries go in the order they landed when the second
  commit is dated a year before the first, which fails on the order by date; an entry
  no commit holds, one changed since its commit, staged or not, and a name used again
  after an earlier fold and not committed yet are each refused and left in place; a
  shallow checkout prints the one reason; a renamed entry lands under its new name.
- `python3 scripts/lint-docs.py` on this change's tree passes, and with a line
  appended to `CHANGELOG.md`, 2,100 lines, prints `change: CHANGELOG.md gained 1
  entry line(s) that are neither a fold's nor a correction's`; the same with an entry
  corrected, or three entry lines deleted, elsewhere in the file.

## Consequences

Easier: a typo in `CHANGELOG.md` can be fixed in any change, the gate gives a pull
request, its merge and its squash one answer, and it has tests that hold what it
passes. Harder: a release that wants to correct an entry it is about to fold lands
the correction first, in a change of its own, or corrects the line in
`CHANGELOG.md` after the fold has landed; and a re-wrapped entry that gains a line
is counted. The gate does not judge what a correction says. The rule lives in
`scripts/changelog.py`, which `scripts/lint-docs.py` loads, so the gate is one
function with one test file.

Revisit if a fold ever has to change lines as it moves them, a heading for each kind
of change for one: then the fold is no longer an append, and neither is this rule.

## References

- `scripts/changelog.py`, `grown`, `held`, `places` and `fold`;
  `scripts/changelog-test.py`, `Grown` and `Fold`; `scripts/lint-docs.py`,
  `check_change`.
- `changelog.d/README.md`.
- ADR 0075; pull request 195's review.
- Card 255.
