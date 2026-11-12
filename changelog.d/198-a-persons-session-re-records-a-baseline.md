- A bench baseline's re-record is now made by a person or by a person's session, a
  Claude Code session that works a card for a person, on a branch of its own through
  the review loop. Until now the pages called it a person's commit. It is made in
  three cases only: master's own run fails or brushes the latency gate at a new tree,
  pull requests at master's tree read red while master's own runs there pass, or a
  change to the harness under `bench/` asks for one in its Accepted ADR. A session
  tells the person before it starts, which case, which tree and how long it holds the
  two bench hosts, and does not wait for an answer. The loop's writer and fixer still
  never change `bench/baseline.json`
  ([ADR 0110](docs/decisions/0110-let-a-persons-session-re-record-a-bench-baseline-after-telling-the-person.md);
  [Run the bench](docs/how-to/run-the-bench.md), step 4; card 198).
