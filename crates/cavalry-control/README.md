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
  anchor that explains it (`CLAUDE.md`, invariant 4). The checks of the two
  configuration keys are here: the filesystem under the driver directory, and the idle
  strategy the running conductor recorded in its counter against the host's isolated
  CPUs. The `cavalry doctor` command is this module behind a `main`.
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
