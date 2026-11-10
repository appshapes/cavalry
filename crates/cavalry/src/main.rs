//! The `cavalry` binary (ADR 0002): `cavalry run` starts a driver and stops it on
//! SIGINT, SIGTERM or SIGHUP; `cavalry doctor` explains the driver from its counters;
//! `cavalry record <stream id>` records a stream to the durable log and `cavalry record
//! --stop <recording id>` stops it (ADR 0025), `--truncate <id> --to <point>`, `--purge <id>
//! --before <point>` and `--delete <id>` cut, purge and delete a stopped one (ADR 0031);
//! `cavalry replay --recording <id> --from
//! <position, instant or start>` plays a recording back and prints every message (ADR 0030),
//! or, with `--channel <a cavalry:udp channel>`, plays it to a subscriber on that
//! channel's endpoint and prints the summary alone, exit 1 when no subscriber answered
//! or the driver went before the end (ADR 0043).
//! `console` arrives with its feature. No arguments and no configuration are the primary way to run it (CLAUDE.md,
//! invariant 5): the directory is `spec/client-protocol.md`'s default and the log
//! directory `Driver::default_log_dir()`'s. The keys of `config.toml` are read from the
//! environment, and `--dir` and `--log-dir` on top (ADR 0020; `docs/reference/config.md`).
#![deny(missing_docs, rustdoc::broken_intra_doc_links)]

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use cavalry::cluster::ConsensusModule;
use cavalry::config::Config;
use cavalry::{
    Cavalry, ClientError, Driver, FOLLOW_LENGTH, IdleStrategy, Image, NULL_LENGTH, NULL_POSITION,
    Positions, Subscription, doctor, index,
};

const USAGE: &str = "usage: cavalry <command> [--dir <path>] [--log-dir <path>]

  run                      start a driver; Ctrl-C or SIGTERM stops it; with
                           CAVALRY_CLUSTER_MEMBERS and CAVALRY_CLUSTER_MEMBER_ID set,
                           the driver is that member of the cluster
  doctor                   explain the driver from its counters; exits 1 when a check
                           failed
  record <stream id>       record the live publication on that stream to the durable
                           log; prints the recording id (--channel <uri> for another
                           channel than cavalry:ipc)
  record --stop <id>       stop that recording
  record --truncate <id> --to <position, instant or start>
                           cut that stopped recording at the point: the bytes past it
                           go, and its stop moves there
  record --purge <id> --before <position or instant>
                           remove that stopped recording's segment files before the
                           point; an instant is rounded down to a segment base, so no
                           byte recorded after it goes
  record --delete <id>     remove every file of that stopped recording; its id is never
                           reused
  replay --recording <id> --from <position, instant or start> [--to <position or instant>]
         [--stream <id>] [--follow] [--channel <uri>]
                           play that recording back from the position, or from the
                           instant resolved through its time index, and print one line
                           per message, the position and the body, until the replay
                           ends: at --to, at the recording's extent, or, with --follow,
                           when the recording stops; an instant is an RFC 3339 time
                           (2026-10-29T09:36:00Z) or @<nanoseconds since the Unix
                           epoch>; --from start is the recording's start position,
                           whatever it is, alone or with --follow, not with --to, whose
                           length needs the position; --stream names the replay stream
                           id, else one of the command's own; --channel plays it to
                           that channel instead of cavalry:ipc: on a cavalry:udp
                           channel the recording goes to a subscriber on that endpoint,
                           on this host or another, which needs --stream, and the
                           command prints the summary alone when the replay ends, exit
                           1 when no subscriber on the endpoint answered it within the
                           replay connect timeout, 5 s, or the driver went before the
                           end
  --version                print the version

  --dir <path>      the driver directory, for every command; the default is
                    /dev/shm/cavalry-<user> on Linux and <tmp>/cavalry-<user> elsewhere
                    (docs/reference/client-protocol.md#the-driver-directory)
  --log-dir <path>  the log directory, for run, doctor and replay; the default is
                    $XDG_DATA_HOME/cavalry/log, else ~/.local/share/cavalry/log, and
                    /var/lib/cavalry/log as root (docs/reference/config.md#logdir)

Every configuration key is read from its environment variable, CAVALRY_DIR,
CAVALRY_LOG_DIR, CAVALRY_CONDUCTOR_IDLE_STRATEGY, CAVALRY_CLUSTER_MEMBERS and
CAVALRY_CLUSTER_MEMBER_ID in this version; a flag wins over its variable
(docs/reference/config.md). The command console is not built yet; see
crates/cavalry/README.md.";

/// The channel `replay` plays to unless `--channel` names another.
const IPC_CHANNEL: &str = "cavalry:ipc";

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
    Truncate { recording_id: i64, to: Point },
    Purge { recording_id: i64, before: Point },
    Delete { recording_id: i64 },
}

/// A point in a recording as the command line names it: a position, an instant the
/// time index resolves, or the recording's start, which `--from start` names without
/// knowing it (card 160).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Point {
    Position(i64),
    /// Nanoseconds since the Unix epoch.
    Instant(i64),
    /// The recording's start position, whatever it is: sent as the null position.
    Start,
}

/// What `cavalry replay` asked for.
#[derive(Debug, PartialEq, Eq)]
struct ReplayRequest {
    recording_id: i64,
    from: Point,
    to: Option<Point>,
    stream_id: Option<i32>,
    follow: bool,
    /// The replay channel: `cavalry:ipc` unless `--channel` named another.
    channel: String,
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
/// `record` with its stream id and `--channel <uri>`, or `--stop <id>`, `--truncate <id>
/// --to <point>`, `--purge <id> --before <point>` or `--delete <id>`, `replay` with its
/// `--recording`, `--from`, `--to`, `--stream`, `--follow` and `--channel`, and nothing
/// else.
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
            "--dir <path>, --log-dir <path>, a stream id with --channel <uri>, --stop <recording \
             id>, --truncate <id> with --to <position or instant>, --purge <id> with --before \
             <position or instant>, and --delete <id>"
        }
        "replay" => {
            "--dir <path>, --log-dir <path>, --recording <id>, --from <position, instant or start>, \
             --to <position or instant>, --stream <id>, --follow and --channel <uri>"
        }
        _ => "--dir <path> and --log-dir <path>",
    };
    let mut flags = Flags::default();
    let mut stream_id: Option<i32> = None;
    let mut stop: Option<i64> = None;
    let mut truncate: Option<i64> = None;
    let mut purge: Option<i64> = None;
    let mut delete: Option<i64> = None;
    let mut before: Option<Point> = None;
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
            "--stop" | "--truncate" | "--purge" | "--delete" if command == "record" => {
                let slot = match name.as_str() {
                    "--stop" => &mut stop,
                    "--truncate" => &mut truncate,
                    "--purge" => &mut purge,
                    _ => &mut delete,
                };
                if slot.is_some() {
                    return twice(&name);
                }
                match value.parse::<i64>() {
                    Ok(id) if id >= 0 => *slot = Some(id),
                    _ => {
                        return Invocation::Wrong(format!(
                            "{name} needs a recording id, a whole number from 0; got `{value}`"
                        ));
                    }
                }
            }
            "--to" | "--before" if command == "record" => {
                let slot = if name == "--to" { &mut to } else { &mut before };
                if slot.is_some() {
                    return twice(&name);
                }
                match parse_point(&value) {
                    Ok(point) => *slot = Some(point),
                    Err(why) => return Invocation::Wrong(format!("{name}: {why}")),
                }
            }
            "--channel" if command == "record" || command == "replay" => {
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
            // Over another channel than cavalry:ipc the subscriber is elsewhere and
            // reads a stream it chose, so the command cannot pick one of its own.
            (Some(_), Some(_))
                if channel
                    .as_deref()
                    .is_some_and(|channel| channel != IPC_CHANNEL)
                    && stream_id.is_none() =>
            {
                Invocation::Wrong(
                    "replay --channel with a channel other than cavalry:ipc needs --stream <id>: \
                     the subscriber on that channel's endpoint reads the stream it chose, and \
                     the command cannot pick one of its own; see \
                     docs/how-to/replay-a-recording.md#when-it-does-not-work"
                        .to_string(),
                )
            }
            (Some(recording_id), Some(from)) => Invocation::Replay(
                flags,
                ReplayRequest {
                    recording_id,
                    from,
                    to,
                    stream_id,
                    follow,
                    channel: channel.unwrap_or_else(|| IPC_CHANNEL.to_string()),
                },
            ),
            (None, _) => Invocation::Wrong("replay needs --recording <id>".to_string()),
            (_, None) => {
                Invocation::Wrong("replay needs --from <position, instant or start>".to_string())
            }
        },
        _ => {
            // One action: a stream id to start, or one of the four id flags.
            let given = [
                stream_id.map(|_| "a stream id"),
                stop.map(|_| "--stop"),
                truncate.map(|_| "--truncate"),
                purge.map(|_| "--purge"),
                delete.map(|_| "--delete"),
            ];
            let names: Vec<&str> = given.iter().flatten().copied().collect();
            if names.len() > 1 {
                return Invocation::Wrong(format!(
                    "record takes one of a stream id, --stop, --truncate, --purge and --delete; \
                     got {}",
                    names.join(" and ")
                ));
            }
            if channel.is_some() && stream_id.is_none() {
                return Invocation::Wrong(
                    "--channel goes with a stream id, not with --stop, --truncate, --purge or \
                     --delete"
                        .to_string(),
                );
            }
            if to.is_some() && truncate.is_none() {
                return Invocation::Wrong("--to goes with --truncate <id>".to_string());
            }
            if before.is_some() && purge.is_none() {
                return Invocation::Wrong("--before goes with --purge <id>".to_string());
            }
            let action = if let Some(stream_id) = stream_id {
                RecordAction::Start {
                    stream_id,
                    channel: channel.unwrap_or_else(|| "cavalry:ipc".to_string()),
                }
            } else if let Some(recording_id) = stop {
                RecordAction::Stop { recording_id }
            } else if let Some(recording_id) = truncate {
                let Some(to) = to else {
                    return Invocation::Wrong(
                        "--truncate needs --to <position or instant>".to_string(),
                    );
                };
                RecordAction::Truncate { recording_id, to }
            } else if let Some(recording_id) = purge {
                let Some(before) = before else {
                    return Invocation::Wrong(
                        "--purge needs --before <position or instant>".to_string(),
                    );
                };
                RecordAction::Purge {
                    recording_id,
                    before,
                }
            } else if let Some(recording_id) = delete {
                RecordAction::Delete { recording_id }
            } else {
                return Invocation::Wrong(
                    "record needs a stream id, --stop <id>, --truncate <id> --to <point>, \
                     --purge <id> --before <point> or --delete <id>"
                        .to_string(),
                );
            };
            Invocation::Record(flags, action)
        }
    }
}

/// A point of a recording as a position: an instant is resolved through the time index
/// in the log directory, to the first entry's position after it (`after`, where a
/// `--to` keeps every byte written at the instant) or to the entry's at or before it
/// (where a `--before` purges no byte recorded after the instant), which needs
/// `--log-dir` on a host with no default.
fn resolve_point(
    config: &Config,
    recording_id: i64,
    point: Point,
    what: &str,
    after: bool,
) -> Result<i64, String> {
    match point {
        Point::Position(position) => Ok(position),
        Point::Instant(instant) => {
            let log_dir = log_dir_for(config, "an instant")?;
            let resolved = if after {
                index::position_after(log_dir, recording_id, instant)
            } else {
                index::position_at(log_dir, recording_id, instant)
            };
            resolved.map_err(|error| format!("{what}: {error}"))
        }
        // `start` is the retained position, read from the descriptor: a truncate to it
        // leaves the recording empty, and a purge before it has nothing to remove.
        Point::Start => descriptor_of(config, recording_id, what, "`start`")
            .map(|descriptor| descriptor.retained_position()),
    }
}

/// The log directory a point needs, or why there is none to read.
fn log_dir_for<'a>(config: &'a Config, point: &str) -> Result<&'a Path, String> {
    config.log_dir.as_deref().ok_or_else(|| {
        format!(
            "{point} needs the log directory, which this host has no default for: pass \
             --log-dir, or a position"
        )
    })
}

/// A recording's descriptor from the catalog in the log directory, for a point the
/// command line resolves itself.
fn descriptor_of(
    config: &Config,
    recording_id: i64,
    what: &str,
    point: &str,
) -> Result<cavalry_log::layout::Descriptor, String> {
    let log_dir = log_dir_for(config, point)?;
    let catalog = cavalry_log::catalog::CatalogReader::open(log_dir)
        .map_err(|error| format!("{what}: {error}"))?;
    catalog
        .get(recording_id)
        .map_err(|error| format!("{what}: {error}"))?
        .ok_or_else(|| format!("{what}: recording {recording_id} is not in the catalog"))
}

/// The segment base at or before `position` in `recording_id`, from its descriptor in
/// the catalog: what a purge sends for an instant, so that no byte recorded after the
/// instant is removed (ADR 0031).
fn segment_base_at_or_before(
    config: &Config,
    recording_id: i64,
    position: i64,
) -> Result<i64, String> {
    let descriptor = descriptor_of(config, recording_id, "--before", "an instant")?;
    cavalry_log::layout::locate(&descriptor, position)
        .map(|located| located.segment_base_position)
        .map_err(|error| format!("--before: {error}"))
}

/// A point of a recording from the command line: `start` is the recording's start
/// position, whatever it is; a whole number is a position, a multiple of 32 from 0;
/// `@<n>` is an instant, n nanoseconds since the Unix epoch; anything else is an RFC
/// 3339 time, `2026-10-29T09:36:00Z`, with an optional fraction of a second and an
/// offset `Z` or `±HH:MM`.
fn parse_point(value: &str) -> Result<Point, String> {
    if value == "start" {
        return Ok(Point::Start);
    }
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

/// Why `cavalry run` stops: a signal, or the cluster member it runs that cannot go on.
enum Stop {
    Signal,
    Member(String),
}

/// Installs the signal handler, then starts a driver and waits for a signal; the driver
/// closes on the way out, leaving −1 in the heartbeat so that doctor and the clients
/// know. The handler goes first, so that no signal between the start and the handler
/// kills the process with a fresh heartbeat behind it, and a handler that cannot be
/// installed refuses to start at all, with the directory untouched. With
/// `cluster.members` and `cluster.member.id` set, the driver is a cluster member: its
/// consensus module starts beside it, on a thread of its own, and stops before it
/// (ADR 0060; ADR 0063).
fn run(config: &Config) -> ExitCode {
    let member = match (&config.cluster_members, config.cluster_member_id) {
        (None, None) => None,
        (Some(members), Some(id)) => Some((members.as_str(), id)),
        (members, _) => {
            // A member id of -1, its default, is read as unset (`Config::from_vars`).
            let (set, unset, missing) = if members.is_some() {
                (
                    "CAVALRY_CLUSTER_MEMBERS",
                    "CAVALRY_CLUSTER_MEMBER_ID",
                    "is unset or -1, no member",
                )
            } else {
                (
                    "CAVALRY_CLUSTER_MEMBER_ID",
                    "CAVALRY_CLUSTER_MEMBERS",
                    "is not",
                )
            };
            eprintln!(
                "cavalry run: {set} is set and {unset} {missing}, so the driver did not \
                 start: a cluster member needs both, the cluster's members string and this \
                 host's id in it; set {unset}, or unset {set} for a driver in no cluster; \
                 `cavalry doctor` checks both; see docs/reference/config.md#clustermembers"
            );
            return ExitCode::FAILURE;
        }
    };
    let (stop, stopped) = std::sync::mpsc::channel::<Stop>();
    let signal = stop.clone();
    if let Err(error) = ctrlc::set_handler(move || {
        let _ = signal.send(Stop::Signal);
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
    let member_stop = Arc::new(AtomicBool::new(false));
    let mut member_thread = None;
    let mut failed = false;
    if let Some((members, id)) = member {
        match start_member(
            dir,
            log_dir,
            members,
            id,
            config.conductor_idle_strategy,
            Arc::clone(&member_stop),
            stop,
        ) {
            Ok(thread) => member_thread = Some(thread),
            Err(error) => {
                eprintln!("cavalry run: {error}");
                failed = true;
            }
        }
    }
    if !failed {
        match stopped.recv() {
            Ok(Stop::Signal) => eprintln!("cavalry run: stopping"),
            Ok(Stop::Member(error)) => {
                eprintln!("cavalry run: the cluster member stopped, so the driver stops: {error}");
                failed = true;
            }
            // Every sender is gone without a signal: a bug, never silent.
            Err(_) => eprintln!("cavalry run: the signal handler went away; stopping"),
        }
    }
    // The member first: it is a client of the driver, and closes its client while the
    // driver is there to hear it.
    member_stop.store(true, Ordering::Release);
    if let Some(thread) = member_thread
        && thread.join().is_err()
    {
        eprintln!("cavalry run: the cluster member's thread panicked");
        failed = true;
    }
    match driver.close() {
        Ok(()) => {
            eprintln!(
                "cavalry run: closed; {} stays for doctor, and the next driver remakes it",
                dir.display()
            );
            if failed {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(error) => {
            eprintln!("cavalry run: {error}");
            ExitCode::FAILURE
        }
    }
}

/// How long a cluster member's duty cycle may fail without a cycle that works before
/// `cavalry run` gives the member up and stops: the leader heartbeat timeout, after
/// which the cluster has counted the member out anyway.
const MEMBER_FAILURE_TIMEOUT: Duration = Duration::from_secs(10);

/// Starts the cluster member `id` of `members` as a client of the driver in `dir`,
/// with its record of terms in `log_dir`, and runs its duty cycle on a thread of its
/// own with the conductor's idle strategy, until `stop` is set. An error of the start
/// is returned; a duty cycle that fails is tried again every 100 ms, said once for each
/// different error, and given up after [`MEMBER_FAILURE_TIMEOUT`] of failures on end,
/// which `stopped` is told.
fn start_member(
    dir: &Path,
    log_dir: &Path,
    members: &str,
    id: i32,
    strategy: IdleStrategy,
    stop: Arc<AtomicBool>,
    stopped: std::sync::mpsc::Sender<Stop>,
) -> Result<std::thread::JoinHandle<()>, String> {
    let mut module =
        ConsensusModule::start(dir, log_dir, members, id).map_err(|error| error.to_string())?;
    // No command prints a member's counters yet, so the line names the file and the
    // labels a client reads them by, not a command that would show nothing.
    eprintln!(
        "cavalry run: cluster member {id} started: it follows or leads as \
         docs/reference/cluster.md#roles-and-the-election says; its role and its election \
         state are the counters labelled `cluster.role {id}` and `cluster.election_state \
         {id}` in {}, as docs/reference/counters.md describes them",
        dir.join("counters").display()
    );
    std::thread::Builder::new()
        .name("cavalry-consensus".to_string())
        .spawn(move || {
            let mut idle = Idle::new(strategy);
            let mut failing_since: Option<Instant> = None;
            let mut last_error = String::new();
            while !stop.load(Ordering::Acquire) {
                match module.do_work() {
                    Ok(work) => {
                        failing_since = None;
                        idle.idle(work);
                    }
                    Err(error) => {
                        let text = error.to_string();
                        if text != last_error {
                            eprintln!("cavalry run: cluster member {id}: {text}");
                            last_error = text;
                        }
                        let since = *failing_since.get_or_insert_with(Instant::now);
                        if since.elapsed() >= MEMBER_FAILURE_TIMEOUT {
                            let _ = stopped.send(Stop::Member(last_error));
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(100));
                    }
                }
            }
            if let Err(error) = module.close() {
                eprintln!("cavalry run: cluster member {id}: {error}");
            }
        })
        .map_err(|error| {
            format!(
                "the cluster member's thread could not start: {error}; free a thread on this \
                 host and run cavalry again"
            )
        })
}

/// The idle strategy of the consensus module's thread, the conductor's: busy-spin never
/// leaves the CPU, yield gives it up on every idle cycle, and backoff spins, then
/// yields, then sleeps from 1 µs to 1 ms, doubling, until there is work again.
struct Idle {
    strategy: IdleStrategy,
    spins: u32,
    yields: u32,
    park: Duration,
}

impl Idle {
    const MAX_SPINS: u32 = 10;
    const MAX_YIELDS: u32 = 20;
    const MIN_PARK: Duration = Duration::from_micros(1);
    const MAX_PARK: Duration = Duration::from_millis(1);

    fn new(strategy: IdleStrategy) -> Self {
        Self {
            strategy,
            spins: 0,
            yields: 0,
            park: Self::MIN_PARK,
        }
    }

    fn idle(&mut self, work: usize) {
        if work > 0 {
            self.spins = 0;
            self.yields = 0;
            self.park = Self::MIN_PARK;
            return;
        }
        match self.strategy {
            IdleStrategy::BusySpin => std::hint::spin_loop(),
            IdleStrategy::Yield => std::thread::yield_now(),
            IdleStrategy::Backoff => {
                if self.spins < Self::MAX_SPINS {
                    self.spins += 1;
                    std::hint::spin_loop();
                } else if self.yields < Self::MAX_YIELDS {
                    self.yields += 1;
                    std::thread::yield_now();
                } else {
                    std::thread::sleep(self.park);
                    self.park = (self.park * 2).min(Self::MAX_PARK);
                }
            }
        }
    }
}

/// Prints the report and exits with its verdict: 0 when no check failed, 1 otherwise.
fn doctor(config: &Config) -> ExitCode {
    let report = doctor::examine_config(config);
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
        RecordAction::Truncate { recording_id, to } => {
            match resolve_point(config, recording_id, to, "--to", true) {
                Ok(position) => client
                    .truncate_recording(recording_id, position)
                    .map(|()| println!("recording {recording_id} truncated to {position}")),
                Err(error) => {
                    eprintln!("cavalry record: {error}");
                    let _ = client.close();
                    return ExitCode::FAILURE;
                }
            }
        }
        RecordAction::Purge {
            recording_id,
            before,
        } => {
            // An instant resolves to a frame boundary, which is almost never a segment
            // base: the base at or before it is what goes, and both are printed.
            let base = match before {
                Point::Position(position) => Ok(position),
                Point::Start => resolve_point(config, recording_id, before, "--before", false),
                Point::Instant(_) => resolve_point(config, recording_id, before, "--before", false)
                    .and_then(|position| {
                        segment_base_at_or_before(config, recording_id, position)
                            .map(|base| (position, base))
                    })
                    .map(|(position, base)| {
                        println!(
                            "--before resolved to position {position}; purging before the \
                             segment base {base}"
                        );
                        base
                    }),
            };
            match base {
                Ok(base) => client
                    .purge_segments(recording_id, base)
                    .map(|()| println!("recording {recording_id} purged before {base}")),
                Err(error) => {
                    eprintln!("cavalry record: {error}");
                    let _ = client.close();
                    return ExitCode::FAILURE;
                }
            }
        }
        RecordAction::Delete { recording_id } => client
            .purge_recording(recording_id)
            .map(|()| println!("recording {recording_id} deleted")),
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
/// the replay and prints one line per message, `<position> <body>`, the position from
/// the frame's header, the body as UTF-8 when it is printable and as hex otherwise, a
/// fragment of a longer message marked `fragment begin`, `fragment` or `fragment end`,
/// until the replay ends; then where it started and ended and how many messages it
/// printed. The position and the length are `replay_range`'s, settled before the
/// command is sent: an instant is resolved through the recording's time index in the
/// log directory (`spec/durable-log.md`, "Time index"), `--to` becomes the length,
/// `--follow` follows a live recording, and neither plays to the recording's extent.
/// With `--channel` naming another channel than `cavalry:ipc`, `replay_to_channel`
/// instead: no subscription, no lines, the summary alone once the replay ends.
/// Exit 1 with the driver's message when the command is refused,
/// when the replay failed (`spec/client-protocol.md`, "Error codes" and `replay
/// stopped`), when the replay's log went before the command had read it to the end,
/// with the positions it did not print, and, over another channel, when no subscriber
/// on the endpoint answered the replay or the client closed before its end
/// (invariant 1).
fn replay(config: &Config, request: ReplayRequest) -> ExitCode {
    let fail = |error: &dyn std::fmt::Display| {
        eprintln!("cavalry replay: {error}");
        ExitCode::FAILURE
    };
    let (from, length) = match replay_range(&request, config.log_dir.as_deref()) {
        Ok(range) => range,
        Err(error) => return fail(&error),
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

/// The position and the length `cavalry replay` sends, from the request and the log
/// directory, settled before the command goes so that a refusal costs no connection: a
/// position as it is; `--from start` as the null position, which the driver resolves to
/// the recording's start (`spec/client-protocol.md`, "start replay"); an instant through
/// the recording's time index. The length is `--to` less `--from`, `NULL_LENGTH` for
/// the extent, `FOLLOW_LENGTH` with `--follow`. Refused, each saying what to pass
/// instead: `--to start`, an end where a replay begins; `--from start` with `--to`,
/// since the length needs the start position and the command does not know it, so
/// `--to` less the null position would play one frame past `--to` for a recording that
/// started at 0 and the whole start offset past it for one that started later; and
/// `--to` before `--from`, which the driver would refuse as a length below −1 (code 7)
/// without saying why.
fn replay_range(request: &ReplayRequest, log_dir: Option<&Path>) -> Result<(i64, i64), String> {
    if request.to == Some(Point::Start) {
        return Err(
            "--to start: the start is where a replay begins, not where it ends".to_string(),
        );
    }
    if request.from == Point::Start && request.to.is_some() {
        return Err(
            "--from start takes no --to: the length to --to is counted from the start \
                    position, which only the driver knows; pass --from <position> instead; a \
                    position the recording does not hold is refused with error 23, whose message \
                    names the start; see docs/how-to/replay-a-recording.md#when-it-does-not-work"
                .to_string(),
        );
    }
    let resolve = |point: Point, what: &str| -> Result<i64, String> {
        match point {
            Point::Position(position) => Ok(position),
            // The driver resolves the null position to the recording's start (spec,
            // "start replay"); only `--from` reaches here with it.
            Point::Start => Ok(NULL_POSITION),
            Point::Instant(instant) => {
                let log_dir = log_dir.ok_or_else(|| {
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
    let from = resolve(request.from, "--from")?;
    let length = if request.follow {
        FOLLOW_LENGTH
    } else {
        match request.to {
            None => NULL_LENGTH,
            Some(to) => {
                let to = resolve(to, "--to")?;
                if to < from {
                    return Err(format!(
                        "--to {to} is before --from {from}; nothing to play"
                    ));
                }
                to - from
            }
        }
    };
    Ok((from, length))
}

/// The replay's image among the subscription's: the one with the replay publication's
/// session id. `--stream` may name a stream with other publications on it, whose images
/// and frames are not the replay's.
fn replay_image(subscription: &Subscription, session_id: i32) -> Option<&Image> {
    subscription
        .images()
        .iter()
        .find(|image| image.session_id() == session_id)
}

/// The subscription, the replay, and the lines, until the replay ended and its image
/// has been read to the end position. Each line's position is the frame's, from its
/// header and the image's positions (`spec/log-buffer.md`, "Positions"): a padding
/// frame, at the end of a term or over an aborted or unblocked claim, is replayed as
/// padding, prints nothing and moves the position, so a sum of the lines' lengths
/// would be short from the first one on. An image that goes before it was read to the
/// end is `Err`, with the positions not printed: the driver keeps an ended replay's log
/// for its subscribers for the publication linger, 5 s (`spec/log-buffer.md`,
/// "Closing"), and a command held up on its stdout for longer loses the rest, which a
/// summary that read as complete would hide (invariant 1). A channel other than
/// `cavalry:ipc` is `replay_to_channel`'s: no subscription and no lines.
fn replay_to_stdout(
    client: &mut Cavalry,
    request: &ReplayRequest,
    from: i64,
    length: i64,
    stream_id: i32,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write as _;
    if request.channel != IPC_CHANNEL {
        return replay_to_channel(client, request, from, length, stream_id);
    }
    let mut subscription = client.add_subscription(IPC_CHANNEL, stream_id)?;
    let replay = client.start_replay(request.recording_id, from, length, stream_id, IPC_CHANNEL)?;
    let session_id = replay.session_id();
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    let mut messages = 0u64;
    let mut line = String::new();
    let mut read_to = replay.start_position();
    let mut positions: Option<Positions> = None;
    let ended = loop {
        // The replay's image joins the subscription at a poll: until it has, a poll
        // with a fragment limit of 0 applies the images the driver announced and reads
        // no frame, so that the first frame printed has its position from the image.
        if positions.is_none() {
            subscription.poll(|_, _| {}, 0)?;
            positions = replay_image(&subscription, session_id).map(Image::positions);
        }
        let delivered = match positions {
            None => 0,
            Some(positions) => {
                let mut io_error = None;
                let polled = subscription.poll(
                    |body, header| {
                        if header.session_id() != session_id {
                            return;
                        }
                        let position =
                            positions.position(header.term_id(), header.term_offset() as u32);
                        line.clear();
                        format_line(&mut line, position, body, header);
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
                polled?
            }
        };
        if let Some(image) = replay_image(&subscription, session_id) {
            read_to = image.position();
        }
        if let Some((end, reason)) = replay.ended()
            && (read_to >= end || replay_image(&subscription, session_id).is_none())
        {
            break (end, reason);
        }
        if delivered == 0 {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    };
    out.flush()?;
    let (end, reason) = ended;
    let said = ended_as(reason);
    let unread = read_to < end;
    println!(
        "replay {} of recording {} {said} at position {end}: {messages} messages from position {}{}",
        replay.replay_session_id(),
        request.recording_id,
        replay.start_position(),
        if unread {
            format!(", read to position {read_to}")
        } else {
            String::new()
        }
    );
    if unread {
        return Err(format!(
            "the driver removed the replay's log before this command had read it to the end: \
             positions {read_to} to {end}, {} bytes, were not printed; an ended replay's log \
             stays for its subscribers for the publication linger, 5 s, and stdout held this \
             command up for longer; read faster, or run the command again with --from \
             {read_to}; see docs/how-to/replay-a-recording.md#when-it-does-not-work",
            end - read_to
        )
        .into());
    }
    if reason == Some(cavalry::ReplayStopReason::Failed) {
        return Err(failed_at(end).into());
    }
    Ok(())
}

/// `cavalry replay --channel <uri>` with a channel other than `cavalry:ipc`: the replay's
/// publication goes to the channel's endpoint, where a subscription on another host or
/// this one reads it (ADR 0043), so the command subscribes to nothing, starts the
/// replay, waits for its end and prints the summary line alone: where it started and
/// ended, the bytes between, the channel, and how it ended. A replay no receiver
/// answers ends after the replay connect timeout, 5 s, `for want of a subscriber`:
/// subscribe on the endpoint first, on the stream `--stream` names. The end comes from
/// `replay stopped`, which the client's conductor stores; a client that closes before
/// it arrives, because the driver went, this client timed out, the broadcast buffer
/// lapped it or an event was corrupt, stores no end, so the wait is `Err` with the
/// reason then, where the `cavalry:ipc` path's poll is `Fault::Closed`.
fn replay_to_channel(
    client: &mut Cavalry,
    request: &ReplayRequest,
    from: i64,
    length: i64,
    stream_id: i32,
) -> Result<(), Box<dyn std::error::Error>> {
    let replay = client.start_replay(
        request.recording_id,
        from,
        length,
        stream_id,
        &request.channel,
    )?;
    let (end, reason) = loop {
        // The close is read before the end: a conductor that stores the end and then
        // closes has both visible once the close is, so a close seen with no end is a
        // close that came with no `replay stopped`, and no end will come.
        let closed = client.closed();
        if let Some(ended) = replay.ended() {
            break ended;
        }
        if let Some(why) = closed {
            return Err(closed_before_the_end(&replay, request, why).into());
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    };
    println!(
        "replay {} of recording {} {} at position {end}: {} bytes from position {} on {}",
        replay.replay_session_id(),
        request.recording_id,
        ended_as(reason),
        end - replay.start_position(),
        replay.start_position(),
        request.channel
    );
    match reason {
        Some(cavalry::ReplayStopReason::Failed) => Err(failed_at(end).into()),
        // Over `cavalry:ipc` the command subscribes itself, so reason 2 cannot reach
        // `replay_to_stdout`; over another channel it is the ordinary failure, the wrong
        // port, a closed firewall or a subscriber not up yet, and a script that read
        // exit 0 for it would read success for a replay that delivered nothing
        // (invariant 1).
        Some(cavalry::ReplayStopReason::NotConnected) => {
            Err(not_connected_on(&request.channel, stream_id, replay.start_position(), end).into())
        }
        _ => Ok(()),
    }
}

/// The error of a replay over a channel other than `cavalry:ipc` that ended with
/// reason 2, for the exit code and stderr: no receiver on the endpoint answered its
/// publication for the replay connect timeout, 5 s, from its start, so nothing was
/// played, or from the moment the last one went (ADR 0043), so what was played is
/// whole as far as that receiver's image read; the message says which, what to do
/// next, and what `cavalry doctor` names on each host.
fn not_connected_on(channel: &str, stream_id: i32, start: i64, end: i64) -> String {
    let what = if end > start {
        format!(
            "the subscriber went, and the {} bytes from position {start} to position {end} \
             are whole as far as its image read; run the command again with --from at the \
             position that image reached",
            end - start
        )
    } else {
        format!(
            "nothing was played; subscribe on that endpoint first, on stream {stream_id}, \
             then run the command again"
        )
    };
    format!(
        "no subscriber on {channel}, stream {stream_id}, answered the replay within the \
         replay connect timeout, 5 s: {what}; `cavalry doctor` on this host names \
         replay.not_connected, and on the receiving host udp-subscription-no-image while \
         its subscription waits for the replay's setup frame; see \
         docs/how-to/replay-a-recording.md#when-it-does-not-work"
    )
}

/// How the summary line says a replay ended, from `replay stopped`'s reason.
fn ended_as(reason: Option<cavalry::ReplayStopReason>) -> &'static str {
    match reason {
        Some(cavalry::ReplayStopReason::Ended) => "ended",
        Some(cavalry::ReplayStopReason::Stopped) => "stopped by command",
        Some(cavalry::ReplayStopReason::NotConnected) => "ended for want of a subscriber",
        Some(cavalry::ReplayStopReason::Failed) => "failed",
        None => "ended with a reason this version does not know",
    }
}

/// The error of a replay over a channel whose client closed before `replay stopped`
/// arrived, for the exit code and stderr: [`ClientError::Closed`]'s own words, which
/// name the reason and what it means, then the replay, whose end is unknown here, so
/// no summary line names one (invariant 1), and what to do next.
fn closed_before_the_end(
    replay: &cavalry::Replay,
    request: &ReplayRequest,
    why: cavalry::Closed,
) -> String {
    format!(
        "{}; replay {} of recording {} from position {} on {} had not ended when the client \
         closed, so where it ended is unknown here; what reached the subscriber is whole up \
         to the position its image reports; run `cavalry doctor` on this host, start the \
         driver if it went, then run the command again with --from at that position; see \
         docs/how-to/replay-a-recording.md#when-it-does-not-work",
        ClientError::Closed(why),
        replay.replay_session_id(),
        request.recording_id,
        replay.start_position(),
        request.channel
    )
}

/// The error of a replay that ended with reason 3, for the exit code and stderr.
fn failed_at(end: i64) -> String {
    format!(
        "the replay failed at position {end}: a segment file could not be read or a frame \
         failed a check; `cavalry doctor` names the recording (replay.failed); see \
         docs/reference/client-protocol.md#events"
    )
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
    fn record_takes_one_retention_action_with_its_point() {
        match parsed(&["record", "--truncate", "3", "--to", "6400"]) {
            Invocation::Record(_, RecordAction::Truncate { recording_id, to }) => {
                assert_eq!((recording_id, to), (3, Point::Position(6400)));
            }
            _ => panic!("a truncate"),
        }
        match parsed(&["record", "--purge", "3", "--before=2026-10-29T09:36:00Z"]) {
            Invocation::Record(
                _,
                RecordAction::Purge {
                    recording_id,
                    before,
                },
            ) => {
                assert_eq!(recording_id, 3);
                assert_eq!(before, Point::Instant(1_793_266_560_000_000_000));
            }
            _ => panic!("a purge"),
        }
        match parsed(&["record", "--delete", "4", "--log-dir", "/l"]) {
            Invocation::Record(
                Flags {
                    log_dir: Some(log_dir),
                    ..
                },
                RecordAction::Delete { recording_id },
            ) => {
                assert_eq!((recording_id, log_dir), (4, PathBuf::from("/l")));
            }
            _ => panic!("a delete"),
        }
        for args in [
            ["record", "--truncate", "3"].as_slice(),
            ["record", "--truncate", "3", "--to", "33"].as_slice(),
            ["record", "--purge", "3"].as_slice(),
            ["record", "--purge", "3", "--before", "x"].as_slice(),
            ["record", "--delete", "3", "--to", "0"].as_slice(),
            ["record", "--delete", "3", "--before", "0"].as_slice(),
            ["record", "--delete", "3", "--channel", "cavalry:ipc"].as_slice(),
            ["record", "--stop", "1", "--delete", "1"].as_slice(),
            ["record", "7", "--truncate", "1", "--to", "0"].as_slice(),
            ["record", "--purge", "1", "--before", "0", "--before", "64"].as_slice(),
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

    /// `--channel` names the replay channel, `cavalry:ipc` when absent; with any other
    /// channel the subscriber is elsewhere and `--stream` is required, refused before
    /// anything is sent (ADR 0043).
    #[test]
    fn replay_takes_a_channel_and_another_than_ipc_needs_a_stream() {
        match parsed(&["replay", "--recording", "3", "--from", "0"]) {
            Invocation::Replay(_, request) => assert_eq!(request.channel, IPC_CHANNEL),
            _ => panic!("not a replay invocation"),
        }
        let udp = "cavalry:udp?endpoint=192.0.2.10:40123";
        match parsed(&[
            "replay",
            "--recording",
            "3",
            "--from",
            "start",
            "--stream",
            "7",
            "--channel",
            udp,
        ]) {
            Invocation::Replay(_, request) => {
                assert_eq!(request.channel, udp);
                assert_eq!(request.stream_id, Some(7));
            }
            _ => panic!("not a replay invocation"),
        }
        match parsed(&[
            "replay",
            "--recording",
            "3",
            "--from",
            "0",
            "--channel",
            udp,
        ]) {
            Invocation::Wrong(why) => assert!(why.contains("needs --stream"), "{why}"),
            _ => panic!("a udp channel with no stream is refused"),
        }
        match parsed(&[
            "replay",
            "--recording",
            "3",
            "--from",
            "0",
            "--channel",
            "cavalry:ipc",
        ]) {
            Invocation::Replay(_, request) => assert_eq!(request.stream_id, None),
            _ => panic!("cavalry:ipc named is the default"),
        }
        match parsed(&[
            "replay",
            "--recording",
            "3",
            "--from",
            "0",
            "--channel",
            udp,
            "--channel",
            udp,
        ]) {
            Invocation::Wrong(why) => assert!(why.contains("twice"), "{why}"),
            _ => panic!("twice is refused"),
        }
    }

    /// `--from start` names the recording's start position without knowing it: the
    /// request carries `Point::Start`, which `replay_range` sends as the null position;
    /// the parser passes `start` through as any point, and what may go with it is
    /// `replay_range`'s to refuse (card 160).
    #[test]
    fn replay_from_start_is_the_recordings_start_position() {
        match parsed(&["replay", "--recording", "3", "--from", "start"]) {
            Invocation::Replay(_, request) => {
                assert_eq!(request.from, Point::Start);
                assert_eq!(request.to, None);
            }
            _ => panic!("not a replay invocation"),
        }
        assert_eq!(parse_point("start"), Ok(Point::Start));
        assert!(parse_point("Start").is_err(), "the word is lowercase");
    }

    /// The position and the length before the command is sent (card 160): a position
    /// and `--to` give the bytes between them; `--from start` goes as the null position
    /// with the extent or the follow length, and takes no `--to`, whose length would be
    /// `--to` less −1: one frame past `--to` for a recording that started at 0 and the
    /// whole start offset past it for one that started later. `--to start`, `--to`
    /// before `--from` and an instant without a log directory are refused too, each
    /// saying what to pass.
    #[test]
    fn the_replay_range_sends_start_as_the_null_position_and_never_with_to() {
        let request = |from: Point, to: Option<Point>, follow: bool| ReplayRequest {
            recording_id: 3,
            from,
            to,
            stream_id: None,
            follow,
            channel: IPC_CHANNEL.to_string(),
        };
        let range = |from, to, follow| replay_range(&request(from, to, follow), None);
        assert_eq!(
            range(Point::Position(64), Some(Point::Position(192)), false),
            Ok((64, 128))
        );
        assert_eq!(
            range(Point::Position(64), Some(Point::Position(64)), false),
            Ok((64, 0))
        );
        assert_eq!(
            range(Point::Position(64), None, false),
            Ok((64, NULL_LENGTH))
        );
        assert_eq!(
            range(Point::Position(64), None, true),
            Ok((64, FOLLOW_LENGTH))
        );
        assert_eq!(
            range(Point::Start, None, false),
            Ok((NULL_POSITION, NULL_LENGTH))
        );
        assert_eq!(
            range(Point::Start, None, true),
            Ok((NULL_POSITION, FOLLOW_LENGTH))
        );
        for to in [
            Point::Position(0),
            Point::Position(64),
            Point::Instant(1_700_000_000_000_000_000),
        ] {
            let refused = range(Point::Start, Some(to), false).expect_err("refused");
            assert!(
                refused.starts_with("--from start takes no --to"),
                "{refused}"
            );
            assert!(refused.contains("error 23"), "{refused}");
            assert!(
                refused.contains("docs/how-to/replay-a-recording.md"),
                "{refused}"
            );
        }
        for from in [Point::Position(0), Point::Start] {
            let refused = range(from, Some(Point::Start), false).expect_err("refused");
            assert!(refused.starts_with("--to start"), "{refused}");
        }
        let refused =
            range(Point::Position(128), Some(Point::Position(64)), false).expect_err("refused");
        assert_eq!(refused, "--to 64 is before --from 128; nothing to play");
        let refused = range(Point::Instant(0), None, false).expect_err("refused");
        assert!(refused.contains("--log-dir"), "{refused}");
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
                        channel: IPC_CHANNEL.to_string(),
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
