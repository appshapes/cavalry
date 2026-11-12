- `scripts/test-times.sh <times> <cargo test arguments>` runs one `cargo test` command
  many times in a row: it builds once, prints one line after each run and one at the
  end, and stops at the first run that fails, with that run's exit code. The `test
  times` workflow, started by hand with `gh workflow run test-times.yml`, runs the
  script on the Linux runner and on the macOS runner at the branch it is started on.
  The tests of a cluster over three drivers are held to twenty such runs on both
  runners before they are trusted, and a test that failed once in CI is run until it
  fails again
  ([Run a test many times in a row](docs/how-to/run-a-test-many-times.md); card 228).
