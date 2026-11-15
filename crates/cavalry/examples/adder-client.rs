//! The client of `docs/tutorials/run-a-cluster.md`: it opens a session with a cluster
//! whose members each run `adder-service` beside them, sends each number on its
//! command line as one message, prints the cluster's reply to each, the total after
//! that number, and closes the session. `adder-client 0` reads the total and changes
//! nothing.
//!
//! ```text
//! adder-client [--dir <driver directory>] [--members <members string>]
//!              [--response <host:port>] <number>...
//! ```
//!
//! The program is a client of a driver on its own host, which need not be a member:
//! `--dir` is that driver's directory, `CAVALRY_DIR` or the default without it.
//! `--members` is the members string the cluster's members were started with, and
//! `CAVALRY_CLUSTER_MEMBERS` without it. `--response` is a UDP host and port of this
//! host that every member can reach, where the cluster answers the session;
//! `127.0.0.1:9100` without it, which serves a cluster on this host alone.
//!
//! Every wait has a bound. The connect waits 20 s for a leader to open the session,
//! long enough for a cluster that has just lost its leader to elect another. An offer
//! the session cannot take is made again for 10 s. A reply is waited for 20 s: a
//! message whose leader was lost before a quorum held it is answered by nobody, and
//! the program then stops and says what to do, since it cannot know whether the
//! number was added.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use cavalry::{Cavalry, ClusterSession, Offer};

/// How long an offer the session does not take is made again.
const OFFER_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the program waits for the reply to one message.
const REPLY_TIMEOUT: Duration = Duration::from_secs(20);
/// Between two duty cycles: a session is polled at least every 500 ms, its keepalive
/// interval, and this program polls far more often than that.
const IDLE: Duration = Duration::from_millis(1);

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("adder-client: {error}");
            ExitCode::FAILURE
        }
    }
}

struct Args {
    dir: Option<PathBuf>,
    members: String,
    response: String,
    numbers: Vec<i64>,
}

const USAGE: &str = "usage: adder-client [--dir <driver directory>] [--members <members \
                     string>] [--response <host:port>] <number>...; see \
                     docs/tutorials/run-a-cluster.md";

fn parse(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut dir = None;
    let mut members = std::env::var("CAVALRY_CLUSTER_MEMBERS").ok();
    let mut response = "127.0.0.1:9100".to_string();
    let mut numbers = Vec::new();
    while let Some(arg) = args.next() {
        let mut value = |what: &str| args.next().ok_or(format!("{arg} needs {what}; {USAGE}"));
        match arg.as_str() {
            "--dir" => dir = Some(PathBuf::from(value("a path")?)),
            "--members" => members = Some(value("a members string")?),
            "--response" => response = value("a host and a port")?,
            number => numbers.push(
                number
                    .parse::<i64>()
                    .map_err(|_| format!("{number} is not a whole number; {USAGE}"))?,
            ),
        }
    }
    if numbers.is_empty() {
        return Err(format!("no number to send; {USAGE}"));
    }
    let members = members.ok_or(format!(
        "no members string: give --members, or set CAVALRY_CLUSTER_MEMBERS to the string \
         the cluster's members were started with; {USAGE}"
    ))?;
    Ok(Args {
        dir,
        members,
        response,
        numbers,
    })
}

/// Sends one number and returns the cluster's reply to it.
fn send(session: &mut ClusterSession, number: i64) -> Result<(String, i64, i64), String> {
    let message = number.to_string();
    let started = Instant::now();
    loop {
        match session.offer(message.as_bytes()) {
            Ok(_) => break,
            // The session's publication to the leader is not connected yet, or is
            // back-pressured, or rotated its term: nothing was sent, and the next
            // offer may be taken.
            Err(Offer::NotConnected | Offer::BackPressured | Offer::AdminAction)
                if started.elapsed() < OFFER_TIMEOUT => {}
            Err(refused) => {
                return Err(format!(
                    "the session did not take {number} ({}){}; nothing was sent; run \
                     `cavalry doctor` on a member's directory to see whether the cluster \
                     has a leader",
                    refused.reason(),
                    session
                        .closed()
                        .map(|why| format!(": the session is closed, {why}"))
                        .unwrap_or_default()
                ));
            }
        }
        // The duty cycle between two offers: the keepalive, and a new leader's event.
        session
            .poll(|_, _| {}, 10)
            .map_err(|fault| format!("the session's poll stopped on a fault: {fault:?}"))?;
        std::thread::sleep(IDLE);
    }
    let sent = Instant::now();
    let mut answer = None;
    while answer.is_none() {
        session
            .poll(
                |reply, header| {
                    answer = Some((
                        String::from_utf8_lossy(reply).into_owned(),
                        header.leadership_term_id,
                        header.timestamp,
                    ));
                },
                1,
            )
            .map_err(|fault| format!("the session's poll stopped on a fault: {fault:?}"))?;
        if answer.is_some() {
            break;
        }
        if let Some(why) = session.closed() {
            return Err(format!(
                "the session closed before the reply to {number} came: {why}; run the \
                 program again with 0 to read the total, which says whether {number} \
                 was added"
            ));
        }
        if sent.elapsed() > REPLY_TIMEOUT {
            return Err(format!(
                "no reply to {number} in {} s: the leader was lost before a quorum held \
                 the message, or before its service answered; run the program again \
                 with 0 to read the total, which says whether {number} was added",
                REPLY_TIMEOUT.as_secs()
            ));
        }
        std::thread::sleep(IDLE);
    }
    answer.ok_or_else(|| "no reply".to_string())
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args = parse(std::env::args().skip(1))?;
    let mut client = match &args.dir {
        Some(dir) => Cavalry::connect_to(dir)?,
        None => Cavalry::connect()?,
    };
    let channel = format!("cavalry:udp?endpoint={}", args.response);
    let mut session = client.connect_cluster(&args.members, &channel)?;
    println!(
        "adder-client: session {} is open; the leader is member {}, in term {}",
        session.session_id(),
        session.leader_member_id(),
        session.leadership_term_id()
    );
    let mut failed = None;
    for number in &args.numbers {
        match send(&mut session, *number) {
            Ok((answer, term, time)) => println!(
                "adder-client: sent {number}; the cluster answered {answer} (term {term}, \
                 cluster time {time} ms)"
            ),
            Err(error) => {
                failed = Some(error);
                break;
            }
        }
    }
    let id = session.session_id();
    session.close()?;
    println!("adder-client: session {id} is closed");
    client.close()?;
    match failed {
        Some(error) => Err(error.into()),
        None => Ok(()),
    }
}
