//! The service of `docs/tutorials/run-a-cluster.md`: an adder. It runs beside one
//! cluster member, as a client of the member's driver, and reads the cluster's
//! committed log through a [`ClusterService`](cavalry::ClusterService): every entry a
//! quorum of members holds, in order, each once. Its state is one number, the total.
//! A session's message is a whole number in decimal text; the service adds it to the
//! total and replies with the total, so `0` reads the total. A message that is no
//! such number, or that would take the total past what an `i64` holds, changes
//! nothing and is answered with `error:` and why.
//!
//! One service runs beside every member, and all of them run this code on the same
//! entries, so all of them hold the same total. One of them sends each reply: the
//! service beside the member that led the entry's term (`spec/cluster.md`, "The
//! service" and "Replies"). When that member is lost, the others elect a leader, and
//! the service beside it answers from then on, with the total it already holds.
//!
//! `--dir <path>` is the member's driver directory; without it the program attaches
//! to the driver at `CAVALRY_DIR`, or the default directory. The program may start
//! before its member, with it or after it. It reads the log from its start, so a
//! service that starts late, or starts again, applies every entry and reaches the
//! same total, and it answers no entry that a service before it answered. It runs
//! until it is stopped, or until its member's driver closes.
//!
//! The service's state depends on the log alone: it reads no clock but the entry's
//! cluster time, draws no random number and reads no file. That is what makes the
//! total the same beside every member.
//!
//! The program prints a line for each entry, so that the tutorial's reader sees what
//! each service did. Each line begins with the last part of the driver directory's
//! path, `cavalry-member-0` for `/tmp/cavalry-member-0`, so that the lines of three
//! services in one terminal can be told apart. Printing makes the program no model
//! for a service that must hold a latency budget, which prints nothing and builds no
//! string for a message.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use cavalry::{Action, Cavalry, Entry, Reply};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("adder-service: {error}");
            ExitCode::FAILURE
        }
    }
}

/// The driver directory `--dir` names, or none.
fn parse(mut args: impl Iterator<Item = String>) -> Result<Option<PathBuf>, String> {
    let mut dir = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--dir" => {
                dir = Some(PathBuf::from(args.next().ok_or("--dir needs a path")?));
            }
            other => {
                return Err(format!(
                    "{other} is not an argument of this program; usage: adder-service \
                     [--dir <the member's driver directory>]; see \
                     docs/tutorials/run-a-cluster.md"
                ));
            }
        }
    }
    Ok(dir)
}

/// What a message asks the service to add: a whole number in decimal text.
fn number(payload: &[u8]) -> Option<i64> {
    core::str::from_utf8(payload).ok()?.trim().parse().ok()
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = match parse(std::env::args().skip(1))? {
        Some(dir) => Cavalry::connect_to(dir)?,
        None => Cavalry::connect()?,
    };
    let mut service = client.cluster_service();
    // What tells this service's lines from another's: the last part of its driver
    // directory's path.
    let beside = client
        .dir()
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| client.dir().display().to_string());
    println!(
        "adder-service {beside}: beside the driver at {}, reading its member's \
         committed log",
        client.dir().display()
    );
    // The service's state.
    let mut total: i64 = 0;
    loop {
        let taken = service.poll(
            &mut client,
            |committed, replies| {
                let position = committed.position;
                let term = committed.leadership_term_id;
                match committed.entry {
                    Entry::SessionMessage {
                        session_id,
                        payload,
                    } => {
                        // What the message leads to, worked out before anything
                        // changes.
                        let sum = number(payload).map(|n| total.checked_add(n));
                        let answer = match sum {
                            Some(Some(sum)) => sum.to_string(),
                            Some(None) => {
                                "error: the total would pass what an i64 holds".to_string()
                            }
                            None => "error: the message is not a whole number".to_string(),
                        };
                        // Reply first, then change the state: an entry that is
                        // aborted is handed over again, and must find the state as
                        // the first handling found it.
                        let reply = replies.reply(session_id, answer.as_bytes());
                        if reply == Reply::Retry {
                            return Action::Abort;
                        }
                        if let Some(Some(sum)) = sum {
                            total = sum;
                        }
                        let replied = match reply {
                            Reply::Sent => "this service replied".to_string(),
                            Reply::NotAnswering => {
                                "the reply is not this service's to send".to_string()
                            }
                            other => format!("the reply was not sent: {other:?}"),
                        };
                        println!(
                            "adder-service {beside}: position {position}, term {term}: \
                             session {session_id} sent {}; the total is {total}; {replied}",
                            String::from_utf8_lossy(payload)
                        );
                    }
                    Entry::SessionOpen { session_id } => println!(
                        "adder-service {beside}: position {position}, term {term}: \
                         session {session_id} opened"
                    ),
                    Entry::SessionClose {
                        session_id,
                        timed_out,
                    } => println!(
                        "adder-service {beside}: position {position}, term {term}: \
                         session {session_id} closed{}",
                        if timed_out {
                            ", by the leader, for the session timeout"
                        } else {
                            ""
                        }
                    ),
                    Entry::NewLeadershipTerm { leader_member_id } => println!(
                        "adder-service {beside}: position {position}: term {term} began, \
                         led by member {leader_member_id}"
                    ),
                    Entry::Other => {}
                }
                Action::Continue
            },
            10,
        )?;
        if let Some(closed) = client.closed() {
            // The member's driver is gone, or dropped this client: there is no log
            // to read here any more. The total is in the log, on every member that
            // holds it.
            return Err(format!(
                "the driver at {} closed this service's client ({}) with the total at \
                 {total}, read to position {}; the log is on the members that still \
                 run; start this member again, then this program, which reads the log \
                 from its start; see docs/tutorials/run-a-cluster.md",
                client.dir().display(),
                closed.reason(),
                service.position()
            )
            .into());
        }
        if taken == 0 {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}
