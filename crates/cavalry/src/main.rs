//! The `cavalry` binary (ADR 0002): `cavalry run` starts a driver and stops it on
//! SIGINT, SIGTERM or SIGHUP; `cavalry doctor` explains the driver from its counters;
//! `cavalry record <stream id>` records a stream to the durable log and `cavalry record
//! --stop <recording id>` stops it (ADR 0025); `cavalry replay --recording <id> --from
//! <position or instant>` plays a recording back and prints every message (ADR 0030).
//! `console` arrives with its feature. No arguments and no configuration are the primary way to run it (CLAUDE.md,
//! invariant 5): the directory is `spec/client-protocol.md`'s default and the log
//! directory `Driver::default_log_dir()`'s. The keys of `config.toml` are read from the
//! environment, and `--dir` and `--log-dir` on top (ADR 0020; `docs/reference/config.md`).
#![deny(missing_docs, rustdoc::broken_intra_doc_links)]

use std::path::PathBuf;
use std::process::ExitCode;

use cavalry::config::Config;
use cavalry::{Cavalry, Driver, FOLLOW_LENGTH, NULL_LENGTH, doctor, index};

const USAGE: &str = "usage: cavalry <command> [--dir <path>] [--log-dir <path>]

  run                      start a driver; Ctrl-C or SIGTERM stops it
  doctor                   explain the driver from its counters; exits 1 when a check
                           failed
  record <stream id>       record the live publication on that stream to the durable
                           log; prints the recording id (--channel <uri> for another
                           channel than cavalry:ipc)
  record --stop <id>       stop that recording
  replay --recording <id> --from <position or instant> [--to <position or instant>]
         [--stream <id>] [--follow]
                           play that recording back from the position, or from the
                           instant resolved through its time index, and print one line
                           per message, the position and the body, until the replay
                           ends: at --to, at the recording's extent, or, with --follow,
                           when the recording stops; an instant is an RFC 3339 time
                           (2026-10-29T09:36:00Z) or @<nanoseconds since the Unix
                           epoch>; --stream names the replay stream id, else one of the
                           command's own
  --version                print the version

  --dir <path>      the driver directory, for every command; the default is
                    /dev/shm/cavalry-<user> on Linux and <tmp>/cavalry-<user> elsewhere
                    (docs/reference/client-protocol.md#the-driver-directory)
  --log-dir <path>  the log directory, for run, doctor and replay; the default is
                    $XDG_DATA_HOME/cavalry/log, else ~/.local/share/cavalry/log, and
                    /var/lib/cavalry/log as root (docs/reference/config.md#logdir)

Every configuration key is read from its environment variable, CAVALRY_DIR,
CAVALRY_LOG_DIR and CAVALRY_CONDUCTOR_IDLE_STRATEGY in this version; a flag wins over
its variable (docs/reference/config.md). The command console is not built yet; see
crates/cavalry/README.md.";

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

/// A point in a recording as the command line names it: a position, or an instant the
/// time index resolves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Point {
    Position(i64),
    /// Nanoseconds since the Unix epoch.
    Instant(i64),
}

/// What `cavalry replay` asked for.
#[derive(Debug, PartialEq, Eq)]
struct ReplayRequest {
    recording_id: i64,
    from: Point,
    to: Option<Point>,
    stream_id: Option<i32>,
    follow: bool,
}

/// What the command line asked for.
enum Invocation {
    Run(Flags),
    Doctor(Flags),
    Record(Flags, RecordAction),
    Replay(Flags, ReplayRequest),
    Version,
    Help,
    /// Not a command, or a command with arguments it does not take; the message says
    /// which.
    Wrong(String),
}

/// `cavalry <command> [--dir <path> | --dir=<path>] [--log-dir <path> | --log-dir=<path>]`,
/// `record` with its stream id, `--stop <id>` and `--channel <uri>`, `replay` with its
/// `--recording`, `--from`, `--to`, `--stream` and `--follow`, and nothing else.
fn parse(args: impl Iterator<Item = String>) -> Invocation {
    let mut args = args.peekable();
    let Some(command) = args.next() else {
        return Invocation::Wrong(String::new());
    };
    match command.as_str() {
        "--version" | "-V" if args.peek().is_none() => return Invocation::Version,
        "--help" | "-h" | "help" if args.peek().is_none() => return Invocation::Help,
        "run" | "doctor" | "record" | "replay" => {}
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
        "replay" => {
            "--dir <path>, --log-dir <path>, --recording <id>, --from <position or instant>, \
             --to <position or instant>, --stream <id> and --follow"
        }
        _ => "--dir <path> and --log-dir <path>",
    };
    let mut flags = Flags::default();
    let mut stream_id: Option<i32> = None;
    let mut stop: Option<i64> = None;
    let mut channel: Option<String> = None;
    let mut recording: Option<i64> = None;
    let mut from: Option<Point> = None;
    let mut to: Option<Point> = None;
    let mut follow = false;
    while let Some(arg) = args.next() {
        // `--follow` takes no value.
        if arg == "--follow" && command == "replay" {
            if follow {
                return Invocation::Wrong("--follow was given twice to replay".to_string());
            }
            follow = true;
            continue;
        }
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
            "--recording" if command == "replay" => {
                if recording.is_some() {
                    return twice(&name);
                }
                match value.parse::<i64>() {
                    Ok(id) if id >= 0 => recording = Some(id),
                    _ => {
                        return Invocation::Wrong(format!(
                            "--recording needs a recording id, a whole number from 0; got `{value}`"
                        ));
                    }
                }
            }
            "--from" | "--to" if command == "replay" => {
                let slot = if name == "--from" { &mut from } else { &mut to };
                if slot.is_some() {
                    return twice(&name);
                }
                match parse_point(&value) {
                    Ok(point) => *slot = Some(point),
                    Err(why) => return Invocation::Wrong(format!("{name}: {why}")),
                }
            }
            "--stream" if command == "replay" => {
                if stream_id.is_some() {
                    return twice(&name);
                }
                match value.parse::<i32>() {
                    Ok(id) => stream_id = Some(id),
                    Err(_) => {
                        return Invocation::Wrong(format!(
                            "--stream needs a stream id, a whole number that fits an i32; got `{value}`"
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
        "replay" => match (recording, from) {
            (Some(recording_id), Some(from)) if follow && to.is_some() => {
                let _ = (recording_id, from);
                Invocation::Wrong("replay takes --to or --follow, not both".to_string())
            }
            (Some(recording_id), Some(from)) => Invocation::Replay(
                flags,
                ReplayRequest {
                    recording_id,
                    from,
                    to,
                    stream_id,
                    follow,
                },
            ),
            (None, _) => Invocation::Wrong("replay needs --recording <id>".to_string()),
            (_, None) => Invocation::Wrong("replay needs --from <position or instant>".to_string()),
        },
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

/// A point of a recording from the command line: a whole number is a position, a
/// multiple of 32 from 0; `@<n>` is an instant, n nanoseconds since the Unix epoch;
/// anything else is an RFC 3339 time, `2026-10-29T09:36:00Z`, with an optional
/// fraction of a second and an offset `Z` or `±HH:MM`.
fn parse_point(value: &str) -> Result<Point, String> {
    if let Some(nanos) = value.strip_prefix('@') {
        return nanos
            .parse::<i64>()
            .map(Point::Instant)
            .map_err(|_| format!("`{value}` is not @<nanoseconds since the Unix epoch>"));
    }
    if value.bytes().all(|b| b.is_ascii_digit()) {
        return match value.parse::<i64>() {
            Ok(position) if position % 32 == 0 => Ok(Point::Position(position)),
            Ok(_) => Err(format!("position {value} is not a multiple of 32")),
            Err(_) => Err(format!("`{value}` is not a position that fits an i64")),
        };
    }
    parse_rfc3339(value).map(Point::Instant).ok_or_else(|| {
        format!(
            "`{value}` is not a position, an RFC 3339 time such as 2026-10-29T09:36:00Z, or \
             @<nanoseconds since the Unix epoch>"
        )
    })
}

/// An RFC 3339 time as nanoseconds since the Unix epoch: `YYYY-MM-DDTHH:MM:SS`, an
/// optional `.fraction` of up to nine digits, and `Z` or `±HH:MM`; `T` may be a space
/// or a lowercase `t`, and `z` stands for `Z`. The civil date arithmetic is the
/// proleptic Gregorian calendar's.
fn parse_rfc3339(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    let digits = |from: usize, count: usize| -> Option<i64> {
        let slice = bytes.get(from..from + count)?;
        if !slice.iter().all(u8::is_ascii_digit) {
            return None;
        }
        std::str::from_utf8(slice).ok()?.parse().ok()
    };
    let year = digits(0, 4)?;
    let month = digits(5, 2)?;
    let day = digits(8, 2)?;
    let hour = digits(11, 2)?;
    let minute = digits(14, 2)?;
    let second = digits(17, 2)?;
    if bytes.get(4) != Some(&b'-')
        || bytes.get(7) != Some(&b'-')
        || !matches!(bytes.get(10), Some(b'T' | b't' | b' '))
        || bytes.get(13) != Some(&b':')
        || bytes.get(16) != Some(&b':')
    {
        return None;
    }
    let mut at = 19;
    let mut fraction_nanos = 0i64;
    if bytes.get(at) == Some(&b'.') {
        let start = at + 1;
        let mut end = start;
        while bytes.get(end).is_some_and(u8::is_ascii_digit) {
            end += 1;
        }
        let count = end - start;
        if count == 0 || count > 9 {
            return None;
        }
        fraction_nanos = digits(start, count)? * 10i64.pow(9 - count as u32);
        at = end;
    }
    let offset_seconds = match bytes.get(at) {
        Some(b'Z' | b'z') if at + 1 == bytes.len() => 0,
        Some(sign @ (b'+' | b'-')) if at + 6 == bytes.len() && bytes.get(at + 3) == Some(&b':') => {
            let hours = digits(at + 1, 2)?;
            let minutes = digits(at + 4, 2)?;
            if hours > 23 || minutes > 59 {
                return None;
            }
            let seconds = hours * 3600 + minutes * 60;
            if *sign == b'+' { seconds } else { -seconds }
        }
        _ => return None,
    };
    if !(1..=12).contains(&month) || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days_in_month = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if leap => 29,
        _ => 28,
    };
    if !(1..=days_in_month).contains(&day) {
        return None;
    }
    // Days from the civil date to the epoch: Howard Hinnant's days_from_civil.
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let seconds = days
        .checked_mul(86_400)?
        .checked_add(hour * 3600 + minute * 60 + second)?
        .checked_sub(offset_seconds)?;
    seconds
        .checked_mul(1_000_000_000)?
        .checked_add(fraction_nanos)
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
        Invocation::Replay(flags, request) => match configure(flags) {
            Ok(config) => replay(&config, request),
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
/// recording id, the session recorded and the two positions and exits; the recording
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

/// `cavalry replay`: a client of the driver that subscribes to a replay stream, starts
/// the replay and prints one line per message, `<position> <body>`, the body as UTF-8
/// when it is printable and as hex otherwise, a fragment of a longer message marked
/// `fragment begin`, `fragment` or `fragment end`, until the replay ends; then where it
/// started and ended and how many messages it printed. An instant is resolved through
/// the recording's time index in the log directory before the command is sent
/// (`spec/durable-log.md`, "Time index"); `--to` becomes the length, `--follow`
/// follows a live recording, and neither plays to the recording's extent. Exit 1 with
/// the driver's message when the command is refused, and when the replay failed
/// (`spec/client-protocol.md`, "Error codes" and `replay stopped`).
fn replay(config: &Config, request: ReplayRequest) -> ExitCode {
    let fail = |error: &dyn std::fmt::Display| {
        eprintln!("cavalry replay: {error}");
        ExitCode::FAILURE
    };
    let resolve = |point: Point, what: &str| -> Result<i64, String> {
        match point {
            Point::Position(position) => Ok(position),
            Point::Instant(instant) => {
                let log_dir = config.log_dir.as_deref().ok_or_else(|| {
                    "an instant needs the log directory, which this host has no default for: \
                     pass --log-dir, or a position"
                        .to_string()
                })?;
                let resolved = if what == "--from" {
                    index::position_at(log_dir, request.recording_id, instant)
                } else {
                    index::position_after(log_dir, request.recording_id, instant)
                };
                resolved.map_err(|error| format!("{what}: {error}"))
            }
        }
    };
    let from = match resolve(request.from, "--from") {
        Ok(position) => position,
        Err(error) => return fail(&error),
    };
    let length = if request.follow {
        FOLLOW_LENGTH
    } else {
        match request.to.map(|to| resolve(to, "--to")) {
            None => NULL_LENGTH,
            Some(Ok(to)) if to >= from => to - from,
            Some(Ok(to)) => {
                return fail(&format!(
                    "--to {to} is before --from {from}; nothing to play"
                ));
            }
            Some(Err(error)) => return fail(&error),
        }
    };
    let mut client = match Cavalry::connect_to(&config.dir) {
        Ok(client) => client,
        Err(error) => return fail(&error),
    };
    // A stream of the command's own, from the client id, which no two clients of one
    // driver share, so that two replays printed at once never mix.
    let stream_id = request
        .stream_id
        .unwrap_or_else(|| -1 - (client.client_id() % 1_000_000_000) as i32);
    let outcome = replay_to_stdout(&mut client, &request, from, length, stream_id);
    let closed = client.close();
    match outcome {
        Err(error) => fail(&error),
        Ok(()) => match closed {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(&error),
        },
    }
}

/// The subscription, the replay, and the lines, until the replay ended and the image
/// has been read to the end position.
fn replay_to_stdout(
    client: &mut Cavalry,
    request: &ReplayRequest,
    from: i64,
    length: i64,
    stream_id: i32,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write as _;
    let mut subscription = client.add_subscription("cavalry:ipc", stream_id)?;
    let replay =
        client.start_replay(request.recording_id, from, length, stream_id, "cavalry:ipc")?;
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    let mut messages = 0u64;
    let mut line = String::new();
    let mut last = replay.start_position();
    let mut read_to = replay.start_position();
    let ended = loop {
        let mut io_error = None;
        let polled = subscription.poll(
            |body, header| {
                line.clear();
                format_line(&mut line, last, body, header);
                last += header.aligned_length() as i64;
                if header.flags().contains(cavalry::Flags::END) {
                    messages += 1;
                }
                if let Err(error) = writeln!(out, "{line}") {
                    io_error.get_or_insert(error);
                }
            },
            256,
        );
        if let Some(error) = io_error {
            return Err(error.into());
        }
        let delivered = polled?;
        if let Some(image) = subscription.images().first() {
            read_to = image.position();
        }
        if let Some((end, reason)) = replay.ended()
            && (read_to >= end || subscription.images().is_empty())
        {
            break (end, reason);
        }
        if delivered == 0 {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    };
    out.flush()?;
    let (end, reason) = ended;
    let said = match reason {
        Some(cavalry::ReplayStopReason::Ended) => "ended".to_string(),
        Some(cavalry::ReplayStopReason::Stopped) => "stopped by command".to_string(),
        Some(cavalry::ReplayStopReason::NotConnected) => {
            "ended for want of a subscriber".to_string()
        }
        Some(cavalry::ReplayStopReason::Failed) => "failed".to_string(),
        None => "ended with a reason this version does not know".to_string(),
    };
    println!(
        "replay {} of recording {} {said} at position {end}: {messages} messages from position {}",
        replay.replay_session_id(),
        request.recording_id,
        replay.start_position()
    );
    if reason == Some(cavalry::ReplayStopReason::Failed) {
        return Err(format!(
            "the replay failed at position {end}: a segment file could not be read or a frame \
             failed a check; `cavalry doctor` names the recording (replay.failed); see \
             docs/reference/client-protocol.md#events"
        )
        .into());
    }
    Ok(())
}

/// One line: the frame's position, `fragment begin|fragment|fragment end` for a
/// fragment of a longer message, then the body as UTF-8 when it is UTF-8 with no
/// control character but a tab, else as lowercase hex.
fn format_line(line: &mut String, position: i64, body: &[u8], header: &cavalry::DataFrameHeader) {
    use std::fmt::Write as _;
    let _ = write!(line, "{position}");
    let flags = header.flags();
    let begin = flags.contains(cavalry::Flags::BEGIN);
    let end = flags.contains(cavalry::Flags::END);
    match (begin, end) {
        (true, true) => {}
        (true, false) => line.push_str(" fragment begin"),
        (false, false) => line.push_str(" fragment"),
        (false, true) => line.push_str(" fragment end"),
    }
    line.push(' ');
    match std::str::from_utf8(body) {
        Ok(text) if !text.chars().any(|c| c.is_control() && c != '\t') => line.push_str(text),
        _ => {
            for byte in body {
                let _ = write!(line, "{byte:02x}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

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

    #[test]
    fn replay_takes_a_recording_a_from_and_the_rest_and_nothing_else() {
        match parsed(&[
            "replay",
            "--recording",
            "3",
            "--from",
            "4096",
            "--to",
            "8192",
        ]) {
            Invocation::Replay(flags, request) => {
                assert!(flags.dir.is_none());
                assert_eq!(
                    request,
                    ReplayRequest {
                        recording_id: 3,
                        from: Point::Position(4096),
                        to: Some(Point::Position(8192)),
                        stream_id: None,
                        follow: false,
                    }
                );
            }
            _ => panic!("not a replay"),
        }
        match parsed(&[
            "replay",
            "--recording=0",
            "--from",
            "2026-10-29T09:36:00Z",
            "--stream",
            "-7",
            "--follow",
            "--log-dir",
            "/l",
        ]) {
            Invocation::Replay(flags, request) => {
                assert_eq!(flags.log_dir.as_deref(), Some(Path::new("/l")));
                assert_eq!(request.recording_id, 0);
                assert_eq!(request.from, Point::Instant(1_793_266_560_000_000_000));
                assert_eq!(
                    (request.to, request.stream_id, request.follow),
                    (None, Some(-7), true)
                );
            }
            _ => panic!("not a replay"),
        }
        match parsed(&[
            "replay",
            "--recording",
            "0",
            "--from",
            "@1700000000000000000",
        ]) {
            Invocation::Replay(_, request) => {
                assert_eq!(request.from, Point::Instant(1_700_000_000_000_000_000));
            }
            _ => panic!("not a replay"),
        }
        for args in [
            ["replay"].as_slice(),
            ["replay", "--recording", "0"].as_slice(),
            ["replay", "--from", "0"].as_slice(),
            ["replay", "--recording", "-1", "--from", "0"].as_slice(),
            ["replay", "--recording", "0", "--from", "33"].as_slice(),
            ["replay", "--recording", "0", "--from", "yesterday"].as_slice(),
            [
                "replay",
                "--recording",
                "0",
                "--from",
                "0",
                "--to",
                "64",
                "--follow",
            ]
            .as_slice(),
            [
                "replay",
                "--recording",
                "0",
                "--from",
                "0",
                "--follow",
                "--follow",
            ]
            .as_slice(),
            ["replay", "--recording", "0", "--from", "0", "--stream", "x"].as_slice(),
            ["replay", "--recording", "0", "--from", "0", "--stop", "1"].as_slice(),
            ["replay", "--recording", "0", "--from", "0", "7"].as_slice(),
            ["record", "1", "--follow"].as_slice(),
        ] {
            assert!(
                matches!(parsed(args), Invocation::Wrong(_)),
                "{args:?} should be refused"
            );
        }
    }

    #[test]
    fn an_rfc_3339_time_resolves_to_nanoseconds_since_the_epoch() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339("1970-01-01T00:00:01.5Z"), Some(1_500_000_000));
        assert_eq!(
            parse_rfc3339("2026-10-29T09:36:00Z"),
            Some(1_793_266_560_000_000_000)
        );
        assert_eq!(
            parse_rfc3339("2026-10-29 09:36:00z"),
            Some(1_793_266_560_000_000_000)
        );
        assert_eq!(
            parse_rfc3339("2026-10-29t10:36:00+01:00"),
            Some(1_793_266_560_000_000_000)
        );
        assert_eq!(
            parse_rfc3339("2026-10-29T04:36:00.123456789-05:00"),
            Some(1_793_266_560_123_456_789)
        );
        assert_eq!(
            parse_rfc3339("2000-02-29T00:00:00Z"),
            Some(951_782_400_000_000_000)
        );
        assert_eq!(parse_rfc3339("1969-12-31T23:59:59Z"), Some(-1_000_000_000));
        for bad in [
            "",
            "2026-10-29",
            "2026-10-29T09:36:00",
            "2026-13-01T00:00:00Z",
            "2026-02-30T00:00:00Z",
            "2026-10-29T24:00:00Z",
            "2026-10-29T09:36:00.Z",
            "2026-10-29T09:36:00.1234567890Z",
            "2026-10-29T09:36:00+25:00",
            "2026-10-29T09:36:00Zx",
            "2026-10-2T09:36:00Z",
        ] {
            assert_eq!(parse_rfc3339(bad), None, "{bad}");
        }
        assert_eq!(parse_point("4096"), Ok(Point::Position(4096)));
        assert!(parse_point("4100").is_err());
        assert!(parse_point("@abc").is_err());
    }

    #[test]
    fn a_line_is_the_position_the_fragment_mark_and_the_body_as_text_or_hex() {
        use cavalry::{DataFrameHeader, Flags, FrameType};
        let header =
            |flags: Flags| DataFrameHeader::new(FrameType::Data, flags, 40).expect("a header");
        let mut line = String::new();
        format_line(&mut line, 64, b"hello\tworld", &header(Flags::UNFRAGMENTED));
        assert_eq!(line, "64 hello\tworld");
        line.clear();
        format_line(
            &mut line,
            128,
            &[0, 1, 2, 255],
            &header(Flags::UNFRAGMENTED),
        );
        assert_eq!(line, "128 000102ff");
        line.clear();
        format_line(&mut line, 192, b"line\nbreak", &header(Flags::BEGIN));
        assert_eq!(line, "192 fragment begin 6c696e650a627265616b");
        line.clear();
        format_line(&mut line, 256, b"mid", &header(Flags::NONE));
        assert_eq!(line, "256 fragment mid");
        line.clear();
        format_line(&mut line, 320, b"end", &header(Flags::END));
        assert_eq!(line, "320 fragment end end");
    }
}
