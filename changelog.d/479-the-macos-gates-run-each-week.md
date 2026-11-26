- CI runs the macOS gates once a week at `master`'s head, each Monday at 04:23 UTC,
  by hand, and at a release, and no longer for every commit on `master`. A new
  workflow, `macos`, runs them, and `ci` runs the gates on Linux alone, a run by hand
  included: `gh workflow run macos.yml --ref <branch>` is how the macOS gates are
  started at a branch
  ([ADR 0170](docs/decisions/0170-run-the-macos-gates-each-week-and-at-a-release-and-not-for-every-commit.md);
  [Read the weekly run of the macOS gates](docs/how-to/read-the-weekly-run-of-the-macos-gates.md)).
