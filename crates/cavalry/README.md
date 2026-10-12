# cavalry

The one crate users touch (ADR 0002). Not on the data plane: it holds re-exports and the
binary's `main`.

## What this does

- `cargo add cavalry` gives the client API, re-exported from `cavalry-client`, and an
  embeddable driver, `cavalry::Driver`, re-exported from `cavalry-driver`, so a program
  can run with no separate daemon. Embedding is how the ten-minute path works on a
  laptop; one shared driver per host is the production shape. Today the driver is here
  and the client API arrives with its feature.
- `cargo install cavalry` gives the `cavalry` binary: `run` starts a driver on the
  default directory and stops it on Ctrl-C or SIGTERM, leaving the directory for
  `doctor`; `doctor` explains the driver there from its counters and exits 0 when no
  check failed; `replay` and `console` arrive with their features.
- Control-plane and command-line dependencies sit behind the `cli` feature, on by
  default so that `cargo install cavalry` needs no flag. `cargo add cavalry --no-
  default-features` is the lean library build; it skips the binary and `doctor`.
- Takes no arguments and no configuration: the directory is
  `spec/client-protocol.md`'s default, `/dev/shm/cavalry-<user>` on Linux and
  `<tmp>/cavalry-<user>` elsewhere. A key, when one is justified, comes with its
  `why_not_default` and its `doctor` check (invariant 5).

In Aeron, this is the client library and the media driver, which are separate artifacts
there ([glossary](../../docs/glossary.md)).

## What it must never do

- Grow logic of its own. `src/lib.rs` holds only `pub use` re-exports, and `just lint`
  fails on anything else (ADR 0002). Code belongs in the crate it is about and is re-
  exported here; `src/main.rs` only parses the command and calls that code.
- Need configuration to work. Zero configuration is the primary supported configuration:
  `cavalry` with no arguments works on a laptop and on a cloud VM (invariant 5). A new
  configuration key needs a written reason why the default cannot be right, and a
  `doctor` check that fires when it is set wrong.
- Carry a knob that exists to make a benchmark pass.
- Exit 0 while it does nothing. No command prints the usage and exits 2; a command
  that is not built says so and exits non-zero.

## Example

An embedded driver, examined by `doctor` while it runs and after it has closed. This
is the crate's doctest.

```rust
use cavalry::{Driver, DriverError, doctor};

fn main() -> Result<(), DriverError> {
    let dir = std::env::temp_dir().join(format!("cavalry-readme-{}", std::process::id()));
    let driver = Driver::start(&dir)?;

    // While it runs, doctor finds a fresh heartbeat and nothing to fix.
    let report = doctor::examine(&dir);
    assert!(report.ok(), "{report}");
    assert!(report.findings.iter().any(|f| f.check == "driver-heartbeat" && f.message.contains("is running")));

    // After it closes, doctor finds the heartbeat at -1 and says what to do.
    driver.close()?;
    let report = doctor::examine(&dir);
    assert!(!report.ok());
    assert_eq!(report.exit_code(), 1);
    assert!(report.findings[0].message.contains("closed"));
    println!("{report}");
    std::fs::remove_dir_all(&dir).ok();
    Ok(())
}
```

Running it prints the second report, failures first; on Linux the host checks may add
warnings after the failure:

```text
cavalry doctor: /tmp/cavalry-readme-4242, checks run: 13
  FAIL driver-heartbeat       the driver that made /tmp/cavalry-readme-4242 (pid 4242) closed; its counters below are the last it wrote; start a driver with `cavalry run`; see docs/reference/client-protocol.md#the-driver-directory
  1 failed, 0 warnings
```

## What to read next

- [Run a driver and read doctor](../../docs/how-to/run-the-driver.md), the how-to for
  the binary, and [`spec/client-protocol.md`](../../spec/client-protocol.md), the
  directory, the commands and the events it keeps.
- [`cavalry-driver`](../cavalry-driver/README.md), the driver behind `run`, and
  [`cavalry-control`](../cavalry-control/README.md), the checks behind `doctor`.
- [ADR 0002](../../docs/decisions/0002-one-crate-named-cavalry.md), why this is the one
  crate, and [ADR 0007](../../docs/decisions/0007-build-ipc-pub-sub-first.md), the
  slice the binary is the fourth feature of.
