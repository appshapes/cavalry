//! The `cavalry` binary (ADR 0002): `cavalry run` starts a driver and stops it on
//! SIGINT, SIGTERM or SIGHUP; `cavalry doctor` explains the driver from its counters;
//! `cavalry record <stream id>` records a stream to the durable log and `cavalry record
//! --stop <recording id>` stops it (ADR 0025). `replay` and `console` arrive with their
//! features. No arguments and no configuration are the primary way to run it (CLAUDE.md,
//! invariant 5): the directory is `spec/client-protocol.md`'s default and the log
//! directory `Driver::default_log_dir()`'s. The keys of `config.toml` are read from the
//! environment, and `--dir` and `--log-dir` on top (ADR 0020; `docs/reference/config.md`).
#![deny(missing_docs, rustdoc::broken_intra_doc_links)]

use std::path::PathBuf;
use std::process::ExitCode;

use cavalry::config::Config;
use cavalry::{Cavalry, Driver, doctor};

const USAGE: &str = "usage: cavalry <command> [--dir <path>] [--log-dir <path>]

  run                      start a driver; Ctrl-C or SIGTERM stops it
  doctor                   explain the driver from its counters; exits 1 when a check
                           failed
  record <stream id>       record the live publication on that stream to the durable
                           log; prints the recording id (--channel <uri> for another
                           channel than cavalry:ipc)
  record --stop <id>       stop that recording
  --version                print the version

  --dir <path>      the driver directory, for every command; the default is
                    /dev/shm/cavalry-<user> on Linux and <tmp>/cavalry-<user> elsewhere
                    (docs/reference/client-protocol.md#the-driver-directory)
  --log-dir <path>  the log directory, for run and doctor; the default is
                    $XDG_DATA_HOME/cavalry/log, else ~/.local/share/cavalry/log, and
                    /var/lib/cavalry/log as root (docs/reference/config.md#logdir)

Every configuration key is read from its environment variable, CAVALRY_DIR,
CAVALRY_LOG_DIR and CAVALRY_CONDUCTOR_IDLE_STRATEGY in this version; a flag wins over
its variable (docs/reference/config.md). The commands replay and console are not built
yet; see crates/cavalry/README.md.";

/// The flags every command takes.
#[derive(Default)]
struct Flags {
    dir: Option<PathBuf>,
    log_dir: Option<PathBuf>,
}

/// What `cavalry record` asked for.
enum RecordAction {
    Start { stream_id: i32, channel: String },
    Stop { recording_id: i64 },
}

/// What the command line asked for.
enum Invocation {
    Run(Flags),
    Doctor(Flags),
    Record(Flags, RecordAction),
    Version,
    Help,
    /// Not a command, or a command with arguments it does not take; the message says
    /// which.
    Wrong(String),
}

/// `cavalry <command> [--dir <path> | --dir=<path>] [--log-dir <path> | --log-dir=<path>]`,
/// `record` with its stream id, `--stop <id>` and `--channel <uri>`, and nothing else.
fn parse(args: impl Iterator<Item = String>) -> Invocation {
    let mut args = args.peekable();
    let Some(command) = args.next() else {
        return Invocation::Wrong(String::new());
    };
    match command.as_str() {
        "--version" | "-V" if args.peek().is_none() => return Invocation::Version,
        "--help" | "-h" | "help" if args.peek().is_none() => return Invocation::Help,
        "run" | "doctor" | "record" => {}
        other => {
            return Invocation::Wrong(format!(
                "`{other}` is not a command this version has, or it took an argument it does \
                 not take"
            ));
        }
    }
    let takes = match command.as_str() {
        "record" => {
            "--dir <path>, --log-dir <path>, --stop <recording id>, --channel <uri> and a stream id"
        }
        _ => "--dir <path> and --log-dir <path>",
    };
    let mut flags = Flags::default();
    let mut stream_id: Option<i32> = None;
    let mut stop: Option<i64> = None;
    let mut channel: Option<String> = None;
    while let Some(arg) = args.next() {
        // `--name value` and `--name=value` both; a value may be empty only to be refused.
        let (name, value) = match arg.split_once('=') {
            Some((name, value)) if name.starts_with("--") => {
                (name.to_string(), Some(value.to_string()))
            }
            _ if arg.starts_with("--") => (arg.clone(), args.next()),
            _ => (String::new(), Some(arg.clone())),
        };
        let value = match value {
            Some(value) if !value.is_empty() => value,
            _ if name.is_empty() => {
                return Invocation::Wrong(format!("`{arg}` is not an argument {command} takes"));
            }
            _ => {
                let kind = if name.ends_with("dir") {
                    "a path"
                } else {
                    "a value"
                };
                return Invocation::Wrong(format!("{name} needs {kind}; {command} got none"));
            }
        };
        let twice = |name: &str| Invocation::Wrong(format!("{name} was given twice to {command}"));
        match name.as_str() {
            "--dir" => {
                if flags.dir.is_some() {
                    return twice(&name);
                }
                flags.dir = Some(PathBuf::from(value));
            }
            "--log-dir" => {
                if flags.log_dir.is_some() {
                    return twice(&name);
                }
                flags.log_dir = Some(PathBuf::from(value));
            }
            "--stop" if command == "record" => {
                if stop.is_some() {
                    return twice(&name);
                }
                match value.parse::<i64>() {
                    Ok(id) if id >= 0 => stop = Some(id),
                    _ => {
                        return Invocation::Wrong(format!(
                            "--stop needs a recording id, a whole number from 0; got `{value}`"
                        ));
                    }
                }
            }
            "--channel" if command == "record" => {
                if channel.is_some() {
                    return twice(&name);
                }
                channel = Some(value);
            }
            "" if command == "record" => {
                if stream_id.is_some() {
                    return Invocation::Wrong(format!(
                        "record takes one stream id; got `{value}` too"
                    ));
                }
                match value.parse::<i32>() {
                    Ok(id) => stream_id = Some(id),
                    Err(_) => {
                        return Invocation::Wrong(format!(
                            "record needs a stream id, a whole number that fits an i32; got `{value}`"
                        ));
                    }
                }
            }
            _ => {
                return Invocation::Wrong(format!(
                    "`{arg}` is not an argument {command} takes; it takes {takes}"
                ));
            }
        }
    }
    match command.as_str() {
        "run" => Invocation::Run(flags),
        "doctor" => Invocation::Doctor(flags),
        _ => match (stream_id, stop) {
            (Some(stream_id), None) => Invocation::Record(
                flags,
                RecordAction::Start {
                    stream_id,
                    channel: channel.unwrap_or_else(|| "cavalry:ipc".to_string()),
                },
            ),
            (None, Some(recording_id)) if channel.is_none() => {
                Invocation::Record(flags, RecordAction::Stop { recording_id })
            }
            (None, Some(_)) => {
                Invocation::Wrong("--channel goes with a stream id, not with --stop".to_string())
            }
            (Some(_), Some(_)) => {
                Invocation::Wrong("record takes a stream id or --stop <id>, not both".to_string())
            }
            (None, None) => {
                Invocation::Wrong("record needs a stream id, or --stop <recording id>".to_string())
            }
        },
    }
}

fn main() -> ExitCode {
    match parse(std::env::args().skip(1)) {
        Invocation::Run(flags) => match configure(flags) {
            Ok(config) => run(&config),
            Err(code) => code,
        },
        Invocation::Doctor(flags) => match configure(flags) {
            Ok(config) => doctor(&config),
            Err(code) => code,
        },
        Invocation::Record(flags, action) => match configure(flags) {
            Ok(config) => record(&config, action),
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

/// The configuration: every key from the environment, then the flags on top. A value a
/// key does not take is refused before anything starts, exit 2 with what to do.
fn configure(flags: Flags) -> Result<Config, ExitCode> {
    let mut config = Config::from_env().map_err(|error| {
        eprintln!("cavalry: {error}");
        ExitCode::from(2)
    })?;
    if let Some(dir) = flags.dir {
        config.dir = dir;
    }
    if let Some(log_dir) = flags.log_dir {
        config.log_dir = Some(log_dir);
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
    let Some(log_dir) = config.log_dir.as_deref() else {
        eprintln!(
            "cavalry run: no log directory: HOME and XDG_DATA_HOME are unset and the driver \
             does not run as root, so log.dir has no default; set CAVALRY_LOG_DIR or \
             --log-dir to a directory on a disk; see docs/reference/config.md#logdir"
        );
        return ExitCode::FAILURE;
    };
    let driver = match Driver::start_with(dir, log_dir, config.conductor_idle_strategy) {
        Ok(driver) => driver,
        Err(error) => {
            eprintln!("cavalry run: {error}");
            return ExitCode::FAILURE;
        }
    };
    eprintln!(
        "cavalry run: driver started on {} (pid {}), recording into {}, conductor idle \
         strategy {}; Ctrl-C or SIGTERM stops it",
        dir.display(),
        std::process::id(),
        log_dir.display(),
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
    let report = doctor::examine_with(&config.dir, config.log_dir.as_deref());
    println!("{report}");
    ExitCode::from(report.exit_code())
}

/// `cavalry record`: a client of the driver for one command. A start prints the
/// recording id, the session recorded and the two counter ids and exits; the recording
/// is the driver's and goes on. A stop prints that it stopped. Exit 1 with the driver's
/// message when the command is refused (`spec/client-protocol.md`, "Error codes").
fn record(config: &Config, action: RecordAction) -> ExitCode {
    let mut client = match Cavalry::connect_to(&config.dir) {
        Ok(client) => client,
        Err(error) => {
            eprintln!("cavalry record: {error}");
            return ExitCode::FAILURE;
        }
    };
    let outcome = match action {
        RecordAction::Start { stream_id, channel } => client
            .start_recording(&channel, stream_id)
            .map(|recording| {
                println!(
                    "recording {} started: stream {} session {} on {}, position {}, synced {}; \
                         `cavalry record --stop {}` stops it, and doctor lists it",
                    recording.recording_id(),
                    recording.stream_id(),
                    recording.session_id(),
                    recording.channel(),
                    recording.position(),
                    recording.synced_position(),
                    recording.recording_id()
                );
            }),
        RecordAction::Stop { recording_id } => client
            .stop_recording(recording_id)
            .map(|()| println!("recording {recording_id} stopped")),
    };
    let closed = client.close();
    match (outcome, closed) {
        (Ok(()), Ok(())) => ExitCode::SUCCESS,
        (Err(error), _) | (Ok(()), Err(error)) => {
            eprintln!("cavalry record: {error}");
            ExitCode::FAILURE
        }
    }
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
        assert!(matches!(
            parsed(&["run"]),
            Invocation::Run(Flags {
                dir: None,
                log_dir: None
            })
        ));
        assert!(matches!(
            parsed(&["doctor"]),
            Invocation::Doctor(Flags {
                dir: None,
                log_dir: None
            })
        ));
        for args in [
            ["run", "--dir", "/x", "--log-dir", "/l"].as_slice(),
            ["run", "--dir=/x", "--log-dir=/l"].as_slice(),
        ] {
            match parsed(args) {
                Invocation::Run(Flags {
                    dir: Some(dir),
                    log_dir: Some(log_dir),
                }) => {
                    assert_eq!(dir, PathBuf::from("/x"));
                    assert_eq!(log_dir, PathBuf::from("/l"));
                }
                _ => panic!("{args:?}"),
            }
        }
        for args in [
            ["run", "--dir"].as_slice(),
            ["run", "--dir="].as_slice(),
            ["run", "--log-dir"].as_slice(),
            ["doctor", "--dir", "/x", "--dir", "/y"].as_slice(),
            ["doctor", "--log-dir", "/x", "--log-dir", "/y"].as_slice(),
            ["doctor", "--idle", "spin"].as_slice(),
            ["doctor", "extra"].as_slice(),
            ["run", "--stop", "1"].as_slice(),
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
    fn record_takes_a_stream_id_or_a_stop_and_nothing_else() {
        match parsed(&["record", "1001"]) {
            Invocation::Record(_, RecordAction::Start { stream_id, channel }) => {
                assert_eq!((stream_id, channel.as_str()), (1001, "cavalry:ipc"));
            }
            _ => panic!("a start"),
        }
        match parsed(&["record", "--channel", "cavalry:ipc", "-7", "--dir=/x"]) {
            Invocation::Record(
                Flags { dir: Some(dir), .. },
                RecordAction::Start { stream_id, .. },
            ) => {
                assert_eq!((stream_id, dir), (-7, PathBuf::from("/x")));
            }
            _ => panic!("a start with a channel and a directory"),
        }
        match parsed(&["record", "--stop", "3"]) {
            Invocation::Record(_, RecordAction::Stop { recording_id }) => {
                assert_eq!(recording_id, 3)
            }
            _ => panic!("a stop"),
        }
        for args in [
            ["record"].as_slice(),
            ["record", "abc"].as_slice(),
            ["record", "1", "2"].as_slice(),
            ["record", "1", "--stop", "2"].as_slice(),
            ["record", "--stop", "-1"].as_slice(),
            ["record", "--stop", "1", "--channel", "cavalry:ipc"].as_slice(),
            ["record", "--stop"].as_slice(),
        ] {
            assert!(matches!(parsed(args), Invocation::Wrong(_)), "{args:?}");
        }
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
