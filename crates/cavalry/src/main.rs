//! The `cavalry` binary (ADR 0002): `cavalry run` starts a driver and stops it on
//! SIGINT, SIGTERM or SIGHUP; `cavalry doctor` explains the driver from its counters.
//! `replay` and `console` arrive with their features. No arguments and no configuration
//! are the primary way to run it (CLAUDE.md, invariant 5): the directory is
//! `spec/client-protocol.md`'s default. The keys of `config.toml` are read from the
//! environment, and `--dir` on top (ADR 0020; `docs/reference/config.md`).
#![deny(missing_docs, rustdoc::broken_intra_doc_links)]

use std::path::PathBuf;
use std::process::ExitCode;

use cavalry::config::Config;
use cavalry::{Driver, doctor};

const USAGE: &str = "usage: cavalry <command> [--dir <path>]

  run       start a driver; Ctrl-C or SIGTERM stops it
  doctor    explain the driver from its counters; exits 1 when a check failed
  --version print the version

  --dir <path>  the driver directory, for run and doctor; the default is
                /dev/shm/cavalry-<user> on Linux and <tmp>/cavalry-<user> elsewhere
                (docs/reference/client-protocol.md#the-driver-directory)

Every configuration key is read from its environment variable, CAVALRY_DIR and
CAVALRY_CONDUCTOR_IDLE_STRATEGY in this version; --dir wins over CAVALRY_DIR
(docs/reference/config.md). The commands replay and console are not built yet; see
crates/cavalry/README.md.";

/// What the command line asked for.
enum Invocation {
    Run {
        dir: Option<PathBuf>,
    },
    Doctor {
        dir: Option<PathBuf>,
    },
    Version,
    Help,
    /// Not a command, or a command with arguments it does not take; the message says
    /// which.
    Wrong(String),
}

/// `cavalry <command> [--dir <path> | --dir=<path>]`, and nothing else.
fn parse(args: impl Iterator<Item = String>) -> Invocation {
    let mut args = args.peekable();
    let Some(command) = args.next() else {
        return Invocation::Wrong(String::new());
    };
    match command.as_str() {
        "--version" | "-V" if args.peek().is_none() => return Invocation::Version,
        "--help" | "-h" | "help" if args.peek().is_none() => return Invocation::Help,
        "run" | "doctor" => {}
        other => {
            return Invocation::Wrong(format!(
                "`{other}` is not a command this version has, or it took an argument it does \
                 not take"
            ));
        }
    }
    let mut dir = None;
    while let Some(arg) = args.next() {
        let value = if arg == "--dir" {
            args.next()
        } else if let Some(value) = arg.strip_prefix("--dir=") {
            Some(value.to_string())
        } else {
            return Invocation::Wrong(format!(
                "`{arg}` is not an argument {command} takes; it takes --dir <path>"
            ));
        };
        match value {
            Some(value) if !value.is_empty() && dir.is_none() => dir = Some(PathBuf::from(value)),
            Some(_) if dir.is_some() => {
                return Invocation::Wrong(format!("--dir was given twice to {command}"));
            }
            _ => return Invocation::Wrong(format!("--dir needs a path; {command} got none")),
        }
    }
    match command.as_str() {
        "run" => Invocation::Run { dir },
        _ => Invocation::Doctor { dir },
    }
}

fn main() -> ExitCode {
    match parse(std::env::args().skip(1)) {
        Invocation::Run { dir } => match configure(dir) {
            Ok(config) => run(&config),
            Err(code) => code,
        },
        Invocation::Doctor { dir } => match configure(dir) {
            Ok(config) => doctor(&config),
            Err(code) => code,
        },
        Invocation::Version => {
            println!("cavalry {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Invocation::Help => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Invocation::Wrong(message) => {
            if message.is_empty() {
                eprintln!("{USAGE}");
            } else {
                eprintln!("cavalry: {message}\n{USAGE}");
            }
            ExitCode::from(2)
        }
    }
}

/// The configuration: every key from the environment, then `--dir` on top. A value a
/// key does not take is refused before anything starts, exit 2 with what to do.
fn configure(dir: Option<PathBuf>) -> Result<Config, ExitCode> {
    let mut config = Config::from_env().map_err(|error| {
        eprintln!("cavalry: {error}");
        ExitCode::from(2)
    })?;
    if let Some(dir) = dir {
        config.dir = dir;
    }
    Ok(config)
}

/// Installs the signal handler, then starts a driver and waits for a signal; the driver
/// closes on the way out, leaving −1 in the heartbeat so that doctor and the clients
/// know. The handler goes first, so that no signal between the start and the handler
/// kills the process with a fresh heartbeat behind it, and a handler that cannot be
/// installed refuses to start at all, with the directory untouched.
fn run(config: &Config) -> ExitCode {
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
    let dir = &config.dir;
    let driver = match Driver::start_with(dir, config.conductor_idle_strategy) {
        Ok(driver) => driver,
        Err(error) => {
            eprintln!("cavalry run: {error}");
            return ExitCode::FAILURE;
        }
    };
    eprintln!(
        "cavalry run: driver started on {} (pid {}), conductor idle strategy {}; Ctrl-C or \
         SIGTERM stops it",
        dir.display(),
        std::process::id(),
        config.conductor_idle_strategy.name()
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
fn doctor(config: &Config) -> ExitCode {
    let report = doctor::examine(&config.dir);
    println!("{report}");
    ExitCode::from(report.exit_code())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cavalry::config::generated::{DIR, KEYS};

    fn parsed(args: &[&str]) -> Invocation {
        parse(args.iter().map(|arg| arg.to_string()))
    }

    #[test]
    fn the_dir_flag_is_read_both_ways_and_refused_without_a_path() {
        assert!(matches!(parsed(&["run"]), Invocation::Run { dir: None }));
        assert!(matches!(
            parsed(&["doctor"]),
            Invocation::Doctor { dir: None }
        ));
        for args in [
            ["run", "--dir", "/x"].as_slice(),
            ["run", "--dir=/x"].as_slice(),
        ] {
            match parsed(args) {
                Invocation::Run { dir: Some(dir) } => assert_eq!(dir, PathBuf::from("/x")),
                _ => panic!("{args:?}"),
            }
        }
        for args in [
            ["run", "--dir"].as_slice(),
            ["run", "--dir="].as_slice(),
            ["doctor", "--dir", "/x", "--dir", "/y"].as_slice(),
            ["doctor", "--idle", "spin"].as_slice(),
            ["doctor", "extra"].as_slice(),
            ["fly"].as_slice(),
            ["--version", "now"].as_slice(),
        ] {
            assert!(matches!(parsed(args), Invocation::Wrong(_)), "{args:?}");
        }
        assert!(matches!(parsed(&[]), Invocation::Wrong(message) if message.is_empty()));
        assert!(matches!(parsed(&["--version"]), Invocation::Version));
        assert!(matches!(parsed(&["help"]), Invocation::Help));
    }

    #[test]
    fn the_usage_names_every_flag_the_registry_has() {
        assert!(USAGE.contains(DIR.flag.expect("dir has a flag")));
        for key in KEYS {
            assert!(USAGE.contains(key.env), "{}", key.env);
            if let Some(flag) = key.flag {
                assert!(USAGE.contains(flag), "{flag}");
            }
        }
    }
}
