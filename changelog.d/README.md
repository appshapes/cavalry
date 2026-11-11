# Changelog entries

One file here is one change's entry for `CHANGELOG.md`, waiting for the next release.
[ADR 0075](../docs/decisions/0075-keep-each-changes-changelog-entry-in-a-file-of-its-own.md)
has the reason: two pull requests that each appended to `CHANGELOG.md` always met in
it, whatever else they changed.

## To add an entry

1. Create `changelog.d/<number>-<a few words>.md`. The number is the card's, or the
   pull request's when the change has no card, so that two changes do not choose one
   name; the words are lower-case letters, digits and hyphens:
   `255-changelog-entries-as-files.md`.
2. Write in it what you would have appended under Unreleased, in the reader's words:
   a line that begins `- `, its continuation lines indented two spaces, and a link to
   the ADR or the page where one exists. Write a link from the repository's root, as
   `CHANGELOG.md` will hold it: `[Run the bench](docs/how-to/run-the-bench.md)`. A
   change with two entries puts both in the one file, one after the other with no
   empty line between them.
3. Do not add the entry to `CHANGELOG.md`. A correction to a line that file already
   has is the one edit a change may make there: replace the line where it stands, and
   add none.
4. Run `just lint-docs`. It prints `changelog: N entries in changelog.d/, ok`, or names
   the file and the line it could not place.

## To read every change

`CHANGELOG.md` has what was folded at the last release and before; the files here have
what landed since, one change each. `git log --first-parent --diff-filter=A
--format=%cs -- changelog.d/<file>` gives the day one landed.

## At a release

`python3 scripts/changelog.py fold` puts the entries at the end of `CHANGELOG.md`'s
Unreleased section, in the order they landed on `master`, and removes the files.
Commit both with the release. A second run changes nothing. It needs the repository's
history, so it refuses in a shallow checkout. It folds the files as `HEAD` holds them,
so it refuses a file that `git status` shows: commit the entry, then fold.

The change that folds an entry neither corrects it nor adds it: `just lint-docs` reads
a change from its two ends, and passes a folded line only when the entry files held
that very line where the change began. To correct an entry that is still a file,
correct the file in a pull request of its own and let it land before the fold's. To
correct a line the fold has moved, land the fold, then correct the line in
`CHANGELOG.md`.

## When it does not work

- `changelog: <file>: the first line begins`, or any other line from the check: the
  file is not the shape the fold places. The line says which rule; fix the file.
- `changelog: <file>: the link to <path> names no file from the repository's root`: a
  link in an entry is read where `CHANGELOG.md` is, not where the entry's file is.
  Write `docs/...`, not `../docs/...`.
- `lint-docs: change: CHANGELOG.md gained N entry line(s) that are neither a fold's
  nor a correction's`: the change added N entry lines to `CHANGELOG.md` that the entry
  files did not hold where the change began, and that replace no entry line where
  they stand
  ([ADR 0076](../docs/decisions/0076-let-the-changelog-grow-by-a-folds-lines-and-a-corrections-alone.md)).
  - The change appended its entry: move those lines into a file here and take them
    out of `CHANGELOG.md`. A branch that was open before 2026-11-11 meets this the
    first time it merges `master`.
  - A correction added a line where it replaced one, a re-wrapped entry for example:
    keep the entry to the lines it had.
  - A release corrected an entry, or added one, in the change that folds it: "At a
    release", above, has the order. Each commit passes by itself on a laptop, and the
    pull request as a whole does not.
- `lint-docs: change: what CHANGELOG.md gained from <range> could not be read`: git
  does not hold one end of the range, or the two ends share no commit. Run it over a
  range this checkout holds: `LINT_DOCS_RANGE=origin/master...HEAD just lint-docs`.
- `changelog: <file> is not as HEAD holds it`: `fold` moves an entry as its last
  commit holds it, and this file is new, changed or staged. Commit it, then fold.
- `changelog: git could not read this checkout's history or its status`: `fold` asks
  git where each entry landed. Run `git status` in the checkout, mend what it
  reports, then fold.
- `changelog: <file> was added by no commit on this branch's first-parent history`:
  the entry reached this branch some other way than a commit or a merge on it. Fold
  on `master`, where the entry landed.
- Two pull requests gave their files one name: the second to land conflicts. Rename
  yours; the card's number or the pull request's makes the name its own.
- A file whose name begins with a dot or ends with `~` is not read: an editor or a file
  manager left it.
