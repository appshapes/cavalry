- Brigade's server is built against `master` each night. The new workflow
  `.github/workflows/brigade-server.yml` runs at 06:43 UTC and by hand. It takes
  Brigade's `master`, points its server at this repository's tree in place of the
  commit the server pins, with the new `scripts/brigade-server-patch.py`, and runs
  Brigade's own tests of the server, its conformance suite on the Cavalry feed among
  them. The run is reported and not required: it is no required check and no check of
  a pull request. It runs on a runner of GitHub's own with a read-only token, no
  secret and no cache, since it runs another repository's code. A red night is a
  regression here, which is fixed, or a change made on purpose, which is said in the
  changelog and told to Brigade's sessions, or a failure of Brigade's own.
  `/next-task` reads the newest runs on `master` and names a red one that nobody has
  read ahead of the features. `gh workflow run brigade-server.yml --ref <branch>`
  builds the server against a branch before it lands, and the script does the same on
  a laptop
  ([Read the nightly run of Brigade's server](docs/how-to/read-the-nightly-run-of-brigades-server.md);
  [ADR 0142](docs/decisions/0142-build-brigades-server-against-master-each-night-through-a-cargo-patch.md);
  [ADR 0138](docs/decisions/0138-draw-the-finish-line-at-a-beta-that-is-an-alternative-to-aeron-and-build-it-sixth.md),
  feature 2; work item 362).
