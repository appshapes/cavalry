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
3. Do not edit `CHANGELOG.md`.
4. Run `just lint-docs`. It prints `changelog: N entries in changelog.d/, ok`, or names
   the file and the line it could not place.

## To read every change

`CHANGELOG.md` has what was folded at the last release and before; the files here have
what landed since, one change each. `git log --first-parent --diff-filter=A
--format=%cs -- changelog.d/<file>` gives the day one landed.

## At a release

`python3 scripts/changelog.py fold` puts the entries at the end of `CHANGELOG.md`'s
Unreleased section, in the order they landed, and removes the files. Commit both with
the release. A second run changes nothing. It needs the repository's history, so it
refuses in a shallow checkout.

## When it does not work

- `changelog: <file>: the first line begins`, or any other line from the check: the
  file is not the shape the fold places. The line says which rule; fix the file.
- `changelog: <file>: the link to <path> names no file from the repository's root`: a
  link in an entry is read where `CHANGELOG.md` is, not where the entry's file is.
  Write `docs/...`, not `../docs/...`.
- `lint-docs: change: CHANGELOG.md gained N entry line(s)`: the change appended its
  entry to `CHANGELOG.md`. Move those lines into a file here and take them out of
  `CHANGELOG.md`. A branch that was open before 2026-11-11 meets this the first time
  it merges `master`.
- Two pull requests gave their files one name: the second to land conflicts. Rename
  yours; the card's number or the pull request's makes the name its own.
- A file whose name begins with a dot or ends with `~` is not read: an editor or a file
  manager left it.
