# 0075. Keep each change's changelog entry in a file of its own

- **Status:** Accepted
- **Date:** 2026-11-11
- **Invariants affected:** 6. It also serves `CLAUDE.md`'s working rule that `master`
  is green after every commit, by way of the loop that lands the commits
- **Supersedes:** none. Amends where `.claude/rules/docs.md`, "Changelog", puts an
  entry, and takes `CHANGELOG.md` off `CLAUDE.md`'s list of the files concurrent
  branches collide in. Amends ADR 0071's list of what cannot move the bench by one
  path, `changelog.d/`.

## Context

Every user-visible change adds its entry to `CHANGELOG.md` in the same commit, at the
end of the Unreleased section. Two pull requests open at once therefore both change
the last lines of one file, and the second to land conflicts with the first, whatever
else the two change. GitHub does not merge a pull request that conflicts, and starts
no CI on its head. The second then merges `master`, runs the four gates, is reviewed
again and, when its files can move the bench, takes the two gate hosts again.

On 2026-11-10 and 2026-11-11 two sessions landed pull requests in turn. Every merge
of `master` that the drain's pull requests needed was forced by that one file: three
for pull request 182, four for pull request 188, three for card 251's branch before
it was pushed, each named in its pull request body. Pull request 188 took 96 minutes
from its opening to its merge, over three rounds.

`master`'s protection does not ask a branch to be up to date (ADR 0004). Without the
shared file, two pull requests that touch different code merge in either order with
neither going round again.

The changelog has one reader beside people: `scripts/release-notes.py` gives the
weekly email's writer "the lines the window added to `CHANGELOG.md`".

## Decision

A change's changelog entry is a file of its own: `changelog.d/<number>-<a few
words>.md`, the number being the card's, or the pull request's when there is no card.
It holds the lines that would have been appended, a line that begins `- ` and its
continuation lines indented two spaces, with its links written from the repository's
root, as `CHANGELOG.md` will hold them. Nobody appends to `CHANGELOG.md` between
releases, and two gates hold that:

- `python3 scripts/changelog.py check`, which `just lint-docs` runs, fails on a file
  whose name or shape the fold could not place, and on a link that names no file.
- `scripts/lint-docs.py` fails a change in which `CHANGELOG.md` gains an entry line and
  no entry file is removed. A release's fold is the one change that adds such lines.

`python3 scripts/changelog.py fold` puts the entries at the end of `CHANGELOG.md`'s
Unreleased section, in the order they landed on the branch's first-parent history,
and removes the files: at a release, in the release's commit. `CHANGELOG.md` keeps
every line it has. `scripts/release-notes.py` reads a window's changelog lines from
both places and counts none twice across a fold. `scripts/bench-needed.py` lists
`changelog.d/` among the paths that cannot move the bench, as the root's markdown,
where `CHANGELOG.md` is, already was.

## Alternatives considered

- **A merge driver**, `CHANGELOG.md merge=union` in `.gitattributes`. Lost: it
  resolves the conflict in a local `git merge`, and the merge that matters is
  GitHub's, which decides whether a pull request can merge and whether CI starts on
  it, and which runs no merge driver.
- **A bot that folds on every push to `master`.** Lost: it commits to `master` with a
  token that passes the protection rule, adds a run of CI for every landing, and
  changes the file under every open pull request again, which is the problem.
- **GitHub's merge queue.** Lost for this: a queue rebuilds a pull request on what
  landed before it, and a real conflict still stops it. It answers a different
  question, and ADR 0074 has it as the answer if conflicts between unrelated pull
  requests come back.
- **Generate `CHANGELOG.md` from commit messages.** Lost: an entry is written for the
  reader of a release, a commit message for the reviewer of a change
  (`.claude/rules/docs.md`), and one squash can carry two entries or none.
- **Leave it, and land pull requests one at a time.** Lost: that is the night of
  2026-11-10, with the second session waiting on the first.
- **A front matter in each file** for a heading or a kind. Lost for now:
  `CHANGELOG.md` has one heading under Unreleased, and an entry's own first words say
  what kind of change it is. A file is the lines themselves, so the fold is an
  append.

## Evidence

- `scripts/changelog-test.py`, 19 tests: `check` passes no entry, well-formed entries
  and the directory's README, links from the root, an anchor and an address, and
  what an editor or a file manager leaves; it refuses and names fifteen shapes and
  names, a link that names no file among them, a directory and a file that is not
  text. `fold`, in a repository of its own, puts entries in the order of the commits
  that added them and not of their names, at the end of Unreleased and not below a
  release's section; a branch's entry lands with its merge; a name used again after a
  fold lands with its second commit; an uncommitted entry follows the landed ones; a
  second run changes nothing; and it folds nothing in a shallow checkout, when one
  entry is malformed, or when `CHANGELOG.md` is missing or has no Unreleased section.
- `scripts/release-notes-test.py`, four more: an entry added as a file in the window
  is among its changelog lines, and one that predates the window, the directory's
  README and a renamed file are not; a line added in the window to a file that was
  there is; a fold inside the window reports no entry a second time, and two entries
  that end with the same line keep it; a window with no entry says so.
- `scripts/lint-docs.py`'s gate, by hand on this change's tree: it passes as the tree
  stands; with one entry line appended to `CHANGELOG.md` it prints `change:
  CHANGELOG.md gained 1 entry line(s)`; with the same line and an entry file removed,
  a fold, it passes.
- The pull request of this decision carries its own entry as the first file of
  `changelog.d/`, and `just lint-docs` passes with it.
- The two pull requests that first merge from one `master` with an entry each, and
  neither merging `master`, are named on card 255 when they have.

## Consequences

Easier: two pull requests land in either order unless they change the same code or
one of the other files everyone appends to, `docs/SUMMARY.md`, `docs/glossary.md`,
`spec/README.md`, `counters.toml` and a crate's `lib.rs`, where the rule of keeping
both sides stands. Harder: the newest entries are not in `CHANGELOG.md` until a
release folds them, so a reader of that file alone is sent to the directory by its
opening paragraph, and the weekly email reads both. A release has one step more,
the fold.

A pull request that was open when this landed, with its entry appended to
`CHANGELOG.md`, meets the gate the first time it merges `master`: it moves its lines
into a file, once. The reviewer's brief asks for a file from then on.

Revisit if the other shared files start to cost what this one did: `docs/SUMMARY.md`
is the next, one line an ADR.

## References

- `changelog.d/README.md`; `scripts/changelog.py`; `scripts/release-notes.py`,
  `changelog_added`.
- `.claude/rules/docs.md`, "Changelog"; `CLAUDE.md`, "Working rules for agents".
- ADR 0004, ADR 0071, ADR 0074.
- Card 255.
