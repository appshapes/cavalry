//! The replay gate's program for a cluster (`just replay-check`; CLAUDE.md, gate 1; ADR
//! 0052, feature 3): one process runs three cluster members on the loopback, each a
//! driver with its consensus module beside it, has the leader append entries whose
//! payloads carry a hash chain, and waits until every member has committed and synced
//! every one. It prints each payload it offered on stdout, one line of hex an entry.
//! Beside each member it runs a service, a client of the member's driver that reads
//! the committed log as the entries commit (`spec/cluster.md`, "The service"; ADR
//! 0066) and writes each entry to a file as `cavalry replay` prints a message, its
//! position and its body in hex.
//! It then closes the three modules, which ends the log's stream, and the three
//! drivers, which leaves each member's recording stopped at its end, and prints on
//! stderr what the script needs: each member's driver directory, log directory,
//! service file and recording id, and the end position. The script starts a driver on
//! each member's directories, replays that member's recording with `cavalry replay`,
//! and compares the three members' lines with each other, with the lines each
//! member's service wrote, and their payloads with the lines of this program. Each
//! entry's payload is its number and a hash over every payload before
//! it, so a byte out of place or an entry missing on any member changes every line
//! after it. `--messages <n>` sets the count, 100000 by default; `--dir <path>` is the
//! directory the three members' directories are made under.

use std::fs::File;
use std::io::{BufWriter, Write as _};
use std::net::UdpSocket;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use cavalry::cluster::{ConsensusModule, ElectionState, RaftError, Refused, Role};
use cavalry::{Cavalry, CommittedLog, Driver};

fn main() -> ExitCode {
    let (messages, dir) = match parse(std::env::args().skip(1)) {
        Ok(parsed) => parsed,
        Err(error) => {
            eprintln!("cluster-replay-check: {error}");
            return ExitCode::from(2);
        }
    };
    match run(messages, &dir) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("cluster-replay-check: {error}");
            ExitCode::FAILURE
        }
    }
}

/// `--messages <n>` and `--dir <path>`, the second required.
fn parse(mut args: impl Iterator<Item = String>) -> Result<(u64, PathBuf), String> {
    let mut messages = 100_000;
    let mut dir = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--messages" => {
                let value = args.next().ok_or("--messages needs a count")?;
                messages = value
                    .parse::<u64>()
                    .ok()
                    .filter(|&n| n > 0)
                    .ok_or_else(|| format!("--messages needs a count above 0; got `{value}`"))?;
            }
            "--dir" => dir = Some(PathBuf::from(args.next().ok_or("--dir needs a path")?)),
            other => {
                return Err(format!(
                    "`{other}` is not an argument cluster-replay-check takes"
                ));
            }
        }
    }
    let dir = dir.ok_or("--dir <path> is required: where the three members' directories go")?;
    Ok((messages, dir))
}

/// FNV-1a over `bytes`, continued from `hash`.
fn fnv1a(mut hash: u64, bytes: &[u8]) -> u64 {
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
    }
    hash
}

/// The length of one entry's frame: the frame header's 32 bytes, the message header's
/// 8, the session message's 24 and the payload's 16, which is 80, aligned to 32
/// (`spec/cluster.md`, "The log"). The term's first entry, the new leadership term
/// event, has a body of 56 bytes and so a frame of the same length.
const FRAME_LENGTH: i64 = 96;

/// The log's position after `frames` frames of [`FRAME_LENGTH`] appended from position
/// 0: a frame that does not fit what is left of its term starts the next term, and a
/// padding frame takes what was left.
fn end_position(frames: i64) -> i64 {
    let term = i64::from(cavalry::spec::UDP_TERM_LENGTH);
    let per_term = term / FRAME_LENGTH;
    let (terms, rest) = (frames / per_term, frames % per_term);
    if rest == 0 && terms > 0 {
        (terms - 1) * term + per_term * FRAME_LENGTH
    } else {
        terms * term + rest * FRAME_LENGTH
    }
}

/// The members string of three members on the loopback, their fifteen ports taken
/// from 20000 to 31999, each tried by binding it.
///
/// The range is below the ports an operating system gives a socket bound to port 0,
/// from 32768 on Linux and from 49152 on macOS. A member binds its endpoints after
/// this returns, a follower its `log` endpoint only when it joins the log, and every
/// publication of the three drivers has a send socket bound to port 0 meanwhile. A
/// port chosen by binding port 0 and releasing it could be given to one of those
/// first, and the member's own bind would then be refused, `Address already in use`.
/// The process id picks where the choice starts, so that two runs on one host start
/// apart. This program starts no child process, so a socket it drops is closed.
fn members_string() -> Result<String, std::io::Error> {
    const FIRST: u16 = 20_000;
    const SPAN: u16 = 12_000;
    const PORTS: usize = 15;
    let start = u16::try_from(std::process::id() % u32::from(SPAN)).unwrap_or(0);
    let mut ports = Vec::with_capacity(PORTS);
    for offset in 0..SPAN {
        // Both sums stay below 32000.
        let port = FIRST + (start + offset) % SPAN;
        if UdpSocket::bind(("127.0.0.1", port)).is_ok() {
            ports.push(port);
            if ports.len() == PORTS {
                break;
            }
        }
    }
    if ports.len() < PORTS {
        return Err(std::io::Error::other(format!(
            "no {PORTS} free loopback ports from {FIRST} to {}; free some and run it again",
            FIRST + SPAN - 1
        )));
    }
    Ok(ports
        .chunks(5)
        .enumerate()
        .map(|(member, ports)| {
            let endpoints: Vec<String> = ports
                .iter()
                .map(|port| format!("127.0.0.1:{port}"))
                .collect();
            format!("{member},{}", endpoints.join(","))
        })
        .collect::<Vec<_>>()
        .join("|"))
}

/// The service beside a member: a client of the member's driver, the committed log
/// once the member has one, the file its lines go to, and how many entries it read.
struct Service {
    client: Cavalry,
    log: Option<CommittedLog>,
    path: PathBuf,
    lines: BufWriter<File>,
    read: u64,
    /// Cycles since the service last asked for the log: it asks once in 64, since
    /// each ask reads the counters file's records.
    asked: u32,
}

impl Service {
    /// Opens the log when the member has one, and reads what has been committed since
    /// the last cycle: one line an entry, the position and the body in hex, which is
    /// how `cavalry replay` prints a body that is not text.
    fn do_work(&mut self) -> Result<usize, Box<dyn std::error::Error>> {
        let Some(log) = self.log.as_mut() else {
            self.asked += 1;
            if self.asked % 64 == 1 {
                self.log = self.client.committed_log(0)?;
            }
            return Ok(0);
        };
        let (lines, read) = (&mut self.lines, &mut self.read);
        let mut failed = None;
        let entries = log.poll(
            |entry, _, position| {
                let mut line = write!(lines, "{position} ");
                for byte in entry {
                    line = line.and_then(|()| write!(lines, "{byte:02x}"));
                }
                if let Err(error) = line.and_then(|()| writeln!(lines)) {
                    failed.get_or_insert(error);
                }
                *read += 1;
            },
            256,
        )?;
        match failed {
            Some(error) => Err(error.into()),
            None => Ok(entries),
        }
    }
}

/// One member: its driver, its consensus module until the module closes, the service
/// beside it, and where its files are.
struct Member {
    driver: Driver,
    module: Option<ConsensusModule>,
    service: Service,
    dir: PathBuf,
    log_dir: PathBuf,
}

/// Runs every member's duty cycle and every service's once; sleeps a little when none
/// had work.
fn cycle(members: &mut [Member]) -> Result<(), Box<dyn std::error::Error>> {
    let mut work = 0;
    for member in members.iter_mut() {
        if let Some(module) = member.module.as_mut() {
            work += module.do_work()?;
        }
        work += member.service.do_work()?;
    }
    if work == 0 {
        std::thread::sleep(Duration::from_micros(100));
    }
    Ok(())
}

fn run(messages: u64, dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let started = Instant::now();
    let members_string = members_string()?;
    let mut members = Vec::with_capacity(3);
    for id in 0..3 {
        let driver_dir = dir.join(format!("member-{id}"));
        let log_dir = dir.join(format!("member-{id}-log"));
        let driver = Driver::start(&driver_dir, &log_dir)?;
        let module = ConsensusModule::start(&driver_dir, &log_dir, &members_string, id)?;
        // The service starts with its member, before the member has a log to read.
        let path = dir.join(format!("service-{id}.txt"));
        let service = Service {
            client: Cavalry::connect_to(&driver_dir)?,
            log: None,
            lines: BufWriter::new(File::create(&path)?),
            path,
            read: 0,
            asked: 0,
        };
        members.push(Member {
            driver,
            module: Some(module),
            service,
            dir: driver_dir,
            log_dir,
        });
    }
    let modules = |members: &[Member]| -> Vec<(Role, ElectionState, i64, i64)> {
        members
            .iter()
            .filter_map(|member| member.module.as_ref())
            .map(|module| {
                (
                    module.role(),
                    module.election_state(),
                    module.append_position(),
                    module.commit_position(),
                )
            })
            .collect()
    };

    // Every member's election closes, and one member leads: the offers below go
    // through it, whichever member it is (ADR 0100, change 3).
    let deadline = Instant::now() + Duration::from_secs(60);
    while !modules(&members)
        .iter()
        .all(|(_, state, _, _)| *state == ElectionState::Closed)
    {
        if Instant::now() > deadline {
            return Err(format!("the elections did not close: {:?}", modules(&members)).into());
        }
        cycle(&mut members)?;
    }
    let Some(leader) = modules(&members)
        .iter()
        .position(|(role, ..)| *role == Role::Leader)
    else {
        return Err(format!("no member leads: {:?}", modules(&members)).into());
    };

    // The leader's own process appends: each payload its number and the hash of every
    // payload before it. A back-pressured offer is made again once the members ran.
    let stdout = std::io::stdout();
    let mut offered = std::io::BufWriter::new(stdout.lock());
    let mut chain: u64 = 0xcbf2_9ce4_8422_2325;
    for number in 0..messages {
        let mut payload = [0u8; 16];
        payload[..8].copy_from_slice(&number.to_le_bytes());
        payload[8..].copy_from_slice(&chain.to_le_bytes());
        chain = fnv1a(chain, &payload);
        for byte in payload {
            write!(offered, "{byte:02x}")?;
        }
        writeln!(offered)?;
        loop {
            let offered = match members.get_mut(leader).and_then(|m| m.module.as_mut()) {
                Some(leader) => leader.offer(&payload),
                None => return Err("the leader's module is gone".into()),
            };
            match offered {
                Ok(()) => break,
                Err(RaftError::Offer(Refused::BackPressured)) => cycle(&mut members)?,
                Err(error) => return Err(error.into()),
            }
        }
        if number % 64 == 63 {
            cycle(&mut members)?;
        }
    }

    offered.flush()?;
    drop(offered);

    // Every entry committed and synced on every member: the term's first entry and
    // each offer are one frame.
    let end = end_position(i64::try_from(messages)? + 1);
    let deadline = Instant::now() + Duration::from_secs(120);
    while !modules(&members)
        .iter()
        .all(|(_, _, append, commit)| *append == end && *commit == end)
    {
        if Instant::now() > deadline {
            return Err(format!(
                "not every member committed and synced to {end}: {:?}",
                modules(&members)
            )
            .into());
        }
        cycle(&mut members)?;
    }

    // Every service has read every entry: all of them are committed, and each
    // member's recording, which its service replays, holds them.
    let entries = messages + 1;
    let deadline = Instant::now() + Duration::from_secs(60);
    while !members.iter().all(|member| member.service.read == entries) {
        if Instant::now() > deadline {
            let read: Vec<u64> = members.iter().map(|member| member.service.read).collect();
            return Err(format!("the services read {read:?} of {entries} entries").into());
        }
        cycle(&mut members)?;
    }
    for member in &mut members {
        member.service.lines.flush()?;
        if let Some(log) = member.service.log.take() {
            if log.position() != end || log.commit_position() != end {
                return Err(format!("a service ended at {log:?}, not at {end}").into());
            }
            log.close(&mut member.service.client)?;
        }
    }

    // The modules close, which ends the log's publication and each follower's image
    // of it, and then the drivers, each of which stops what it still records. Every
    // member's recording is whole on its disk before either: its append position, what
    // its recorder has synced, is the end.
    let mut recordings = Vec::with_capacity(3);
    for member in &mut members {
        if let Some(module) = member.module.take() {
            recordings.push(
                module
                    .log_recording_id()
                    .ok_or("a member has no recording of the log")?,
            );
            module.close()?;
        }
    }
    let mut lines = Vec::with_capacity(3);
    for (id, (member, recording)) in members.into_iter().zip(&recordings).enumerate() {
        lines.push(format!(
            "member={id} dir={} log_dir={} service={} recording={recording}",
            member.dir.display(),
            member.log_dir.display(),
            member.service.path.display()
        ));
        member.service.client.close()?;
        member.driver.close()?;
    }
    for line in lines {
        eprintln!("{line}");
    }
    eprintln!(
        "end={end} entries={entries} seconds={:.3}",
        started.elapsed().as_secs_f64()
    );
    Ok(())
}
