- [Run the bench](docs/how-to/run-the-bench.md), "When it does not work", says what
  to do with a `REGRESSION` on a message row after a change that touches nothing the
  row runs through: which crates the bench links, how to emit and compare the
  assembly of the row's path in the two trees, what the same and a difference
  mean, and how the job's re-run tells the binary from the process. A row's p99.99
  moves with where the linker puts code that did not change:
  over 50 processes of 33 of `master`'s binaries on one bench host a claim row's
  median run read 127 to 151 ns, where processes of one binary differ by 10 at most
  (card 198).
