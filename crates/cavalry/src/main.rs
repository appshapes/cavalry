//! The `cavalry` binary (ADR 0002): `cavalry run` starts a driver on the default
//! directory and stops it on SIGINT, SIGTERM or SIGHUP; `cavalry doctor` explains the
//! driver on that directory from its counters. `replay` and `console` arrive with their
//! features. No arguments and no configuration are the primary way to run it
//! (CLAUDE.md, invariant 5): the directory is `spec/client-protocol.md`'s default.
#![deny(missing_docs, rustdoc::broken_intra_doc_links)]

use std::process::ExitCode;

use cavalry::{Driver, doctor};

const USAGE: &str = "usage: cavalry <command>

  run       start a driver on the default directory; Ctrl-C or SIGTERM stops it
  doctor    explain the driver on the default directory from its counters
  --version print the version

The default directory is /dev/shm/cavalry-<user> on Linux and <tmp>/cavalry-<user>
elsewhere (docs/reference/client-protocol.md#the-driver-directory). The commands
replay and console are not built yet; see crates/cavalry/README.md.";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let command = args.next();
    let extra = args.next();
    match (command.as_deref(), extra) {
        (Some("run"), None) => run(),
        (Some("doctor"), None) => doctor(),
        (Some("--version" | "-V"), None) => {
            println!("cavalry {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        (Some("--help" | "-h" | "help"), None) => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        (Some(other), _) => {
            eprintln!(
                "cavalry: `{other}` is not a command this version has, or it took an argument it does not take\n{USAGE}"
            );
            ExitCode::from(2)
        }
        (None, _) => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

/// Installs the signal handler, then starts a driver and waits for a signal; the driver
/// closes on the way out, leaving −1 in the heartbeat so that doctor and the clients
/// know. The handler goes first, so that no signal between the start and the handler
/// kills the process with a fresh heartbeat behind it, and a handler that cannot be
/// installed refuses to start at all, with the directory untouched.
fn run() -> ExitCode {
    let (stop, stopped) = std::sync::mpsc::channel::<()>();
    if let Err(error) = ctrlc::set_handler(move || {
        let _ = stop.send(());
    }) {
        eprintln!(
            "cavalry run: cannot install the signal handler, so the driver did not start: \
             {error}; run cavalry from a process that lets it handle SIGINT and SIGTERM, \
             or embed cavalry::Driver and stop it yourself"
        );
        return ExitCode::FAILURE;
    }
    let dir = Driver::default_dir();
    let driver = match Driver::start(&dir) {
        Ok(driver) => driver,
        Err(error) => {
            eprintln!("cavalry run: {error}");
            return ExitCode::FAILURE;
        }
    };
    eprintln!(
        "cavalry run: driver started on {} (pid {}); Ctrl-C or SIGTERM stops it",
        dir.display(),
        std::process::id()
    );
    match stopped.recv() {
        Ok(()) => eprintln!("cavalry run: stopping"),
        // The handler's sender is gone without a signal: a bug, never silent.
        Err(_) => eprintln!("cavalry run: the signal handler went away; stopping"),
    }
    match driver.close() {
        Ok(()) => {
            eprintln!(
                "cavalry run: closed; {} stays for doctor, and the next driver remakes it",
                dir.display()
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("cavalry run: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Prints the report and exits with its verdict: 0 when no check failed, 1 otherwise.
fn doctor() -> ExitCode {
    let report = doctor::examine(&Driver::default_dir());
    println!("{report}");
    ExitCode::from(report.exit_code())
}
