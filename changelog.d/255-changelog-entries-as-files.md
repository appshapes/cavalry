- A change's changelog entry is now a file of its own under `changelog.d/`, and
  nobody appends to `CHANGELOG.md` between releases. Two pull requests that each
  appended to the end of Unreleased always conflicted, whatever else they changed, so
  the second to land went round again: merge, gates, review and, for most, the bench
  hosts. `just lint-docs` checks an entry's name and shape,
  `python3 scripts/changelog.py fold` moves the entries into `CHANGELOG.md` at a
  release in the order they landed, and the weekly release notes read both places
  ([how to add an entry](changelog.d/README.md);
  [ADR 0075](docs/decisions/0075-keep-each-changes-changelog-entry-in-a-file-of-its-own.md);
  card 255).
