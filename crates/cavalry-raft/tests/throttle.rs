//! A member of a cluster throttled as a cgroup CPU limit throttles it (`CLAUDE.md`,
//! gate 3; ADR 0052, feature 4; ADR 0077). Three members elect a leader, which offers
//! thirty thousand entries a second. One follower is then given a twentieth of a
//! processor for five seconds. The test holds that the cluster stays available while
//! a quorum remains, that the throttled member unseats nobody, and that once the
//! throttle ends the three hold one log with every committed entry in it, and it
//! prints what the throttle cost: the entries committed a second before it, under it
//! and after it, and how far behind the throttled member was.
//!
//! # What the throttle costs, and why the test prints it
//!
//! The leader's log goes to both followers under `fc=min` (`spec/cluster.md`,
//! "Members and streams"): the leader sends no further than the receiver window, 128
//! KiB, past the slower follower's position. A throttled follower still sends its
//! status messages in the part of each period it runs, so it never times out and it
//! paces the leader, though the leader and the other follower are a quorum without
//! it: under the throttle the cluster commits about one window each period, some
//! 13,000 entries of 96 bytes a second, whatever the leader is offered (ADR 0052,
//! "Harder"). The test asserts only that entries are committed. The rates are what
//! an operator reads the cost from, so they are printed and no bound is put on them.
//!
//! # Why the members are processes
//!
//! A CPU limit is a process's. Every other test of this crate runs its three drivers
//! in the test's own process, where no one member can be throttled. Here each member
//! is a process of its own: this program started again with [`MEMBER`] set, which runs
//! one driver and one consensus module until the test says stop. The test reads the
//! members from outside, as an operator does: their state from the counters each
//! member's driver holds, and their logs through a service's handle on each.
//!
//! # How the member is throttled
//!
//! - **Where the runner allows it, by the kernel.** On Linux with cgroup v2, `sudo`
//!   without a password and `systemd-run`, each member is started in a transient scope
//!   unit of its own, and the test sets `CPUQuota` on one follower's scope: the kernel
//!   stops every thread of that member for the rest of each 100 ms period once they
//!   have run 5 ms of it. The scope's `cpu.stat` says how many periods the kernel
//!   throttled, and the test fails when it says none.
//! - **Elsewhere, by stop and continue.** On macOS, and on a Linux host that refuses
//!   any of the above, the test stops the member's process for 95 ms in every 100 ms
//!   with `SIGSTOP` and `SIGCONT`, sent to that process by its id. It is the same
//!   starvation, harsher in one way: a quota stops a member only once it has used its
//!   share, and this stops it whether or not it had work.
//!
//! The line the test prints says which of the two it was, and why.
//!
//! A program with no test harness (`harness = false` in the crate's `Cargo.toml`): a
//! program that starts itself again as a member needs its own `main`, and one thread
//! at the moment it starts a child (`tests/ports/mod.rs`, "What the rule does not
//! cover"). `cargo test` runs it with the others, whatever filter it is given.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use cavalry_client::Cavalry;
use cavalry_driver::Driver;
use cavalry_proto::udp::UDP_TERM_LENGTH;
use cavalry_raft::{ConsensusModule, ElectionState, RaftError, Refused, Role};
use cavalry_shm::counters::CountersReader;
use cavalry_shm::counters::generated::{
    CLUSTER_APPEND_POSITION, CLUSTER_COMMIT_POSITION, CLUSTER_ELECTION_STATE, CLUSTER_ELECTIONS,
    CLUSTER_LEADERSHIP_TERM_ID, CLUSTER_ROLE,
};

mod ports;
use ports::free_ports;

/// The environment of a member's process: its member id, the test's directory and
/// the members string.
const MEMBER: &str = "CAVALRY_THROTTLE_MEMBER";
const DIR: &str = "CAVALRY_THROTTLE_DIR";
const MEMBERS: &str = "CAVALRY_THROTTLE_MEMBERS";

/// How many entries a second the leader offers. An entry it is refused under
/// back-pressure it does not make up later: it may offer a tenth of a second's worth
/// at once and no more.
const RATE: u64 = 30_000;

/// The length in the log of one entry the leader offers, and of a new leadership
/// term event.
const FRAME: i64 = 96;

/// How long the cluster runs before the throttle, and how long the throttle lasts.
const BEFORE: Duration = Duration::from_secs(2);
const THROTTLED: Duration = Duration::from_secs(5);

/// The throttled member's share of one processor, as `CPUQuota` takes it, and the
/// same share of a 100 ms period for stop and continue.
const QUOTA: &str = "5%";
const RUNS: Duration = Duration::from_millis(5);
const STOPPED: Duration = Duration::from_millis(95);

/// How long a member's process lives when nothing tells it to stop: a test that
/// died leaves no member behind for longer.
const LIFETIME: Duration = Duration::from_secs(180);

/// `cluster.role` of a leader, and `cluster.election_state` with no election running
/// (`spec/cluster.md`, "Counters").
const LEADER: i64 = 2;
const NO_ELECTION: i64 = 17;

fn main() {
    match std::env::var(MEMBER) {
        Ok(id) => member(id.parse().expect("a member id")),
        Err(_) => test(),
    }
}

/// One member's process: a driver and a consensus module, run until the test's
/// directory holds `stop`. While it leads it offers [`RATE`] entries a second, each
/// its number and the number's complement, until the directory holds `quiet`.
fn member(id: i32) {
    let dir = PathBuf::from(std::env::var(DIR).expect("the test's directory"));
    let members = std::env::var(MEMBERS).expect("the members string");
    let driver_dir = dir.join(format!("member-{id}"));
    let log_dir = dir.join(format!("member-{id}-log"));
    let driver = Driver::start(&driver_dir, &log_dir).expect("the driver starts");
    let mut module =
        ConsensusModule::start(&driver_dir, &log_dir, &members, id).expect("the member starts");
    let (stop, quiet) = (dir.join("stop"), dir.join("quiet"));
    let started = Instant::now();
    let mut looked = started;
    let mut quieted = false;
    // What the leader may offer now, in millionths of an entry, and when it was
    // last added to.
    let mut allowance = 0u64;
    let mut paid = Instant::now();
    let mut offered = 0u32;
    loop {
        let mut work = module.do_work().expect("the member's duty cycle");
        if looked.elapsed() >= Duration::from_millis(5) {
            looked = Instant::now();
            if stop.exists() || !dir.exists() || started.elapsed() >= LIFETIME {
                break;
            }
            quieted = quiet.exists();
        }
        if !quieted
            && module.role() == Role::Leader
            && module.election_state() == ElectionState::Closed
        {
            let micros = u64::try_from(paid.elapsed().as_micros()).unwrap_or(u64::MAX);
            paid = Instant::now();
            allowance = allowance
                .saturating_add(micros.saturating_mul(RATE))
                .min(RATE * 100_000);
            let mut burst = 0;
            while allowance >= 1_000_000 && burst < 64 {
                let mut payload = [0u8; 8];
                payload[..4].copy_from_slice(&offered.to_le_bytes());
                payload[4..].copy_from_slice(&(!offered).to_le_bytes());
                match module.offer(&payload) {
                    Ok(()) => {
                        offered += 1;
                        burst += 1;
                        allowance -= 1_000_000;
                    }
                    Err(RaftError::Offer(Refused::BackPressured)) => break,
                    Err(other) => panic!("{other}"),
                }
            }
            work += burst;
        } else {
            paid = Instant::now();
        }
        if work == 0 {
            std::thread::sleep(Duration::from_micros(200));
        }
    }
    module.close().expect("the member closes");
    driver.close().expect("the driver closes");
}

/// How this host throttles a member.
enum Throttle {
    /// Each member in a transient scope unit of its own, started as this user.
    Cgroup { uid: String, gid: String },
    /// `SIGSTOP` and `SIGCONT`, and why the host has no cgroup this test may use.
    StopAndContinue { why: String },
}

/// What a command printed, when it ran and succeeded.
fn output(program: &str, arguments: &[&str]) -> Option<String> {
    let output = Command::new(program)
        .args(arguments)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// True when the command ran and succeeded.
fn ran(program: &str, arguments: &[&str]) -> bool {
    Command::new(program)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

impl Throttle {
    /// Finds what the host allows: a command run through `sudo -n systemd-run --scope`
    /// as this user, with a variable set, is the whole of what a member's start needs.
    fn find() -> Self {
        let why = |why: &str| Throttle::StopAndContinue {
            why: why.to_string(),
        };
        if !cfg!(target_os = "linux") {
            return why("no cgroup on this operating system");
        }
        if !Path::new("/sys/fs/cgroup/cgroup.controllers").exists() {
            return why("no cgroup v2 at /sys/fs/cgroup");
        }
        let (Some(uid), Some(gid)) = (output("id", &["-u"]), output("id", &["-g"])) else {
            return why("`id` did not say who runs the test");
        };
        let probe = [
            "-n",
            "systemd-run",
            "--scope",
            "--quiet",
            "--collect",
            &format!("--uid={uid}"),
            &format!("--gid={gid}"),
            "--setenv=CAVALRY_THROTTLE_PROBE=1",
            "/bin/sh",
            "-c",
            "test \"$CAVALRY_THROTTLE_PROBE\" = 1",
        ];
        if !ran("sudo", &probe) {
            return why("`sudo -n systemd-run --scope` ran no command as this user");
        }
        Throttle::Cgroup { uid, gid }
    }
}

/// The scope unit of member `id` of this test's process.
fn unit(id: i32) -> String {
    format!("cavalry-throttle-{}-{id}", std::process::id())
}

/// The three members' processes. Dropped, it tells them to stop and waits for them.
struct Cluster {
    dir: PathBuf,
    members: Vec<(i32, Child)>,
}

impl Cluster {
    fn start(dir: &Path, members: &str, throttle: &Throttle) -> Self {
        let program = std::env::current_exe().expect("this program's path");
        let started = [1, 0, 2]
            .into_iter()
            .map(|id| {
                let mut command = match throttle {
                    Throttle::Cgroup { uid, gid } => {
                        let mut command = Command::new("sudo");
                        command
                            .args(["-n", "systemd-run", "--scope", "--quiet", "--collect"])
                            .arg(format!("--unit={}", unit(id)))
                            .arg(format!("--uid={uid}"))
                            .arg(format!("--gid={gid}"))
                            .arg(format!("--setenv={MEMBER}={id}"))
                            .arg(format!("--setenv={DIR}={}", dir.display()))
                            .arg(format!("--setenv={MEMBERS}={members}"))
                            .arg(&program);
                        command
                    }
                    Throttle::StopAndContinue { .. } => {
                        let mut command = Command::new(&program);
                        command
                            .env(MEMBER, id.to_string())
                            .env(DIR, dir)
                            .env(MEMBERS, members);
                        command
                    }
                };
                let child = command
                    .stdin(Stdio::null())
                    .spawn()
                    .expect("a member's process starts");
                (id, child)
            })
            .collect();
        Self {
            dir: dir.to_path_buf(),
            members: started,
        }
    }

    /// The process id of member `id`, for stop and continue.
    fn process(&self, id: i32) -> String {
        self.members
            .iter()
            .find(|(member, _)| *member == id)
            .map(|(_, child)| child.id().to_string())
            .expect("a member")
    }

    /// Tells every member to stop and waits for each: true when all three ended
    /// with success.
    fn stop(&mut self) -> bool {
        let _ = std::fs::write(self.dir.join("stop"), b"");
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut clean = true;
        for (id, child) in &mut self.members {
            // A member left stopped by a test that failed mid-throttle cannot read
            // the file: it is continued first. To a process that runs it is nothing.
            let _ = ran("kill", &["-CONT", &child.id().to_string()]);
            loop {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        clean &= status.success();
                        break;
                    }
                    Ok(None) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    _ => {
                        eprintln!("throttle: member {id} did not stop; its process is ended");
                        let _ = child.kill();
                        let _ = child.wait();
                        clean = false;
                        break;
                    }
                }
            }
        }
        self.members.clear();
        clean
    }
}

impl Drop for Cluster {
    fn drop(&mut self) {
        if !self.members.is_empty() {
            self.stop();
        }
    }
}

/// A directory of its own, removed when the guard drops.
struct TestDir(PathBuf);

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The members string of three members on the loopback.
fn members_string() -> String {
    let ports = free_ports(15);
    (0..3)
        .map(|member| {
            let endpoints: Vec<String> = ports[member * 5..member * 5 + 5]
                .iter()
                .map(|port| format!("127.0.0.1:{port}"))
                .collect();
            format!("{member},{}", endpoints.join(","))
        })
        .collect::<Vec<_>>()
        .join("|")
}

/// The value of member `id`'s counter of `counter`, read from its driver's counters
/// file by the type and the member id in the key; `None` until the member has it.
fn gauge(dir: &Path, id: i32, counter: i32) -> Option<i64> {
    let reader = CountersReader::open(&dir.join(format!("member-{id}"))).ok()?;
    let key = id.to_le_bytes();
    (0..reader.capacity()).find_map(|slot| {
        let record = reader.record(slot).ok()??;
        (record.type_id() == counter && record.key().starts_with(&key))
            .then(|| reader.read_acquire(slot).ok())?
    })
}

/// What the test reads of one member.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct State {
    role: i64,
    election_state: i64,
    term: i64,
    elections: i64,
    append: i64,
    commit: i64,
}

fn state(dir: &Path, id: i32) -> Option<State> {
    Some(State {
        role: gauge(dir, id, CLUSTER_ROLE.0)?,
        election_state: gauge(dir, id, CLUSTER_ELECTION_STATE.0)?,
        term: gauge(dir, id, CLUSTER_LEADERSHIP_TERM_ID.0)?,
        elections: gauge(dir, id, CLUSTER_ELECTIONS.0)?,
        append: gauge(dir, id, CLUSTER_APPEND_POSITION.0)?,
        commit: gauge(dir, id, CLUSTER_COMMIT_POSITION.0)?,
    })
}

/// The three members' states once every member has one; `None` before.
fn states(dir: &Path) -> Option<[State; 3]> {
    Some([state(dir, 0)?, state(dir, 1)?, state(dir, 2)?])
}

/// Waits until `done` says so of the three states, or fails with `what`.
fn wait_until(
    dir: &Path,
    what: &str,
    timeout: Duration,
    mut done: impl FnMut(&[State; 3]) -> bool,
) -> [State; 3] {
    let deadline = Instant::now() + timeout;
    loop {
        let now = states(dir);
        if let Some(states) = &now
            && done(states)
        {
            return *states;
        }
        assert!(Instant::now() < deadline, "{what}: {now:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// How many entries the member at `id` committed in `window`.
fn committed_in(dir: &Path, id: usize, window: Duration) -> (i64, [State; 3]) {
    let before = states(dir).expect("three members");
    std::thread::sleep(window);
    let after = states(dir).expect("three members");
    ((after[id].commit - before[id].commit) / FRAME, after)
}

/// Entries a second, for the line the test prints.
fn rate(entries: i64, window: Duration) -> i64 {
    entries * 1000 / i64::try_from(window.as_millis()).expect("a window")
}

/// The committed log of member `id` from 0 to `end`, as a service beside it reads it
/// (`spec/cluster.md`, "The service"): how many entries it holds and a hash of their
/// positions and bytes. It holds the log to its shape as it reads: a new leadership
/// term event first, then the entries the leader offered, numbered from 0 with none
/// missing and none twice, each [`FRAME`] after the one before, but for the first of
/// a term of the log's stream, 16 MiB, which a padding frame precedes when the term
/// before had no room for it (`spec/udp.md`, "The datagram").
fn read_log(dir: &Path, id: i32, end: i64) -> (u32, u64) {
    let term = i64::from(UDP_TERM_LENGTH);
    let mut client =
        Cavalry::connect_to(dir.join(format!("member-{id}"))).expect("the service connects");
    let deadline = Instant::now() + Duration::from_secs(120);
    let (mut count, mut hash) = (0u32, 0xcbf2_9ce4_8422_2325u64);
    let mut position = 0;
    let mut next = 0;
    while position < end {
        let mut log = client
            .committed_log(position)
            .expect("the driver answers")
            .expect("the member has its log");
        while !(log.is_ended() || log.position() >= end) {
            assert!(
                Instant::now() < deadline,
                "member {id}: the log read to {} of {end}",
                log.position()
            );
            let delivered = log
                .poll(
                    |entry, _, at| {
                        let room = term - next % term;
                        if room < FRAME {
                            next += room;
                        }
                        assert_eq!(at, next, "member {id}: entry {count}");
                        next = at + FRAME;
                        if count > 0 {
                            let number = count - 1;
                            let mut payload = [0u8; 8];
                            payload[..4].copy_from_slice(&number.to_le_bytes());
                            payload[4..].copy_from_slice(&(!number).to_le_bytes());
                            assert!(
                                entry.len() == 40 && entry[32..] == payload,
                                "member {id}: the entry at {at} is not number {number}"
                            );
                        }
                        for byte in at.to_le_bytes().iter().chain(entry) {
                            hash = (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3);
                        }
                        count += 1;
                    },
                    256,
                )
                .expect("the committed log polls");
            if delivered == 0 {
                std::thread::sleep(Duration::from_micros(200));
            }
        }
        assert_eq!(
            (log.failed_at(), log.withdrawn_at(), log.faulted()),
            (None, None, None),
            "member {id}: a handle that ends, ends whole"
        );
        assert!(
            log.position() > position,
            "member {id}: nothing read at {position}"
        );
        position = log.position();
        log.close(&mut client).expect("the handle closes");
    }
    client.close().expect("the service's client closes");
    (count, hash)
}

/// Throttles member `id` for [`THROTTLED`] and says how many entries the leader
/// committed meanwhile, the states at the end, and what the throttle was.
fn throttle(
    cluster: &Cluster,
    how: &Throttle,
    id: i32,
    leader: usize,
) -> (i64, [State; 3], String) {
    match how {
        Throttle::Cgroup { .. } => {
            let scope = format!("{}.scope", unit(id));
            let quota = format!("CPUQuota={QUOTA}");
            assert!(
                ran(
                    "sudo",
                    &[
                        "-n",
                        "systemctl",
                        "set-property",
                        "--runtime",
                        &scope,
                        &quota
                    ]
                ),
                "the quota is set on {scope}"
            );
            let (entries, states) = committed_in(&cluster.dir, leader, THROTTLED);
            // The kernel's own count, read before the quota is lifted.
            let group = output(
                "systemctl",
                &["show", "-p", "ControlGroup", "--value", &scope],
            )
            .expect("the scope's control group");
            let stat = std::fs::read_to_string(format!("/sys/fs/cgroup{group}/cpu.stat"))
                .expect("the scope's cpu.stat");
            let read = |name: &str| {
                stat.lines()
                    .find_map(|line| line.strip_prefix(name)?.trim().parse::<u64>().ok())
                    .expect("a line of cpu.stat")
            };
            let (periods, throttled) = (read("nr_periods "), read("nr_throttled "));
            assert!(
                ran(
                    "sudo",
                    &[
                        "-n",
                        "systemctl",
                        "set-property",
                        "--runtime",
                        &scope,
                        "CPUQuota="
                    ]
                ),
                "the quota is lifted from {scope}"
            );
            assert!(
                throttled > 0,
                "the kernel throttled member {id} in none of {periods} periods: it used less \
                 than {QUOTA} of a processor, so the test throttled nothing; raise RATE or \
                 lower QUOTA in crates/cavalry-raft/tests/throttle.rs"
            );
            (
                entries,
                states,
                format!(
                    "by its cgroup, CPUQuota={QUOTA}, throttled in {throttled} of {periods} \
                     periods"
                ),
            )
        }
        Throttle::StopAndContinue { why } => {
            let process = cluster.process(id);
            let before = states(&cluster.dir).expect("three members");
            let started = Instant::now();
            // The intervals in which the leader's commit position rose while the
            // member was stopped outright: commits that the two others made alone.
            let mut alone = 0u32;
            let mut intervals = 0u32;
            while started.elapsed() < THROTTLED {
                assert!(ran("kill", &["-STOP", &process]), "member {id} is stopped");
                let stopped = states(&cluster.dir).expect("three members")[leader].commit;
                std::thread::sleep(STOPPED);
                let still = states(&cluster.dir).expect("three members")[leader].commit;
                assert!(
                    ran("kill", &["-CONT", &process]),
                    "member {id} is continued"
                );
                intervals += 1;
                alone += u32::from(still > stopped);
                std::thread::sleep(RUNS);
            }
            // A quorum is the leader and the other follower. A cluster that needed
            // all three to commit would commit nothing while one is stopped.
            assert!(
                alone > 0,
                "the leader's commit position rose in none of the {intervals} intervals \
                 in which member {id} was stopped: the two others did not commit alone"
            );
            let after = states(&cluster.dir).expect("three members");
            (
                (after[leader].commit - before[leader].commit) / FRAME,
                after,
                format!(
                    "by SIGSTOP and SIGCONT, stopped {STOPPED:?} in every {:?} ({why}); \
                     the two others committed alone in {alone} of its {intervals} stops",
                    STOPPED + RUNS
                ),
            )
        }
    }
}

fn test() {
    let dir =
        TestDir(std::env::temp_dir().join(format!("cavalry-raft-throttle-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&dir.0);
    std::fs::create_dir_all(&dir.0).expect("the test's directory");
    let how = Throttle::find();
    let mut cluster = Cluster::start(&dir.0, &members_string(), &how);
    let dir = dir.0.as_path();

    // A leader, every election closed, and the term's first entry committed on all.
    let elected = wait_until(
        dir,
        "the three elected a leader",
        Duration::from_secs(90),
        |s| {
            s.iter()
                .all(|m| m.election_state == NO_ELECTION && m.commit >= FRAME)
                && s.iter().filter(|m| m.role == LEADER).count() == 1
        },
    );
    let leader = elected
        .iter()
        .position(|m| m.role == LEADER)
        .expect("one member leads");
    let throttled = (0..3).find(|&id| id != leader).expect("a follower");
    let throttled_id = i32::try_from(throttled).expect("a member id");

    let (before, _) = committed_in(dir, leader, BEFORE);
    let (during, at_end, what) = throttle(&cluster, &how, throttled_id, leader);
    let behind = (at_end[leader].append - at_end[throttled].append) / FRAME;
    let lifted = Instant::now();
    let reach = at_end[leader].append;
    wait_until(
        dir,
        "the member that was throttled holds what the leader held when the throttle ended",
        Duration::from_secs(60),
        |s| s[throttled].append >= reach,
    );
    let held = lifted.elapsed();
    let (after, _) = committed_in(dir, leader, BEFORE);

    // The leader offers no more, and the three come to one end.
    std::fs::write(dir.join("quiet"), b"").expect("the quiet file");
    let mut last = None;
    let settled = wait_until(
        dir,
        "the three hold one log",
        Duration::from_secs(60),
        |s| {
            let level = s
                .iter()
                .all(|m| m.commit == s[leader].commit && m.append == s[leader].commit);
            let still = level && last == Some(s[leader].commit);
            last = level.then_some(s[leader].commit);
            still
        },
    );
    let end = settled[leader].commit;

    // The cluster stayed available with a quorum, and the throttled member unseated
    // nobody: no member began an election, and the leader and the term are the same.
    assert!(
        during > 0,
        "nothing was committed while member {throttled} was throttled {what}"
    );
    for (id, (now, then)) in settled.iter().zip(&elected).enumerate() {
        assert_eq!(
            (now.role, now.term, now.elections, now.election_state),
            (then.role, then.term, then.elections, then.election_state),
            "member {id} began an election: {settled:?}"
        );
    }

    // Every committed entry is in every member's log, once and in order.
    let logs: Vec<(u32, u64)> = (0..3).map(|id| read_log(dir, id, end)).collect();
    let entries = logs[0].0;
    for (id, log) in logs.iter().enumerate() {
        assert_eq!(log, &logs[0], "member {id} holds the log member 0 holds");
    }
    assert!(cluster.stop(), "every member's process ended with success");
    println!(
        "throttle: member {throttled}, a follower of member {leader}, was throttled {what} for \
         {THROTTLED:?}; the cluster committed {} entries/s before, {} under it and {} after; \
         the member was {behind} entries behind when it ended and held them {held:?} later; \
         the three logs hold the same {} entries",
        rate(before, BEFORE),
        rate(during, THROTTLED),
        rate(after, BEFORE),
        entries - 1
    );
}
