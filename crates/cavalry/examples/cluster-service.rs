//! A service beside a cluster member (`spec/cluster.md`, "The service"; ADR 0066): a
//! client of the member's driver that reads the cluster's committed log, every entry a
//! quorum of members holds, in order, each once, and prints a line for each: its
//! position, its template id, and its length. `docs/how-to/read-the-committed-log.md`
//! runs it.
//!
//! `--dir <path>` is the member's driver directory, `CAVALRY_DIR` or the default when
//! absent. `--from <position>` is where to start reading, 0 by default: the position
//! of an entry, as an earlier run printed it for the entry after the last it read.
//! `--entries <n>` ends the program after `n` entries; without it the program reads
//! until its member's log ends or it is stopped.
//!
//! The program may start before its member, with it or after it: while the driver
//! hosts no member that has a log it asks again ten times a second.
//!
//! A member's log is one recording or several in a row, and a handle reads one (ADR
//! 0103). When a handle has delivered all its recording holds, the program opens the
//! log again where the handle ended: that is the member's next recording, at once, or
//! nothing, when the member closed or left its log, and then the program ends.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use cavalry::{Cavalry, CommittedLog, Fault};

struct Args {
    dir: Option<PathBuf>,
    from: i64,
    entries: Option<u64>,
}

fn parse(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut parsed = Args {
        dir: None,
        from: 0,
        entries: None,
    };
    while let Some(arg) = args.next() {
        let mut value = |what: &str| args.next().ok_or(format!("{arg} needs {what}"));
        match arg.as_str() {
            "--dir" => parsed.dir = Some(PathBuf::from(value("a path")?)),
            "--from" => {
                let text = value("a position")?;
                parsed.from = text
                    .parse::<i64>()
                    .ok()
                    .filter(|position| *position >= 0 && position % 32 == 0)
                    .ok_or(format!(
                        "--from needs a position, a multiple of 32 from 0; got `{text}`"
                    ))?;
            }
            "--entries" => {
                let text = value("a count")?;
                parsed.entries = Some(
                    text.parse::<u64>()
                        .ok()
                        .filter(|count| *count > 0)
                        .ok_or(format!("--entries needs a count above 0; got `{text}`"))?,
                );
            }
            other => {
                return Err(format!(
                    "`{other}` is not an argument cluster-service takes"
                ));
            }
        }
    }
    Ok(parsed)
}

fn main() -> ExitCode {
    let args = match parse(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(error) => {
            eprintln!("cluster-service: {error}");
            return ExitCode::from(2);
        }
    };
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("cluster-service: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Opens the member's committed log at `position`, asking ten times a second while the
/// driver hosts no member that has one.
fn open(client: &mut Cavalry, position: i64) -> Result<CommittedLog, Box<dyn std::error::Error>> {
    let mut waiting = false;
    loop {
        if let Some(log) = client.committed_log(position)? {
            return Ok(log);
        }
        if !waiting {
            eprintln!("cluster-service: waiting for a cluster member with a log on this driver");
            waiting = true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn run(args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    let mut client = match &args.dir {
        Some(dir) => Cavalry::connect_to(dir)?,
        None => Cavalry::connect()?,
    };
    let mut log = open(&mut client, args.from)?;
    eprintln!(
        "cluster-service: reading member {}'s log, recording {}, from position {}",
        log.member_id(),
        log.recording_id(),
        log.position()
    );
    let mut read = 0u64;
    loop {
        let limit = match args.entries {
            Some(entries) => usize::try_from(entries - read)
                .unwrap_or(usize::MAX)
                .min(64),
            None => 64,
        };
        let polled = log.poll(
            |entry, _, position| {
                // An entry begins with the message header of `spec/cluster.md`,
                // "Encoding": the template id is its second field, two bytes.
                let template = match entry {
                    [_, _, low, high, ..] => u16::from_le_bytes([*low, *high]),
                    _ => 0,
                };
                println!("{position} template {template} {} bytes", entry.len());
            },
            limit,
        );
        let entries = match polled {
            Ok(entries) => entries,
            // The fault of the log's own image, the replay's: the handle is ended, and
            // keeps the fault, which is read below after the other two reports.
            Err(Fault::Image { .. } | Fault::ImagesFull { .. }) => 0,
            Err(fault) => {
                // A closed client says why: the driver went, or it timed this client
                // out, or the client could not keep up with the driver's events.
                return Err(match client.closed() {
                    Some(why) => format!("{fault}; the client closed: {}", why.reason()).into(),
                    None => fault.into(),
                });
            }
        };
        read += entries as u64;
        if args.entries.is_some_and(|wanted| read >= wanted) {
            break;
        }
        // Ended, and not merely a poll of 0: a poll returns 0 while entries below the
        // bound are still on their way from the replay, and the handle is ended only
        // once every one of them has been delivered.
        if log.is_ended() {
            if let Some(position) = log.failed_at() {
                // The driver could not play the member's recording on from there. What
                // this program read ends at the handle's position, which is below the
                // failure when the driver withdrew the failed replay before this
                // program had read it, and is where the next run starts.
                let next = log.position();
                let again = if read > 0 {
                    format!(
                        "this program read every entry below position {next} and none \
                         from it on; start it again with --from {next}"
                    )
                } else {
                    format!(
                        "this program read no entry: --from {} was not the position of an \
                         entry, or the recording is damaged there; give --from a next \
                         position an earlier run printed, or 0",
                        args.from
                    )
                };
                return Err(format!(
                    "the replay of member {}'s recording {} failed at position {position}: \
                     a segment file of the recording could not be read or a frame in it \
                     failed a check; {again}; run `cavalry doctor`, which names a damaged \
                     recording; see \
                     docs/how-to/read-the-committed-log.md#when-it-does-not-work",
                    log.member_id(),
                    log.recording_id()
                )
                .into());
            }
            if let Some(position) = log.withdrawn_at() {
                // The driver withdrew the replay's image 5 s after the replay ended,
                // before this program had read it: the entries from there on were not
                // read, and the next run starts where this one stopped.
                return Err(format!(
                    "the driver withdrew the replay of member {}'s recording {} before \
                     this program read the entries from position {position}: it did not \
                     read them within 5 s of its member closing or leaving its log; start \
                     it again with --from {} once the member runs; see \
                     docs/how-to/read-the-committed-log.md#when-it-does-not-work",
                    log.member_id(),
                    log.recording_id(),
                    log.position()
                )
                .into());
            }
            if let Some(fault) = log.faulted() {
                // This client's read of the replay's image stopped: no entry from the
                // handle's position on was delivered, though the member may have
                // committed more, and the next run starts there.
                let next = log.position();
                let what = match fault {
                    Fault::Image { reason, .. } if reason == Fault::GONE => {
                        "the replay's image was gone before this program's client read it, \
                         which takes the client being held for over a second"
                            .to_string()
                    }
                    Fault::Image {
                        position, reason, ..
                    } => format!(
                        "a frame of the replay's image failed the `{reason}` check at \
                         position {position}"
                    ),
                    _ => "the replay's image could not join: the subscription already \
                          held 256 images"
                        .to_string(),
                };
                return Err(format!(
                    "the read of member {}'s recording {} stopped on a fault: {what}; this \
                     program read every entry below position {next} and none from it on; \
                     start it again with --from {next}; see \
                     docs/how-to/read-the-committed-log.md#when-it-does-not-work",
                    log.member_id(),
                    log.recording_id()
                )
                .into());
            }
            // Every entry this handle's recording holds below the last commit position
            // the handle took has been read. The member's log goes on in its next
            // recording when this one lies below its live one: the log opened again at
            // the handle's position is that recording, at once.
            if let Some(next) = client.committed_log(log.position())? {
                let ended = std::mem::replace(&mut log, next);
                ended.close(&mut client)?;
                eprintln!(
                    "cluster-service: reading member {}'s log, recording {}, from position {}",
                    log.member_id(),
                    log.recording_id(),
                    log.position()
                );
                continue;
            }
            // No member names a recording: the member left its log or closed, and the
            // next run starts here.
            eprintln!(
                "cluster-service: the member's log ended; read to position {}",
                log.position()
            );
            break;
        }
        if entries == 0 {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    eprintln!(
        "cluster-service: {read} entries, next position {}, commit position {}",
        log.position(),
        log.commit_position()
    );
    log.close(&mut client)?;
    client.close()?;
    Ok(())
}
