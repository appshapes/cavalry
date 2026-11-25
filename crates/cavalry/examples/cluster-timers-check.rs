//! The timers' half of `just replay-check` (CLAUDE.md, invariant 1; `spec/cluster.md`,
//! "The service", "Timers"; ADR 0151): three cluster members on the loopback, each
//! a driver and a consensus module, and beside each a service whose program
//! schedules a timer at every session message and writes down each timer it is
//! handed. The check is that a timer is handed over once, at one position, beside
//! every member, through what a cluster goes through:
//!
//! 1. The leader is offered the first half of the messages, each of which schedules
//!    a timer due a few milliseconds later, and every program is handed every one.
//! 2. The leader asks for a snapshot, every program gives its state, and the
//!    program beside one follower stops and starts again from that snapshot.
//! 3. The leader is offered the second half, whose timers are due five seconds
//!    later, and stops with its service while they are pending. The two members
//!    left elect a leader, whose member was told of those timers by its own
//!    service, and both programs are handed every one of them.
//! 4. A fourth program starts beside one member and reads its log from position 0,
//!    as a replay does: it is handed the same timers at the same positions.
//!
//! Each program's lines, a timer's position, id and time, go to a file. This
//! program checks nothing of them but their count: `scripts/replay-check.sh` hashes
//! the files and fails unless the three are one.
//!
//! Standard error has one line for each file, `program=<name> path=<file>`, and a
//! last line with the timers' count and the seconds it took.
//!
//! ```sh
//! cargo run --release -p cavalry --example cluster-timers-check -- --timers 200 --dir /tmp/cavalry-timers
//! ```

use std::fs::File;
use std::io::{BufWriter, Write as _};
use std::net::UdpSocket;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use cavalry::cluster::{ConsensusModule, ElectionState, RaftError, Refused, Role};
use cavalry::{Action, Cavalry, ClusterService, Driver, Entry, Restored};

fn main() -> ExitCode {
    let (timers, dir) = match parse(std::env::args().skip(1)) {
        Ok(parsed) => parsed,
        Err(error) => {
            eprintln!("cluster-timers-check: {error}");
            return ExitCode::from(2);
        }
    };
    match run(timers, &dir) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("cluster-timers-check: {error}");
            ExitCode::FAILURE
        }
    }
}

/// `--timers <n>`, an even count, and `--dir <path>`, the second required.
fn parse(mut args: impl Iterator<Item = String>) -> Result<(u64, PathBuf), String> {
    let mut timers = 200;
    let mut dir = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--timers" => {
                let value = args.next().ok_or("--timers needs a count")?;
                timers = value
                    .parse::<u64>()
                    .ok()
                    .filter(|&n| n > 0 && n % 2 == 0 && n <= 1_000)
                    .ok_or_else(|| {
                        format!("--timers needs an even count from 2 to 1000; got `{value}`")
                    })?;
            }
            "--dir" => dir = Some(PathBuf::from(args.next().ok_or("--dir needs a path")?)),
            other => {
                return Err(format!(
                    "`{other}` is not an argument cluster-timers-check takes"
                ));
            }
        }
    }
    let dir = dir.ok_or("--dir <path> is required: where the three members' directories go")?;
    Ok((timers, dir))
}

/// The members string of three members on the loopback, their fifteen ports taken
/// from 20000 to 31999, each tried by binding it: the choice
/// `examples/cluster-replay-check.rs` makes, for its reasons.
fn members_string() -> Result<String, std::io::Error> {
    const FIRST: u16 = 20_000;
    const SPAN: u16 = 12_000;
    const PORTS: usize = 15;
    // Apart from where `cluster-replay-check` of the same process id started.
    let start = u16::try_from((std::process::id() + 6_000) % u32::from(SPAN)).unwrap_or(0);
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

/// A session message of this program: the timer's id and how many milliseconds
/// after the entry's own time it is due.
fn message(id: i64, after_ms: i64) -> [u8; 16] {
    let mut payload = [0u8; 16];
    payload[..8].copy_from_slice(&id.to_le_bytes());
    payload[8..].copy_from_slice(&after_ms.to_le_bytes());
    payload
}

/// A service and its program: the program's state is the timers it was handed, each
/// a position, an id and a time.
struct Program {
    client: Cavalry,
    service: ClusterService,
    handed: Vec<(i64, i64, i64)>,
    /// Whether the program gives its state when a snapshot is due. One that does
    /// not reads on, as a program that replays the log does.
    takes_snapshots: bool,
}

impl Program {
    /// A program beside the member whose driver is at `dir`, which reads the
    /// member's log from position 0.
    fn beside(dir: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let client = Cavalry::connect_to(dir)?;
        let service = client.cluster_service();
        Ok(Self {
            client,
            service,
            handed: Vec::new(),
            takes_snapshots: true,
        })
    }

    /// A program that starts from the snapshot its member lists, and fails when the
    /// member lists none it can take.
    fn restored(dir: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let mut program = Self::beside(dir)?;
        match program.service.restore(&mut program.client)? {
            Restored::State(state) => {
                program.handed = state
                    .as_chunks::<24>()
                    .0
                    .iter()
                    .map(|timer| {
                        let number = |at: usize| {
                            let mut bytes = [0u8; 8];
                            bytes.copy_from_slice(&timer[at..at + 8]);
                            i64::from_le_bytes(bytes)
                        };
                        (number(0), number(8), number(16))
                    })
                    .collect();
                Ok(program)
            }
            other => Err(format!("a program found no snapshot to start from: {other:?}").into()),
        }
    }

    /// The program's state as bytes: what a snapshot takes.
    fn state(&self) -> Vec<u8> {
        let mut state = Vec::with_capacity(self.handed.len() * 24);
        for (position, id, time) in &self.handed {
            state.extend_from_slice(&position.to_le_bytes());
            state.extend_from_slice(&id.to_le_bytes());
            state.extend_from_slice(&time.to_le_bytes());
        }
        state
    }

    /// One duty cycle: the snapshot when one is due and `give` allows it, else a
    /// poll, in which a session message schedules its timer and a timer handed over
    /// is written down.
    fn do_work(&mut self, give: bool) -> Result<usize, Box<dyn std::error::Error>> {
        if self.takes_snapshots && self.service.snapshot_due().is_some() {
            if !give {
                return Ok(0);
            }
            let state = self.state();
            self.service.snapshot(&mut self.client, &state)?;
            return Ok(1);
        }
        let handed = &mut self.handed;
        let mut refused = false;
        let polled = self.service.poll(
            &mut self.client,
            |committed, replies| {
                match committed.entry {
                    Entry::SessionMessage { payload, .. } if payload.len() == 16 => {
                        let number = |at: usize| {
                            let mut bytes = [0u8; 8];
                            bytes.copy_from_slice(&payload[at..at + 8]);
                            i64::from_le_bytes(bytes)
                        };
                        refused |= replies
                            .schedule_timer(number(0), committed.timestamp + number(8))
                            .is_err();
                    }
                    Entry::Timer { correlation_id } => {
                        handed.push((committed.position, correlation_id, committed.timestamp));
                    }
                    _ => {}
                }
                Action::Continue
            },
            64,
        )?;
        if refused {
            return Err("a schedule was refused: more than 1,024 timers were pending".into());
        }
        Ok(polled)
    }

    /// Writes the program's lines to `path`, and closes it.
    fn finish(self, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        let mut lines = BufWriter::new(File::create(path)?);
        for (position, id, time) in &self.handed {
            writeln!(lines, "{position} {id} {time}")?;
        }
        lines.flush()?;
        self.close()
    }

    /// Closes the service and its client.
    fn close(mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.service.close(&mut self.client)?;
        self.client.close()?;
        Ok(())
    }
}

/// One member: its driver, its consensus module, the program beside it, and its
/// driver's directory.
struct Member {
    id: i32,
    driver: Driver,
    module: ConsensusModule,
    program: Program,
    dir: PathBuf,
}

/// Runs every member's duty cycle and every program's once; sleeps a little when none
/// had work. A program gives its state to a snapshot once every member lists its own
/// snapshot at that position, so that the three are of one position.
fn cycle(members: &mut [Member]) -> Result<(), Box<dyn std::error::Error>> {
    let mut work = 0;
    for member in members.iter_mut() {
        work += member.module.do_work()?;
    }
    let listed: Vec<Option<i64>> = members
        .iter()
        .map(|member| member.module.snapshot_position())
        .collect();
    for member in members.iter_mut() {
        let give = member.program.service.snapshot_due().is_some_and(|due| {
            listed
                .iter()
                .all(|own| own.is_some_and(|position| position >= due))
        });
        work += member.program.do_work(give)?;
    }
    if work == 0 {
        std::thread::sleep(Duration::from_micros(100));
    }
    Ok(())
}

/// Cycles until `done`, or fails with `what` after `seconds`.
fn until(
    members: &mut [Member],
    seconds: u64,
    what: &str,
    mut done: impl FnMut(&[Member]) -> bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    while !done(members) {
        if Instant::now() > deadline {
            let states: Vec<String> = members
                .iter()
                .map(|member| {
                    format!(
                        "member {}: {:?} {:?}, append {} commit {}, {} timers handed, {:?}",
                        member.id,
                        member.module.role(),
                        member.module.election_state(),
                        member.module.append_position(),
                        member.module.commit_position(),
                        member.program.handed.len(),
                        member.module.timer_counts()
                    )
                })
                .collect();
            return Err(format!("{what}: {states:?}").into());
        }
        cycle(members)?;
    }
    Ok(())
}

/// The index of the one member that leads once every member's election has closed.
fn elected(members: &mut [Member]) -> Result<usize, Box<dyn std::error::Error>> {
    until(
        members,
        90,
        "the elections did not close with one leader",
        |members| {
            members
                .iter()
                .all(|member| member.module.election_state() == ElectionState::Closed)
                && members
                    .iter()
                    .filter(|member| member.module.role() == Role::Leader)
                    .count()
                    == 1
        },
    )?;
    members
        .iter()
        .position(|member| member.module.role() == Role::Leader)
        .ok_or_else(|| "no member leads".into())
}

/// Offers the leader one payload, through back pressure.
fn offer(
    members: &mut [Member],
    leader: usize,
    payload: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    loop {
        match members[leader].module.offer(payload) {
            Ok(()) => return Ok(()),
            Err(RaftError::Offer(Refused::BackPressured)) => cycle(members)?,
            Err(error) => return Err(error.into()),
        }
    }
}

fn run(timers: u64, dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let started = Instant::now();
    let members_string = members_string()?;
    let mut members = Vec::with_capacity(3);
    for id in 0..3 {
        let driver_dir = dir.join(format!("member-{id}"));
        let log_dir = dir.join(format!("member-{id}-log"));
        let driver = Driver::start(&driver_dir, &log_dir)?;
        let module = ConsensusModule::start(&driver_dir, &log_dir, &members_string, id)?;
        let program = Program::beside(&driver_dir)?;
        members.push(Member {
            id,
            driver,
            module,
            program,
            dir: driver_dir,
        });
    }
    let half = usize::try_from(timers / 2)?;
    let mut leader = elected(&mut members)?;

    // 1. The first half: each timer is due 5 ms after its entry.
    for id in 0..i64::try_from(half)? {
        offer(&mut members, leader, &message(id, 5))?;
    }
    until(
        &mut members,
        60,
        "not every program was handed the first half",
        |members| {
            members
                .iter()
                .all(|member| member.program.handed.len() == half)
        },
    )?;

    // 2. A snapshot of every program, and one program starts again from its own.
    loop {
        match members[leader].module.snapshot() {
            Ok(()) => break,
            Err(RaftError::Offer(Refused::BackPressured)) => cycle(&mut members)?,
            Err(error) => return Err(error.into()),
        }
    }
    until(
        &mut members,
        60,
        "not every member listed its service's snapshot",
        |members| {
            let position = members[0].module.service_snapshot_position();
            position.is_some()
                && members
                    .iter()
                    .all(|member| member.module.service_snapshot_position() == position)
        },
    )?;
    let follower = (leader + 1) % 3;
    let beside = members[follower].dir.clone();
    // The program that ran is closed before the one that starts: a program that has
    // read nothing stands in its place for that moment.
    let ran = std::mem::replace(&mut members[follower].program, Program::beside(&beside)?);
    ran.close()?;
    let stood_in = std::mem::replace(&mut members[follower].program, Program::restored(&beside)?);
    stood_in.close()?;
    if members[follower].program.handed.len() != half {
        return Err(format!(
            "the program that started from its snapshot holds {} timers, not {half}",
            members[follower].program.handed.len()
        )
        .into());
    }

    // 3. The second half, due 5 s after its entries, and the leader stops while
    //    they are pending.
    for id in i64::try_from(half)?..i64::try_from(timers)? {
        offer(&mut members, leader, &message(id, 5_000))?;
    }
    until(
        &mut members,
        60,
        "not every program read the second half's entries",
        |members| {
            let end = members[0].module.append_position();
            members.iter().all(|member| {
                member.module.append_position() == end
                    && member.module.commit_position() == end
                    && member.program.service.position() == end
            })
        },
    )?;
    let was = members.remove(leader);
    let handed_by_then = was.program.handed.len();
    was.program.finish(&dir.join("program-stopped.txt"))?;
    was.module.close()?;
    was.driver.close()?;
    if handed_by_then != half {
        return Err(format!(
            "the leader's program was handed {handed_by_then} timers before it stopped, \
             not the first {half}: the second half was due five seconds after its \
             entries, and this host took longer than that to commit and read them. \
             Run it again on a quieter host"
        )
        .into());
    }
    leader = elected(&mut members)?;
    let all = usize::try_from(timers)?;
    until(
        &mut members,
        90,
        "not every program was handed every timer",
        |members| {
            members
                .iter()
                .all(|member| member.program.handed.len() == all)
        },
    )?;
    // The log is at rest once no member holds a timer: a leader appends an event
    // only for one it holds, and every timer was handed over and cancelled.
    until(
        &mut members,
        60,
        "the members still hold timers",
        |members| {
            let end = members[0].module.append_position();
            members.iter().all(|member| {
                member.module.timer_counts().1 == 0
                    && member.module.append_position() == end
                    && member.module.commit_position() == end
                    && member.program.service.position() == end
            })
        },
    )?;

    // 4. A program that reads the log from position 0 beside the leader, as a
    //    replay does.
    let end = members[leader].module.append_position();
    let mut replay = Program::beside(&members[leader].dir)?;
    replay.takes_snapshots = false;
    let deadline = Instant::now() + Duration::from_secs(60);
    while replay.service.position() != end {
        if Instant::now() > deadline {
            return Err(format!(
                "the replaying program read to {} of {end}",
                replay.service.position()
            )
            .into());
        }
        cycle(&mut members)?;
        replay.do_work(false)?;
    }

    let mut lines = Vec::with_capacity(3);
    let path = dir.join("program-replay.txt");
    replay.finish(&path)?;
    lines.push(format!("program=replay path={}", path.display()));
    for member in members {
        let path = dir.join(format!("program-{}.txt", member.id));
        member.program.finish(&path)?;
        lines.push(format!(
            "program=member-{} path={}",
            member.id,
            path.display()
        ));
        member.module.close()?;
        member.driver.close()?;
    }
    for line in lines {
        eprintln!("{line}");
    }
    eprintln!(
        "timers={timers} seconds={:.3}",
        started.elapsed().as_secs_f64()
    );
    Ok(())
}
