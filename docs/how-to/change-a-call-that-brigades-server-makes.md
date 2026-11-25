# Change a call that Brigade's server makes

A how-to page: one task, changing a call or a behaviour of Cavalry that Brigade's
server relies on, so that the change is made knowingly.
[What does Brigade's server rely on?](../explanation/what-brigades-server-relies-on.md)
lists those calls and says why they are held.

## Prerequisites

- A change that touches a call in that page's table, "What the server calls, and what
  it relies on", or a test of `crates/cavalry/tests/feed_shape.rs` or
  `crates/cavalry/tests/feed_held_still.rs` that fails.
- `cargo`, as for every gate.

## Steps

1. Run the two test programs:

   ```sh
   cargo test -p cavalry --test feed_shape --test feed_held_still
   ```

   The second holds a process still for 12 s. You will see `test result: ok. 12
   passed` and a line that begins `feed_held_still: ok` when the server's use still
   works. Otherwise you will see the test that names what broke.

2. Decide which of two changes yours is.
   - **The server's use can keep working.** Keep it working, and go to "How to verify
     it worked". Until version 0.1.0 the server builds from a pin. The two servers in
     production run Brigade's server 0.10.0, built at d265af82, since 2026-11-24. They
     hold their teams' data in directories that server 0.9.0, built at 620a2e29, wrote
     first and that 0.10.0 writes on, and each keeps a copy taken under 0.9.0.
     Brigade's `master` pins what its `server/crates/brigade-log/Cargo.toml` names.
   - **The change must break the server's use.** Go on with step 3.

3. Change `crates/cavalry/tests/feed/mod.rs` in the way the server's own file,
   `server/crates/brigade-log/src/feed.rs`, will have to change. Say in a comment at
   the place which call changed and why.

4. Change the row of the table on the explanation page, and its "What does not hold"
   section when the change ends one of the cases there.

5. Say in the change's entry under `changelog.d/` what a program that makes the call
   must do now ([how](../../changelog.d/README.md)).

6. Tell the sessions that work on Brigade's server. Add one work item to the Brigade
   Board, titled `Brigade's server on master: <the call that changed>` and tagged
   `brigade` and `server`. Say in it what changed and which commit holds it: `brigade
   work-item add --tag brigade,server`, with the title, an empty line and the
   description on its standard input. You will see `added:` and the work item's
   number. The title's first words are how the nightly run's page and `/next-task`
   find that a red night has been read
   ([Read the nightly run of Brigade's server](read-the-nightly-run-of-brigades-server.md),
   step 4). When your change turns a night red, whoever reads that night comments on
   this work item and adds no second one.

## How to verify it worked

- `cargo test -p cavalry --test feed_shape --test feed_held_still` prints `test
  result: ok` and `feed_held_still: ok`.
- `just lint-docs` ends with `ok`: the changelog entry is in the shape it checks.
- For a change that broke the server's use on purpose: the table's row, the comment in
  `tests/feed/mod.rs` and the changelog entry name the same call.
- To hold a branch against the server's own suite before it lands, start the nightly
  job on it: `gh workflow run brigade-server.yml --ref <branch>`
  ([Read the nightly run of Brigade's server](read-the-nightly-run-of-brigades-server.md),
  "Run it by hand").

## When it does not work

- `conformance/upgrade/log-directory.sh` fails at "build 620a2e29" or "build d265af82"
  and names `crates/cavalry/tests/feed/` or `crates/cavalry/examples/feed-directory.rs`:
  the feed's files now use a call that the older tree lacks, which the server built at
  that tree, 0.9.0 or 0.10.0, cannot make either. When the change is on purpose, keep
  the files as they last compiled against that tree under
  `conformance/upgrade/feed-at-<its first 8 hex digits>/` and have the script copy
  those
  ([ADR 0141](../decisions/0141-open-in-place-a-log-directory-written-by-the-tree-that-servers-run-and-build-that-tree-on-every-run.md),
  [ADR 0156](../decisions/0156-hold-in-the-upgrade-gate-every-directory-a-server-holds-each-trees-alone-and-the-trees-in-turn.md)).

- A test of `feed_shape.rs` fails and your change did not mean to change a call: the
  test's first comment says what it holds, and the explanation page's table says which
  call that is.
- `a_start_beside_an_earlier_client_the_driver_still_holds_records_that_clients_publication`
  fails: the rule for which publication `start recording` takes has changed. That test
  holds the rule as it stood, and its comment says what to turn around (work item 372).
- `feed_held_still` is killed while its two feed processes are stopped: they stay
  stopped. The test printed their process ids before it stopped them; `kill -CONT
  <id>` lets each end.
