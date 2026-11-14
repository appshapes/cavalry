//! The allocation gate over a cluster session (CLAUDE.md, invariant 2 and gate 2; ADR
//! 0117, ADR 0121): once the session is open, a message offered, its reply read, a
//! stale reply passed over and counted, a keepalive sent and a new leader event
//! followed allocate nothing and reallocate nothing. The
//! cluster is played by this thread, which holds the leader's ingress subscription
//! and its egress publication; the session is opened by a second thread, which ends
//! before the count begins, since a region counts every thread of the process. Alone
//! in its binary, so that no other test's threads are counted.

use std::alloc::System;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use cavalry_client::{Cavalry, ClusterSession, Offer, Publication, Subscription};
use cavalry_driver::Driver;
use cavalry_proto::cluster::{EGRESS_STREAM_ID, EventCode, INGRESS_STREAM_ID, Message, Stream};
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};

// This test's ports, from the one file that says how they are taken and why, which is
// `cavalry-raft`'s: `crates/cavalry-raft/tests/ports/mod.rs`.
#[path = "../../cavalry-raft/tests/ports/mod.rs"]
mod ports;

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

const ROUNDS: u64 = 5_000;
/// A stale reply is sent after every this many rounds.
const STALE_EVERY: u64 = 500;
const DEADLINE: Duration = Duration::from_secs(30);
const SESSION: i64 = 11;

/// Sends `message` on `publication` from a buffer on the stack, until it is taken.
fn send(publication: &mut Publication, message: &Message<'_>) {
    let mut bytes = [0u8; 256];
    let length = message.write(&mut bytes).expect("fits");
    let deadline = Instant::now() + DEADLINE;
    loop {
        match publication.offer(&bytes[..length]) {
            Ok(_) => return,
            Err(Offer::NotConnected | Offer::BackPressured | Offer::AdminAction) => {
                assert!(
                    Instant::now() < deadline,
                    "the offer was refused for {DEADLINE:?}"
                );
                std::thread::sleep(Duration::from_micros(50));
            }
            Err(other) => panic!("{other}"),
        }
    }
}

/// Sends the session a reply of the term before `term`, which is stale once the
/// session has handed over a reply of `term`, and polls until the session has counted
/// it. Nothing here allocates.
fn late(session: &mut ClusterSession, egress: &mut Publication, term: i64) {
    send(
        egress,
        &Message::SessionMessage {
            leadership_term_id: term - 1,
            cluster_session_id: SESSION,
            timestamp: 1_700_000_000_000,
            payload: b"late",
        },
    );
    let passed = session.stale_replies();
    let deadline = Instant::now() + DEADLINE;
    while session.stale_replies() == passed {
        assert!(Instant::now() < deadline, "the stale reply was not read");
        let handed = session.poll(|_, _| {}, 1).expect("the session polls");
        assert_eq!(handed, 0, "a stale reply was handed over");
    }
}

/// The played leader's duty cycle: every session message it reads is answered with
/// the same payload, and every keepalive counted. Nothing here allocates.
fn serve(ingress: &mut Subscription, egress: &mut Publication, term: i64, keepalives: &mut u64) {
    let mut reply = [0u8; 64];
    let mut length = 0;
    ingress
        .poll(
            |body, _| match Message::parse(Stream::Ingress, body) {
                Ok(Message::SessionMessage {
                    leadership_term_id,
                    payload,
                    ..
                }) => {
                    assert_eq!(leadership_term_id, term, "the session's term");
                    reply[..payload.len()].copy_from_slice(payload);
                    length = payload.len();
                }
                Ok(Message::SessionKeepalive {
                    leadership_term_id, ..
                }) => {
                    assert_eq!(leadership_term_id, term);
                    *keepalives += 1;
                }
                other => panic!("not a session's message: {other:?}"),
            },
            1,
        )
        .expect("the ingress polls");
    if length > 0 {
        send(
            egress,
            &Message::SessionMessage {
                leadership_term_id: term,
                cluster_session_id: SESSION,
                timestamp: 1_700_000_000_000,
                payload: &reply[..length],
            },
        );
    }
}

#[test]
fn an_open_session_allocates_nothing_per_message() {
    let dir: PathBuf = std::env::temp_dir().join(format!(
        "cavalry-client-cluster-session-allocations-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let driver = Driver::start(&dir, dir.join("log")).expect("the driver starts");
    let ingress_ports = ports::free_ports(2);
    let response_channel = format!("cavalry:udp?endpoint=127.0.0.1:{}", ports::free_port());
    let members = (0..2)
        .map(|id| {
            format!(
                "{id},127.0.0.1:{},127.0.0.1:1,127.0.0.1:2,127.0.0.1:3,127.0.0.1:4",
                ingress_ports[id]
            )
        })
        .collect::<Vec<_>>()
        .join("|");
    // The played members: member 0 leads term 1, member 1 leads term 2 later.
    let mut cluster = Cavalry::connect_to(&dir).expect("the played members connect");
    let mut ingress: Vec<Subscription> = ingress_ports
        .iter()
        .map(|port| {
            cluster
                .add_subscription(
                    &format!("cavalry:udp?endpoint=127.0.0.1:{port}"),
                    INGRESS_STREAM_ID,
                )
                .expect("an ingress subscription")
        })
        .collect();
    let mut egress = cluster
        .add_exclusive_publication(&response_channel, EGRESS_STREAM_ID)
        .expect("the egress publication");

    // The session is opened by a thread of its own, which the played leader answers
    // from this one; the thread has ended before anything is counted.
    let opening = {
        let (dir, members, response_channel) =
            (dir.clone(), members.clone(), response_channel.clone());
        std::thread::spawn(move || {
            let mut client = Cavalry::connect_to(&dir).expect("connects");
            let session = client
                .connect_cluster(&members, &response_channel)
                .expect("the session opens");
            (client, session)
        })
    };
    let deadline = Instant::now() + DEADLINE;
    let mut correlation = None;
    while correlation.is_none() {
        assert!(Instant::now() < deadline, "no session connect arrived");
        ingress[0]
            .poll(
                |body, _| {
                    if let Ok(Message::SessionConnect { correlation_id, .. }) =
                        Message::parse(Stream::Ingress, body)
                    {
                        correlation = Some(correlation_id);
                    }
                },
                1,
            )
            .expect("the ingress polls");
        std::thread::sleep(Duration::from_micros(200));
    }
    send(
        &mut egress,
        &Message::SessionEvent {
            cluster_session_id: SESSION,
            correlation_id: correlation.expect("read"),
            leadership_term_id: 1,
            leader_member_id: 0,
            code: EventCode::Ok,
            version: 0,
            leader_heartbeat_timeout_ns: 10_000_000_000,
            detail: b"",
        },
    );
    let (client, mut session): (Cavalry, ClusterSession) =
        opening.join().expect("the opening thread ended");
    assert_eq!(session.session_id(), SESSION);

    let mut keepalives = 0u64;
    // One round: a message to the leader, its reply read back. Nothing here allocates.
    let round = |session: &mut ClusterSession,
                 ingress: &mut Subscription,
                 egress: &mut Publication,
                 term: i64,
                 sequence: u64,
                 keepalives: &mut u64| {
        let message = sequence.to_le_bytes();
        let deadline = Instant::now() + DEADLINE;
        loop {
            match session.offer(&message) {
                Ok(_) => break,
                Err(Offer::NotConnected | Offer::BackPressured | Offer::AdminAction) => {
                    assert!(Instant::now() < deadline, "the session's offer was refused");
                    std::thread::sleep(Duration::from_micros(50));
                }
                Err(other) => panic!("{other}"),
            }
        }
        let mut replied = false;
        while !replied {
            assert!(Instant::now() < deadline, "no reply to message {sequence}");
            serve(ingress, egress, term, keepalives);
            session
                .poll(
                    |payload, _| {
                        assert_eq!(payload, message);
                        replied = true;
                    },
                    1,
                )
                .expect("the session polls");
        }
    };
    // A quiet stretch: nothing is offered until the played leader has read one more
    // keepalive, which the session sends when the keepalive interval has passed since
    // it last sent anything. Nothing here allocates.
    let quiet = |session: &mut ClusterSession,
                 ingress: &mut Subscription,
                 egress: &mut Publication,
                 term: i64,
                 keepalives: &mut u64| {
        let before = *keepalives;
        let deadline = Instant::now() + DEADLINE;
        while *keepalives == before {
            assert!(Instant::now() < deadline, "no keepalive in {DEADLINE:?}");
            serve(ingress, egress, term, keepalives);
            session.poll(|_, _| {}, 1).expect("the session polls");
        }
    };
    // Warm up: every first-time path, the first keepalive among them.
    for sequence in 0..200 {
        round(
            &mut session,
            &mut ingress[0],
            &mut egress,
            1,
            sequence,
            &mut keepalives,
        );
    }
    quiet(
        &mut session,
        &mut ingress[0],
        &mut egress,
        1,
        &mut keepalives,
    );
    for sequence in 200..400 {
        round(
            &mut session,
            &mut ingress[0],
            &mut egress,
            1,
            sequence,
            &mut keepalives,
        );
    }
    // The second member's ingress image is warmed up too: the session follows a new
    // leader event to it and back.
    for (term, leader) in [(2, 1), (3, 0)] {
        send(
            &mut egress,
            &Message::NewLeaderEvent {
                leadership_term_id: term,
                cluster_session_id: SESSION,
                leader_member_id: leader,
                ingress_endpoints: b"",
            },
        );
        let deadline = Instant::now() + DEADLINE;
        while session.leadership_term_id() != term {
            assert!(
                Instant::now() < deadline,
                "the new leader event was not read"
            );
            session.poll(|_, _| {}, 1).expect("the session polls");
        }
        for sequence in 0..50 {
            round(
                &mut session,
                &mut ingress[leader as usize],
                &mut egress,
                term,
                sequence,
                &mut keepalives,
            );
        }
    }
    // And the stale path: a reply of term 2 after the session has read replies of
    // term 3.
    late(&mut session, &mut egress, 3);

    let region = Region::new(GLOBAL);
    let before = keepalives;
    let mut term = 3;
    let mut leader = 0usize;
    for sequence in 0..ROUNDS {
        // A new leader, twice in the run: the session turns to the other member.
        if sequence == ROUNDS / 3 || sequence == 2 * ROUNDS / 3 {
            term += 1;
            leader = 1 - leader;
            send(
                &mut egress,
                &Message::NewLeaderEvent {
                    leadership_term_id: term,
                    cluster_session_id: SESSION,
                    leader_member_id: leader as i32,
                    ingress_endpoints: b"",
                },
            );
            let deadline = Instant::now() + DEADLINE;
            while session.leadership_term_id() != term {
                assert!(
                    Instant::now() < deadline,
                    "the new leader event was not read"
                );
                session.poll(|_, _| {}, 1).expect("the session polls");
            }
        }
        round(
            &mut session,
            &mut ingress[leader],
            &mut egress,
            term,
            sequence,
            &mut keepalives,
        );
        // A reply of the term before, after one of this term: stale.
        if sequence % STALE_EVERY == STALE_EVERY - 1 {
            late(&mut session, &mut egress, term);
        }
    }
    // A quiet stretch inside the region, to a keepalive.
    quiet(
        &mut session,
        &mut ingress[leader],
        &mut egress,
        term,
        &mut keepalives,
    );
    let change = region.change();
    assert_eq!(
        change.allocations + change.reallocations,
        0,
        "an open session allocated or reallocated over {ROUNDS} messages: {change:?}; see \
         docs/how-to/run-the-bench.md, \"When it does not work\""
    );
    assert!(keepalives > before, "a keepalive was sent while counting");
    assert_eq!(
        session.stale_replies(),
        1 + (ROUNDS / STALE_EVERY) as i64,
        "the stale replies passed over, one of them in the warm-up"
    );
    assert_eq!(session.leader_member_id(), leader as i32);

    session.close().expect("the session closes");
    client.close().expect("closes");
    drop(egress);
    drop(ingress);
    cluster.close().expect("the played members close");
    driver.close().expect("the driver closes");
    let _ = std::fs::remove_dir_all(&dir);
}
