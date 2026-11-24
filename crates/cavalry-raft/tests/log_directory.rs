//! A cluster member's log directory, as ADR 0152 has it (`spec/durable-log.md`, "The
//! driver").
//!
//! The consensus module's hold on the directory: a second module on a directory one
//! holds is refused with `log-directory-held` and writes nothing, whatever its member
//! id and whatever its process, and the hold ends with the module, or with its process
//! however that ends, so that a member whose process was killed is started again on
//! its directory at once. The modules here are clients of a driver in the test's own
//! process, as a member embedded in a program of its own is a client of the host's
//! driver: two such programs on one host reach one log directory through one driver,
//! and only the module's own hold keeps the second out. `cavalry run` holds the driver
//! and the member in one process, so there a second process on the directory is
//! refused by the driver's lock on the catalog first (ADR 0025).
//!
//! And a start beside a `terms` file that is gone while the member's other files are
//! there: with no log, the member starts as one whose log is empty, which ADR 0145's
//! rule keeps from a second vote, and is not refused.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use cavalry_driver::Driver;
use cavalry_log::recordings::RecordingsFile;
use cavalry_log::terms::TermsFile;
use cavalry_raft::{ConsensusModule, RaftError};

mod ports;
use ports::free_ports;

/// The environment of the child of `a_member_whose_process_was_killed_starts_again_on_its_log_directory`:
/// the driver directory, the log directory and the members string.
const CHILD_DIR: &str = "CAVALRY_HOLD_CHILD_DIR";
const CHILD_LOG_DIR: &str = "CAVALRY_HOLD_CHILD_LOG_DIR";
const CHILD_MEMBERS: &str = "CAVALRY_HOLD_CHILD_MEMBERS";
/// The line the child prints once its module has started and holds the directory.
const HELD: &str = "cavalry-hold-test: the module started";
/// How long the child runs at most, should the test that started it end without
/// killing it.
const CHILD_LIFE: Duration = Duration::from_secs(60);

struct TestDir(PathBuf);

impl TestDir {
    fn new(test: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "cavalry-raft-log-directory-{}-{test}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        TestDir(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

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

/// The bytes of the member's three files in `log_dir`, in the order `terms`,
/// `log-recordings`, `snapshots`.
fn files(log_dir: &Path) -> Vec<Vec<u8>> {
    ["terms", "log-recordings", "snapshots"]
        .iter()
        .map(|name| std::fs::read(log_dir.join(name)).expect("the member's file is there"))
        .collect()
}

/// The refusal of a start on a directory another module holds, and that it names the
/// directory and what to do.
fn assert_held(refused: Result<ConsensusModule, RaftError>, log_dir: &Path) {
    let error = match refused {
        Ok(_) => panic!("a second module started on a log directory a module holds"),
        Err(error) => error,
    };
    assert!(
        matches!(&error, RaftError::LogDirectoryHeld { log_dir: held } if held == log_dir),
        "{error:?}"
    );
    assert_eq!(error.reason(), "log-directory-held");
    let text = error.to_string();
    assert!(
        text.starts_with(&format!(
            "member refused (log-directory-held): another consensus module holds the log \
             directory {}",
            log_dir.display()
        )),
        "{text}"
    );
    assert!(text.contains("Nothing was written"), "{text}");
    assert!(
        text.contains(&format!("`lsof {}`", log_dir.display())),
        "{text}"
    );
    assert!(
        text.ends_with("see docs/reference/durable-log.md#the-driver"),
        "{text}"
    );
}

#[test]
fn a_second_module_on_a_held_log_directory_is_refused_and_writes_nothing() {
    let dir = TestDir::new("second");
    let (driver_dir, log_dir) = (dir.path().join("driver"), dir.path().join("log"));
    let _driver = Driver::start(&driver_dir, &log_dir).expect("the driver starts");
    let members = members_string();
    let first = ConsensusModule::start(&driver_dir, &log_dir, &members, 1)
        .expect("the first module starts");
    let before = files(&log_dir);

    // The same member id, and another: a log directory holds one member's files.
    assert_held(
        ConsensusModule::start(&driver_dir, &log_dir, &members, 1),
        &log_dir,
    );
    assert_held(
        ConsensusModule::start(&driver_dir, &log_dir, &members, 2),
        &log_dir,
    );
    assert_eq!(files(&log_dir), before, "the refused starts wrote nothing");

    // The hold ends with the module that held it.
    drop(first);
    let again = ConsensusModule::start(&driver_dir, &log_dir, &members, 1)
        .expect("the member starts again once the module that held it is gone");
    assert_held(
        ConsensusModule::start(&driver_dir, &log_dir, &members, 1),
        &log_dir,
    );
    drop(again);
}

/// A member's module in a process of its own, killed with `SIGKILL`: nothing of it
/// closes, and the operating system drops its hold with the process, so the member
/// starts again on its log directory at once, with no wait for a claim to grow old.
#[test]
fn a_member_whose_process_was_killed_starts_again_on_its_log_directory() {
    let dir = TestDir::new("killed");
    let (driver_dir, log_dir) = (dir.path().join("driver"), dir.path().join("log"));
    let _driver = Driver::start(&driver_dir, &log_dir).expect("the driver starts");
    let members = members_string();
    let mut child = Command::new(std::env::current_exe().expect("this program's path"))
        .args([
            "a_module_in_a_process_of_its_own",
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_DIR, &driver_dir)
        .env(CHILD_LOG_DIR, &log_dir)
        .env(CHILD_MEMBERS, &members)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("the child starts");
    // The child's lines, read on a thread of their own so that the wait has a bound
    // whether or not a line comes. The harness writes `test <name> ... ` before the
    // test's own output, on the same line.
    let stdout = child.stdout.take().expect("the child's standard output");
    let (lines, read) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if lines.send(line).is_err() {
                break;
            }
        }
    });
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match read.recv_timeout(left) {
            Ok(line) if line.contains(HELD) => break,
            Ok(_) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                let _ = child.kill();
                panic!("the child's module did not start within 30 s");
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                let status = child.wait().expect("the child is waited for");
                panic!("the child ended before its module started: {status}");
            }
        }
    }

    // The child's module holds the directory, from its own process.
    assert_held(
        ConsensusModule::start(&driver_dir, &log_dir, &members, 1),
        &log_dir,
    );

    child.kill().expect("the child is killed");
    child.wait().expect("the child is waited for");
    let started = Instant::now();
    let module = ConsensusModule::start(&driver_dir, &log_dir, &members, 1)
        .expect("the member starts again on its log directory once its process is killed");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the start waited {:?}: no claim outlives the process",
        started.elapsed()
    );
    drop(module);
}

/// The child of `a_member_whose_process_was_killed_starts_again_on_its_log_directory`:
/// a module of member 1, a client of the parent's driver, that says when it has
/// started and then runs until it is killed, or for [`CHILD_LIFE`] at most. Run alone,
/// with no environment from that test, it does nothing.
#[test]
#[ignore = "the child of a_member_whose_process_was_killed_starts_again_on_its_log_directory"]
fn a_module_in_a_process_of_its_own() {
    let (Ok(dir), Ok(log_dir), Ok(members)) = (
        std::env::var(CHILD_DIR),
        std::env::var(CHILD_LOG_DIR),
        std::env::var(CHILD_MEMBERS),
    ) else {
        return;
    };
    let mut module =
        ConsensusModule::start(&dir, &log_dir, &members, 1).expect("the child's module starts");
    println!("{HELD}");
    let started = Instant::now();
    while started.elapsed() < CHILD_LIFE {
        let _ = module.do_work();
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A member whose `terms` file is gone while its `log-recordings` and `snapshots` files
/// are there, with no log, starts as a member whose log is empty: with no candidate
/// term and no vote floor, as on a new log directory, and with the files it kept. Its
/// log is empty, so ADR 0145's rule holds the vote it may have cast and the leader it
/// may have taken: it withholds while a member's log holds anything. It is not refused,
/// as a new log directory would give it the same state and no more. Since ADR 0152 no
/// crash leaves this: the `terms` file is created, and the directory synced, before the
/// other two. With a log, the start is `terms-lost` (`tests/cluster.rs`).
#[test]
fn a_member_whose_terms_file_is_gone_beside_no_log_starts_with_its_log_empty() {
    let dir = TestDir::new("terms-gone");
    let (driver_dir, log_dir) = (dir.path().join("driver"), dir.path().join("log"));
    let _driver = Driver::start(&driver_dir, &log_dir).expect("the driver starts");
    let members = members_string();
    // A member that voted in term 7 and took a leader with its log empty, recording
    // no term: its candidate term and its vote floor, then its first start.
    let mut terms = TermsFile::create(&log_dir).expect("the terms file is created");
    terms.set_candidate_term(7).expect("the vote is stored");
    terms.set_vote_floor(4096).expect("the floor is stored");
    drop(terms);
    let module =
        ConsensusModule::start(&driver_dir, &log_dir, &members, 1).expect("the member starts");
    module.close().expect("the member closes");
    let kept = RecordingsFile::open_kept(&log_dir, 1).expect("the list is there");
    let (sequence, floor) = (kept.sequence(), kept.floor_position());
    drop(kept);

    std::fs::remove_file(TermsFile::path_in(&log_dir)).expect("the terms file is removed");
    let module = ConsensusModule::start(&driver_dir, &log_dir, &members, 1)
        .expect("a member with no log starts without its terms file");
    assert_eq!(module.append_position(), 0, "its log is empty");
    let terms = TermsFile::open(&log_dir).expect("the terms file was created again");
    assert_eq!(terms.candidate_term(), -1, "the vote went with the file");
    assert_eq!(terms.vote_floor(), None, "and the floor");
    assert_eq!(terms.log_leadership_term_id(), -1);
    drop(terms);
    let kept = RecordingsFile::open_kept(&log_dir, 1).expect("the list is there");
    assert_eq!(
        (kept.sequence(), kept.floor_position()),
        (sequence, floor),
        "the list it kept, unchanged"
    );
    module.close().expect("the member closes");
}
