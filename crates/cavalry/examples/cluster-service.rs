//! A service beside a cluster member (`spec/cluster.md`, "The service"; ADR 0065): a
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

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use cavalry::{Cavalry, CommittedLog};

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
            other => return Err(format!("`{other}` is not an argument cluster-service takes")),
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
            Some(entries) => usize::try_from(entries - read).unwrap_or(usize::MAX).min(64),
            None => 64,
        };
        let entries = log.poll(
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
        )?;
        read += entries as u64;
        if args.entries.is_some_and(|wanted| read >= wanted) {
            break;
        }
        if entries == 0 {
            if log.is_ended() {
                // The member left its log or closed: nothing more comes from this
                // handle. What was read stays read; the next run starts here.
                eprintln!(
                    "cluster-service: the member's log ended; read to position {}",
                    log.position()
                );
                break;
            }
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
