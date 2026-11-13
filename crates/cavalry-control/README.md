# cavalry-control

The admin API, discovery, OpenTelemetry export and configuration. Control plane: `async`
is allowed here.

## What this does

- Exports OpenTelemetry metrics and traces from the counters file. Counters are the
  primary instrument; this crate turns them into something a dashboard can read, at zero
  cost to the data plane.
- Serves the admin API and discovery.
- Loads configuration: `config::Config` is every key of `config.toml` resolved, the
  defaults with each key whose environment variable is set applied, and a value a key
  does not take refused before anything starts. Every key lives in `config.toml` with a
  non-empty `why_not_default`, a `doctor` check that fires when it is set wrong, and a
  section of the generated reference; `config::generated` is the key table written from
  it (ADR 0020).
- Explains a driver from its directory: `doctor` opens the counters file and the command
  ring read-only, as their pages say a reader does, and every finding names the docs
  anchor that explains it (`CLAUDE.md`, invariant 4). The checks of the configuration
  keys are here: the filesystem under the driver directory (`driver-directory-backing`)
  and under the log directory (`log-directory-backing`), the idle strategy the running
  conductor recorded in its counter against the host's isolated CPUs
  (`conductor-idle-strategy`), and the two cluster keys, checked together and against
  the members string (`cluster-members`, `cluster-member-id`): each host name of the
  string is looked up once, and one that resolves to no address is one finding that
  names every endpoint it is the host of. `doctor::examine_config`
  runs every check with the configuration the keys give, and the `cavalry doctor`
  command is that call behind a `main`.
- Records what `cavalry run` did, for `doctor` to read once the driver has closed:
  `run::member_given_up` adds one to `run.member_given_up` in the driver's counters
  file when `cavalry run` gives up the cluster member it runs beside its driver. The
  counter is static, the driver's own, so it is in the directory a closed driver
  leaves, and `doctor` reports it there with its sentence from `counters.toml`; what
  the member counted is freed with its client. `doctor` reads a static counter only
  from a record of that counter's type: in the file of a driver older than the
  counter the record may hold another, and that is the note `older than doctor`.
- Explains a cluster member from the counters its consensus module stores, while its
  driver runs: who the member is and what it knows, its role, term, election state,
  append position and commit position, and from its two gauges who it follows and why
  its last election began (`cluster-member`, a note; ADR 0102); a member that follows
  no leader, with the state its election is in, why it began, and that a quorum of
  the members elects a leader, the quorum by its size when `examine_config` has the
  members string (`cluster-no-leader`); a member that has taken its leader and joins
  its log, with what its election's state waits for, an image of the leader's log, a
  replication or a catch-up, and the path that needs with its endpoints
  (`cluster-joining`, a note); a leader that lost its quorum (`cluster-quorum-lost`); a
  commit position behind the leader's own append position (`cluster-commit-waits`);
  and, with each counter's own doctor sentence from `counters.toml`, consensus messages
  the member rejected, as from a member of another version
  (`cluster.messages_rejected`), a leader it stopped hearing
  (`cluster.leader_heartbeat_timeouts`), a leader's log stream it lost
  (`cluster.log_images_lost`), the file of its log's recordings that it could not
  write for its service (`cluster.log_recordings_unwritten`), the list of them in
  its log directory that it could not write or sync
  (`cluster.log_recordings_unsynced`), and a recording of its log that it forgot,
  after which it does not start again on its log directory
  (`cluster.log_recordings_forgotten`). `cluster-no-leader` and `cluster-joining` end with the
  elections the member has failed, the counters of three rules that fail one, and
  how many failed on none of the three. A member begins its election again after one
  that fails, so no finding tells an operator to start the cluster again (ADR 0100).
  Every member elects its leader, and `doctor` has one set of findings for it (ADR
  0107). [Run a cluster of three
  members](../../docs/how-to/run-a-cluster.md#when-it-does-not-work) says what an
  operator does about each.
- May use `async` and an executor: nothing a message passes through lives here.

In Aeron, this is AeronStat and hand-written tooling: the open project has no admin API
and no OpenTelemetry export ([glossary](../../docs/glossary.md)).

## What it must never do

- Sit on the path a message takes. The data plane never waits on this crate, and no
  data-plane crate may depend on it; `just lint` fails that dependency.
- Add a lock, a syscall or an allocation to the data plane by reaching into shared
  state. It reads the counters file; it does not write the hot path's structures.
- Replace counters with logs as the primary instrument (invariant 4). Logs and traces
  are derived from counters and are zero-cost when disabled.
- Add a configuration key without a written reason why the default cannot be right
  (invariant 5). Tune by detection, not by documentation.

## Example

`doctor` explains a driver from its directory alone: it opens the counters file and the
command ring read-only, runs every check, and every finding says what the operator does
next and where the docs explain it. This is the crate's doctest.

```rust
use cavalry_control::doctor::{Level, examine};

fn main() {
    // A directory with no driver: the first finding fails and says what to do.
    let dir = std::env::temp_dir().join(format!("cavalry-control-readme-{}", std::process::id()));
    let report = examine(&dir);
    assert!(!report.ok());
    assert_eq!(report.exit_code(), 1);

    let first = &report.findings[0];
    assert_eq!(first.check, "driver-directory");
    assert_eq!(first.level, Level::Fail);
    assert!(first.message.contains("cavalry run"));
    assert_eq!(first.doc, "docs/reference/client-protocol.md#the-driver-directory");

    // With a driver running, the same call reports its heartbeat, the command ring's
    // fill, the counters file's fill, every static counter whose doctor text applies,
    // and the host's huge pages, governor and isolated CPUs on Linux.
    println!("{report}");
}
```

Running it prints the report, one line per finding. On macOS:

```text
cavalry doctor: /tmp/cavalry-control-readme-4242, checks run: 1
  FAIL driver-directory       no driver directory at /tmp/cavalry-control-readme-4242; start a driver with `cavalry run`, or point doctor at the directory the driver uses; see docs/reference/client-protocol.md#the-driver-directory
  1 failed, 0 warnings
```

On Linux the four host checks run too, so the header reads `checks run: 5`, and a
laptop whose governor is not `performance` adds a `WARN host-governor` line; a host with
isolated CPUs adds a `note host-isolated-cpus` line recommending busy-spin there. A
directory that exists adds `driver-directory-backing`, a warning on Linux when it is not
on a filesystem in memory, and a running driver adds `conductor-idle-strategy`.

## What to read next

- [Configuration](../../docs/reference/config.md), every key, how one is set and the
  check that fires when it is set wrong, generated from `config.toml`; and
  [ADR 0020](../../docs/decisions/0020-configure-by-environment-variable.md), why keys
  are read from the environment and what a secret will look like.
- [Counters](../../docs/reference/counters.md), what each counter `doctor` reads means
  and what to do, generated from `counters.toml`; and
  [Run a driver and read doctor](../../docs/how-to/run-the-driver.md), the report as an
  operator sees it.
- [`CLAUDE.md`](../../CLAUDE.md), invariants 4 and 5, which this crate enforces, and
  [ADR 0003](../../docs/decisions/0003-lay-out-the-workspace.md), how the workspace is
  laid out. The admin API, discovery and the OpenTelemetry export have no page yet.
