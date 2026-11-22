//! One feed run as a process of its own: what a test starts when the server's process
//! is what it tests, and what `conformance/upgrade/` builds from two trees. The feed
//! is this directory's `mod.rs`; this file adds nothing to it but a way to drive it
//! from outside.
//!
//! [`run`] opens the feed, says `open <where it begins>`, and then does what its
//! standard input says until that ends. Each line in is one command, and each command
//! is answered by lines that end with one line whose first word says how it went:
//!
//! - `append <first> <count>`: appends records `first` to `first + count - 1`, each
//!   `record(number, length(number))`. Answers a `record <position after> <length>
//!   <digest>` line for each record as it was appended, then `appended` with the
//!   position after each record.
//! - `read <from>`: answers a `record <position after> <length> <digest>` line for
//!   each record from `from` to where this run began, then `read <where it began>`.
//! - `release <before>`: lets go of what a snapshot at `before` covers, as the server
//!   does when it has taken one. Answers `released`.
//! - `hold`: answers `held` and then does nothing more, with the feed open: a process
//!   to kill.
//! - `end`: stops the feed as the server's clean stop does, and answers `ended`.
//!
//! Before the last line of an answer comes a `note` line for each thing the feed had
//! to say. A command that failed is answered `failed` and why, and the process goes
//! on. A process whose input ends stops its feed and ends; one that lives for
//! [`LIFETIME`] ends there, whatever became of the program that started it.

use std::io::{BufRead, Write};
use std::time::{Duration, Instant};

use super::{FeedLog, FeedOptions, MOVED_TO, PINNED, Shape, record};

/// How long a feed process lives at most.
pub const LIFETIME: Duration = Duration::from_secs(300);

/// The length of record `number`: within one frame, and across several.
pub fn length(number: u64) -> usize {
    [1, 100, 1_000, 5_000, 40_000][(number % 5) as usize]
}

/// FNV-1a over a record's bytes: what a feed process says of a record it read back,
/// so that records are compared without passing them through a pipe.
pub fn digest(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// The line a feed process writes for a record it read back.
pub fn record_line(bytes: &[u8], after: i64) -> String {
    format!("record {after} {} {}", bytes.len(), digest(bytes))
}

/// The shape `name` names, `pinned` or `moved-to`.
pub fn shape_named(name: &str) -> Option<Shape> {
    match name {
        "pinned" => Some(PINNED),
        "moved-to" => Some(MOVED_TO),
        _ => None,
    }
}

fn say(line: &str) {
    let mut out = std::io::stdout().lock();
    // A program that has gone reads nothing: the feed ends with its input.
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

/// Says what the feed had to say, then how the command went.
fn answer(log: &mut FeedLog, outcome: Result<String, String>) {
    for note in log.notes() {
        say(&format!("note {note}"));
    }
    match outcome {
        Ok(line) => say(&line),
        Err(error) => say(&format!("failed {error}")),
    }
}

/// Runs one feed until standard input ends, as the module's documentation says.
/// Returns the process's exit code: 0, or 1 when the feed did not open or a line was no
/// command.
pub fn run(options: &FeedOptions) -> i32 {
    let started = Instant::now();
    std::thread::spawn(move || {
        std::thread::sleep(LIFETIME.saturating_sub(started.elapsed()));
        std::process::exit(3);
    });
    let mut log = match FeedLog::open(options) {
        Ok(log) => log,
        Err(error) => {
            say(&format!("failed {error}"));
            return 1;
        }
    };
    say(&format!("open {}", log.begin()));
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let words: Vec<&str> = line.split(' ').collect();
        let number = |word: &str| word.parse::<i64>().ok().filter(|number| *number >= 0);
        match words.as_slice() {
            ["append", first, count] => {
                let (Some(first), Some(count)) = (number(first), number(count)) else {
                    say(&format!("failed `{line}` names no records"));
                    return 1;
                };
                let records: Vec<Vec<u8>> = (first.unsigned_abs()..(first + count).unsigned_abs())
                    .map(|number| record(number, length(number)))
                    .collect();
                let appended = log.append(&records).map(|positions| {
                    for (record, after) in records.iter().zip(&positions) {
                        say(&record_line(record, *after));
                    }
                    let positions: Vec<String> = positions.iter().map(i64::to_string).collect();
                    format!("appended {}", positions.join(" "))
                });
                answer(&mut log, appended);
            }
            ["read", from] => {
                let Some(from) = number(from) else {
                    say(&format!("failed `{line}` names no position"));
                    return 1;
                };
                let mut lines = Vec::new();
                let read = log.read_from(from, &mut |record, after| {
                    lines.push(record_line(record, after));
                });
                for line in &lines {
                    say(line);
                }
                answer(&mut log, read.map(|begin| format!("read {begin}")));
            }
            ["release", before] => {
                let Some(before) = number(before) else {
                    say(&format!("failed `{line}` names no position"));
                    return 1;
                };
                let released = log.release(before).map(|()| "released".to_owned());
                answer(&mut log, released);
            }
            ["hold"] => {
                say("held");
                loop {
                    std::thread::sleep(Duration::from_secs(1));
                }
            }
            ["end"] => {
                drop(log);
                say("ended");
                return 0;
            }
            _ => {
                say(&format!("failed `{line}` is no command of a feed process"));
                return 1;
            }
        }
    }
    0
}
