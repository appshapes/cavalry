- `just lint-docs` now passes an entry line added to `CHANGELOG.md` in two cases only:
  a fold moved it there out of the entry files under `changelog.d/` in the same
  change, or it replaces an entry line where that line stands, a correction. Before,
  the removal of any file under `changelog.d/` let any line through, and a corrected
  typo was refused. `python3 scripts/changelog.py fold` now orders entries by where
  their commits stand on `master` and not by the commits' dates, and refuses an entry
  file that is not as its last commit holds it
  ([how](changelog.d/README.md);
  [ADR 0076](docs/decisions/0076-let-the-changelog-grow-by-a-folds-lines-and-a-corrections-alone.md);
  card 255).
