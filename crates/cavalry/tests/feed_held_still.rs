//! Brigade's server held still, and then killed (work item 362; ADR 0138, condition 9;
//! invariant 1). The feed is `tests/feed/mod.rs`, the server's own use of Cavalry;
//! `tests/feed_shape.rs` holds the rest of it and says what the feed is.
//!
//! A server is one process that embeds the driver and is its only client. A host that
//! holds that process still for longer than the client liveness timeout and the driver
//! timeout, 10 s each (`spec/client-protocol.md`, "Liveness and timeouts"), as a host
//! does while it moves a virtual machine, gives it back with its client closed. The
//! server's next append connects a new client to the driver it still runs, adds the
//! feed again where its recording stopped, and goes on. Nothing that was answered is
//! lost and nothing is in the feed twice. A kill of the process after that loses
//! nothing either: every append was synced before it was answered, and the next start
//! begins the feed at the last record answered.
//!
//! The test, for both shapes of the feed at once, [`feed::PINNED`] and
//! [`feed::MOVED_TO`]:
//!
//! 1. A feed process starts on directories of its own and appends four records.
//! 2. The process is stopped with `SIGSTOP` for 12 s and continued with `SIGCONT`.
//! 3. It appends three records. The append must say that the client was closed and
//!    that the feed goes on at the last position answered before the stop, and its
//!    positions go on from there. It appends two more, with nothing to say.
//! 4. The process is killed with `SIGKILL`.
//! 5. A new feed process on the same directories begins at the last position
//!    answered, and reads back every record once, in order, each at the position it
//!    was answered with.
//!
//! # Why the feed is a process
//!
//! Only a process can be held still with its driver and its client in it, which is
//! what the server meets: this program started again with [`FEED`] set, which runs one
//! feed and does what its standard input says until that ends
//! (`tests/feed/process.rs`). The two feeds are
//! stopped over the same 12 s, so the test waits for them once.
//!
//! A process the test has stopped cannot end itself. A test that is ended by `SIGKILL`
//! or by an interrupt inside those 12 s leaves two stopped processes, each holding one
//! or two UDP ports and a directory under the system's temporary one. `kill -CONT`
//! with the process id lets each end by itself, at the end of its standard input or
//! at `tests/feed/process.rs`'s `LIFETIME`; the test prints the ids before it stops them. A runner is thrown
//! away after its job, so this is a laptop's matter.
//!
//! A program with no test harness (`harness = false` in the crate's `Cargo.toml`), as
//! `crates/cavalry-raft/tests/throttle.rs` is and for its reasons: a program that
//! starts itself again needs its own `main`, and it chooses its ports on its one
//! thread before it starts its first process (`crates/cavalry-raft/tests/ports/mod.rs`,
//! "What the rule does not cover"). `cargo test` runs it with the others, whatever
//! filter it is given.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

mod feed;
use feed::process::{length, record_line, shape_named};
use feed::{FeedOptions, record};

// This test's ports, from the one file that says how they are taken and why, which is
// `cavalry-raft`'s: `crates/cavalry-raft/tests/ports/mod.rs`.
#[path = "../../cavalry-raft/tests/ports/mod.rs"]
mod ports;
use ports::free_ports;

/// Set for a feed process: the shape's name, `pinned` or `moved-to`.
const FEED: &str = "CAVALRY_FEED_HELD_STILL";
/// For a feed process: the directory its driver directory and log directory are in.
const DIR: &str = "CAVALRY_FEED_HELD_STILL_DIR";
/// For a feed process: the feed's control address.
const CONTROL: &str = "CAVALRY_FEED_HELD_STILL_CONTROL";
/// For a feed process: the address the feed reads itself on.
const OWN: &str = "CAVALRY_FEED_HELD_STILL_OWN";

/// How long the feed processes are held still: past the client liveness timeout and
/// the driver timeout, 10 s each. The server's own test holds its server for as long.
const HELD: Duration = Duration::from_secs(12);
/// How long the test waits for one answer of a feed process. An append that meets a
/// closed client connects again inside it.
const ANSWER: Duration = Duration::from_secs(60);

fn main() {
    match std::env::var(FEED) {
        Ok(shape) => feed_process(&shape),
        Err(_) => test(),
    }
}

/// One feed's process: the feed that the variables name, run as
/// `tests/feed/process.rs` says, until standard input ends.
fn feed_process(shape: &str) {
    let dir = PathBuf::from(std::env::var(DIR).expect("the feed's directory"));
    let options = FeedOptions {
        driver_dir: dir.join("driver"),
        log_dir: dir.join("log"),
        control: std::env::var(CONTROL).expect("the feed's control address"),
        own_endpoint: std::env::var(OWN).expect("the feed's own endpoint"),
        roll_bytes: u64::MAX,
        shape: shape_named(shape).expect("the name of a shape of the feed"),
    };
    std::process::exit(feed::process::run(&options));
}

/// A directory of its own per feed, removed when the guard drops.
struct TestDir(PathBuf);

impl TestDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "cavalry-feed-held-still-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("the test directory");
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

fn ran(program: &str, arguments: &[&str]) -> bool {
    Command::new(program)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// One feed's process, as the test holds it.
struct Process {
    child: Child,
    input: Option<ChildStdin>,
    /// The lines the process wrote, from a thread that reads its output.
    lines: Receiver<String>,
}

impl Drop for Process {
    fn drop(&mut self) {
        // A process that is stopped takes no kill until it is continued.
        let _ = ran("kill", &["-CONT", &self.child.id().to_string()]);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// One feed: its shape, the process that runs it now, its directories and addresses,
/// and what the test appended to it.
struct Feed {
    name: &'static str,
    /// Before `dir`, because fields drop in order: a test that panics kills the
    /// feed's process before it removes the directory the process writes in.
    process: Option<Process>,
    dir: TestDir,
    control: String,
    own: String,
    /// The position after each record the feed answered, in order: record `i` is
    /// `record(i, length(i))`.
    answered: Vec<i64>,
}

impl Feed {
    /// The feed's directories and its two ports. Chosen for both feeds before the
    /// first process starts.
    fn new(name: &'static str) -> Self {
        let ports = free_ports(2);
        Self {
            name,
            process: None,
            dir: TestDir::new(name),
            control: format!("127.0.0.1:{}", ports[0]),
            own: format!("127.0.0.1:{}", ports[1]),
            answered: Vec::new(),
        }
    }

    /// Starts the feed's process: this program again, with [`FEED`] set.
    fn start(&mut self) {
        let program = std::env::current_exe().expect("this program's path");
        let mut child = Command::new(program)
            .env(FEED, self.name)
            .env(DIR, self.dir.path())
            .env(CONTROL, &self.control)
            .env(OWN, &self.own)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("the feed's process starts");
        let input = child.stdin.take();
        let output = child.stdout.take().expect("the process's output");
        let (send, lines) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                let Ok(line) = line else { break };
                if send.send(line).is_err() {
                    break;
                }
            }
        });
        self.process = Some(Process {
            child,
            input,
            lines,
        });
    }

    fn id(&self) -> String {
        self.process
            .as_ref()
            .expect("the feed's process")
            .child
            .id()
            .to_string()
    }

    /// Sends one command to the feed's process.
    fn ask(&mut self, command: &str) {
        let process = self.process.as_mut().expect("the feed's process");
        let input = process.input.as_mut().expect("the process's input");
        writeln!(input, "{command}").expect("the feed's process takes a command");
        input.flush().expect("the feed's process takes a command");
    }

    /// The process's lines up to the one that begins with `last`, which is returned
    /// without that word, and the lines before it. Fails on a `failed` line, on the
    /// end of the output and when [`ANSWER`] passes.
    fn answer(&mut self, last: &str) -> (Vec<String>, String) {
        let name = self.name;
        let process = self.process.as_mut().expect("the feed's process");
        let deadline = Instant::now() + ANSWER;
        let mut before = Vec::new();
        loop {
            let wait = deadline.saturating_duration_since(Instant::now());
            let line = process.lines.recv_timeout(wait).unwrap_or_else(|error| {
                panic!(
                    "the {name} feed's process did not answer with `{last}` within {ANSWER:?} \
                     ({error}); it said before that: {before:?}"
                )
            });
            if let Some(rest) = line.strip_prefix(last) {
                return (before, rest.trim_start().to_owned());
            }
            assert!(
                !line.starts_with("failed"),
                "the {name} feed's process: {line}; it said before that: {before:?}"
            );
            before.push(line);
        }
    }

    /// Waits for the process to say where its feed begins.
    fn opened(&mut self) -> i64 {
        let (_, begin) = self.answer("open");
        begin.parse().expect("a position")
    }

    /// Appends the next `count` records and returns what the feed had to say.
    fn append(&mut self, count: usize) -> Vec<String> {
        let first = self.answered.len();
        self.ask(&format!("append {first} {count}"));
        let (lines, positions) = self.answer("appended");
        // What the feed had to say, without the line it writes for each record.
        let notes: Vec<String> = lines
            .into_iter()
            .filter(|line| !line.starts_with("record "))
            .collect();
        let positions: Vec<i64> = positions
            .split(' ')
            .map(|position| position.parse().expect("a position"))
            .collect();
        assert_eq!(
            positions.len(),
            count,
            "{}: a position for every record",
            self.name
        );
        let mut last = self.answered.last().copied().unwrap_or(0);
        for position in &positions {
            assert!(
                *position > last,
                "{}: the position after a record is past the one before it: {position} after \
                 {last}",
                self.name
            );
            last = *position;
        }
        self.answered.extend(positions);
        notes
    }

    /// Fails unless the feed reads back, from position 0, every record the test
    /// appended: once, in order, at the position it was answered with.
    fn reads_back_everything(&mut self) {
        self.ask("read 0");
        let (records, begin) = self.answer("read");
        let expected: Vec<String> = self
            .answered
            .iter()
            .enumerate()
            .map(|(number, after)| {
                let number = number as u64;
                record_line(&record(number, length(number)), *after)
            })
            .collect();
        assert_eq!(
            records, expected,
            "{}: the feed reads back each record once, in order, at its position",
            self.name
        );
        assert_eq!(
            begin.parse::<i64>().ok(),
            self.answered.last().copied(),
            "{}: the read ends where the run began, at the last record answered",
            self.name
        );
    }
}

fn test() {
    let began = Instant::now();
    // Both feeds' ports before the first process: one thread, as the ports' rule asks.
    let mut feeds = [Feed::new("pinned"), Feed::new("moved-to")];
    for feed in &mut feeds {
        feed.start();
    }
    for feed in &mut feeds {
        assert_eq!(
            feed.opened(),
            0,
            "{}: a new feed begins at position 0",
            feed.name
        );
        let notes = feed.append(4);
        assert!(
            notes.is_empty(),
            "{}: nothing to say yet: {notes:?}",
            feed.name
        );
    }

    println!(
        "feed_held_still: processes {} and {} are stopped for {HELD:?}; should this test be \
         killed meanwhile, `kill -CONT` with each id lets it end",
        feeds[0].id(),
        feeds[1].id()
    );
    for feed in &feeds {
        assert!(
            ran("kill", &["-STOP", &feed.id()]),
            "{} is stopped",
            feed.name
        );
    }
    std::thread::sleep(HELD);
    for feed in &feeds {
        assert!(
            ran("kill", &["-CONT", &feed.id()]),
            "{} is continued",
            feed.name
        );
    }

    for feed in &mut feeds {
        let held_at = feed.answered.last().copied().expect("four records");
        let notes = feed.append(3);
        // The stop was long enough to close the client, so this is the path taken.
        assert_eq!(
            notes,
            [format!(
                "note the Cavalry client was closed under an append; connected again, and the \
                 feed goes on at position {held_at}"
            )],
            "{}: the append after the stop connects again where the feed stopped",
            feed.name
        );
        let notes = feed.append(2);
        assert!(
            notes.is_empty(),
            "{}: nothing more to say: {notes:?}",
            feed.name
        );
    }

    // A kill, as a power cut is to a process: no signal it can handle.
    for feed in &mut feeds {
        let mut process = feed.process.take().expect("the feed's process");
        process.child.kill().expect("the feed's process is killed");
        process.child.wait().expect("the feed's process has ended");
    }
    for feed in &mut feeds {
        feed.start();
    }
    for feed in &mut feeds {
        assert_eq!(
            Some(feed.opened()),
            feed.answered.last().copied(),
            "{}: after the kill the feed begins at the last record answered",
            feed.name
        );
        feed.reads_back_everything();
        feed.ask("end");
        feed.answer("ended");
        let mut process = feed.process.take().expect("the feed's process");
        drop(process.input.take());
        let status = process.child.wait().expect("the feed's process has ended");
        assert!(
            status.success(),
            "{}: the feed's process ended with {status}",
            feed.name
        );
    }
    println!(
        "feed_held_still: ok in {:.1} s: both feeds, held still for {HELD:?}, connected again \
         where they stopped; after a kill each began at its last record answered and read back \
         {} records, each once, in order",
        began.elapsed().as_secs_f64(),
        feeds[0].answered.len()
    );
}
