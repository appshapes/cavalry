//! The allocation gate over a cluster's service (CLAUDE.md, invariant 2 and gate 2;
//! ADR 0118): once the service has opened its member's log and holds its session's
//! publication, an entry read, handed over and answered allocates nothing and
//! reallocates nothing. The member is played by this thread, as
//! `tests/cluster_service.rs` plays one, and so is the session's client, which reads
//! every reply; a region counts every thread of the process, the driver's too. Alone
//! in its binary, so that no other test's threads are counted.

use std::alloc::System;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use cavalry_client::{Action, Cavalry, ClusterService, Entry, Publication, Reply, Subscription};
use cavalry_driver::Driver;
use cavalry_proto::cluster::{
    EGRESS_STREAM_ID, LOG_RECORDINGS_FILE_PREFIX, LogRecording, MAX_LOG_RECORDINGS_LENGTH, Message,
    TimeUnit, write_log_recordings,
};
use cavalry_shm::counters::generated::{
    CLUSTER_APPEND_POSITION, CLUSTER_COMMIT_POSITION, CLUSTER_LEADER_MEMBER_ID,
    CLUSTER_LEADERSHIP_TERM_ID, CLUSTER_LOG_RECORDING_ID,
};
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};

// This test's ports, from the one file that says how they are taken and why, which is
// `cavalry-raft`'s: `crates/cavalry-raft/tests/ports/mod.rs`.
#[path = "../../cavalry-raft/tests/ports/mod.rs"]
mod ports;

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

/// The entries counted: `ROUNDS` times `BATCH` session messages, each answered. The
/// member commits a batch at a time, as a leader's commit position covers several
/// entries at once, and a batch waits for the recorder's sync, which comes once a
/// second. So the rounds are few: the test must end inside 60 s, because the test
/// harness allocates on the main thread for the line it prints when a test has run
/// that long, and a region counts it (ADR 0118, Evidence).
const ROUNDS: u64 = 8;
const BATCH: u64 = 600;
/// The rounds before the count begins.
const WARM_UP: u64 = 3;
const DEADLINE: Duration = Duration::from_secs(30);
const MEMBER: i32 = 3;
const SESSION: i64 = 11;

/// Runs `step` until it says it is done, for the deadline at most. Nothing here
/// allocates.
fn until(what: &str, mut step: impl FnMut() -> bool) {
    let deadline = Instant::now() + DEADLINE;
    while !step() {
        assert!(Instant::now() < deadline, "{what} for {DEADLINE:?}");
        std::thread::sleep(Duration::from_micros(50));
    }
}

/// Appends `message` to the member's log from a buffer on the stack and returns the
/// position after it.
fn append(log: &mut Publication, message: &Message<'_>) -> i64 {
    let mut bytes = [0u8; 512];
    let length = message.write(&mut bytes).expect("fits");
    let mut after = 0;
    until("the log refused an entry", || {
        match log.offer(&bytes[..length]) {
            Ok(position) => {
                after = position;
                true
            }
            Err(_) => false,
        }
    });
    after
}

/// What the service and its session's client did.
#[derive(Default)]
struct Done {
    /// Session messages the service's handler took.
    taken: u64,
    /// Replies sent, and replies to try again.
    sent: u64,
    retries: u64,
    /// Replies the session's client read, and the sum of the numbers in them.
    read: u64,
    sum: u64,
}

/// The service's duty cycle and the client's: the service answers every session
/// message with its payload, and the client reads what it is sent.
fn work(
    service: &mut ClusterService,
    client: &mut Cavalry,
    egress: &mut Subscription,
    done: &mut Done,
) {
    service
        .poll(
            client,
            |committed, replies| {
                if let Entry::SessionMessage {
                    session_id,
                    payload,
                } = committed.entry
                {
                    match replies.reply(session_id, payload) {
                        Reply::Sent => done.sent += 1,
                        Reply::Retry => {
                            done.retries += 1;
                            return Action::Abort;
                        }
                        other => panic!("the reply was not sent: {other:?}"),
                    }
                    done.taken += 1;
                }
                Action::Continue
            },
            64,
        )
        .expect("the service polls");
    egress
        .poll(
            |body, _| {
                // A session message's payload follows its header of 32 bytes.
                let number = body[32..40].try_into().expect("eight bytes");
                done.sum += u64::from_le_bytes(number);
                done.read += 1;
            },
            64,
        )
        .expect("the response channel polls");
}

#[test]
fn a_service_allocates_nothing_per_entry_or_reply() {
    let dir: PathBuf = std::env::temp_dir().join(format!(
        "cavalry-client-cluster-service-allocations-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let driver = Driver::start(&dir, dir.join("log")).expect("the driver starts");
    let channel = format!("cavalry:udp?endpoint=127.0.0.1:{}", ports::free_port());

    // The played member: its counters in a consensus module's order, its log and the
    // log's recording, named as its live one.
    let mut member = Cavalry::connect_to(&dir).expect("the member's client connects");
    let key = MEMBER.to_le_bytes();
    let mut add = |id: i32, name: &str, value: i64| {
        let counter = member
            .add_counter(id, &key, &format!("{name} {MEMBER}"))
            .expect("the counter");
        counter.store_release(value).expect("stores");
        counter
    };
    let _term = add(
        CLUSTER_LEADERSHIP_TERM_ID.0,
        "cluster.leadership_term_id",
        0,
    );
    let _leader = add(
        CLUSTER_LEADER_MEMBER_ID.0,
        "cluster.leader_member_id",
        i64::from(MEMBER),
    );
    let append_position = add(CLUSTER_APPEND_POSITION.0, "cluster.append_position", 0);
    let log_recording_id = add(CLUSTER_LOG_RECORDING_ID.0, "cluster.log_recording_id", -1);
    let commit_position = add(CLUSTER_COMMIT_POSITION.0, "cluster.commit_position", 0);
    let mut log = member
        .add_exclusive_publication("cavalry:ipc", 100)
        .expect("the log publication");
    let recording = member
        .start_recording("cavalry:ipc", 100)
        .expect("the log's recording");
    let mut file = vec![0; MAX_LOG_RECORDINGS_LENGTH];
    let length = write_log_recordings(
        &mut file,
        MEMBER,
        recording.recording_id(),
        0,
        std::iter::empty::<LogRecording>(),
    )
    .expect("the file is written");
    std::fs::write(
        dir.join(format!("{LOG_RECORDINGS_FILE_PREFIX}{MEMBER}")),
        &file[..length],
    )
    .expect("the file is stored");
    log_recording_id
        .store_release(recording.recording_id())
        .expect("stores");

    // The session's client, and the service, which opens the member's empty log.
    let mut session = Cavalry::connect_to(&dir).expect("the session's client connects");
    let mut egress = session
        .add_subscription(&channel, EGRESS_STREAM_ID)
        .expect("the response channel");
    let mut client = Cavalry::connect_to(&dir).expect("the service's client connects");
    let mut service = client.cluster_service();
    let mut done = Done::default();
    work(&mut service, &mut client, &mut egress, &mut done);

    // The term's first entry and the session's open event.
    let log_session_id = log.session_id();
    let mut end = append(
        &mut log,
        &Message::NewLeadershipTermEvent {
            leadership_term_id: 0,
            log_position: 0,
            timestamp: 1_700_000_000_000,
            term_base_log_position: 0,
            leader_member_id: MEMBER,
            log_session_id,
            time_unit: TimeUnit::Millis,
            app_version: 0,
        },
    );
    end = end.max(append(
        &mut log,
        &Message::SessionOpenEvent {
            leadership_term_id: 0,
            correlation_id: 1,
            cluster_session_id: SESSION,
            timestamp: 1_700_000_000_001,
            response_stream_id: EGRESS_STREAM_ID,
            response_channel: channel.as_bytes(),
            encoded_principal: &[],
        },
    ));

    // One round: a batch of session messages appended, recorded and committed, then
    // read by the service, answered, and the answers read by the client.
    let mut number = 0u64;
    let mut sum = 0u64;
    let mut round = |service: &mut ClusterService,
                     client: &mut Cavalry,
                     egress: &mut Subscription,
                     done: &mut Done| {
        for _ in 0..BATCH {
            number += 1;
            sum += number;
            end = append(
                &mut log,
                &Message::SessionMessage {
                    leadership_term_id: 0,
                    cluster_session_id: SESSION,
                    timestamp: 1_700_000_000_002,
                    payload: &number.to_le_bytes(),
                },
            );
        }
        until("the recording did not sync", || {
            recording.synced_position() == end
        });
        append_position.store_release(end).expect("stores");
        commit_position.store_release(end).expect("stores");
        until("the service did not answer the batch", || {
            work(service, client, egress, done);
            service.position() == end && done.read == number
        });
        (number, sum)
    };

    // Warm-up: the session's publication connects, the replay and the two images are
    // mapped and touched.
    for _ in 0..WARM_UP {
        round(&mut service, &mut client, &mut egress, &mut done);
    }

    let region = Region::new(GLOBAL);
    let before = done.taken;
    let mut totals = (0, 0);
    for _ in 0..ROUNDS {
        totals = round(&mut service, &mut client, &mut egress, &mut done);
    }
    let change = region.change();
    assert_eq!(
        change.allocations + change.reallocations,
        0,
        "a service allocated or reallocated over {} entries and their replies: {change:?}; \
         see docs/how-to/run-the-bench.md, \"When it does not work\"",
        ROUNDS * BATCH
    );
    assert_eq!(done.taken - before, ROUNDS * BATCH, "every entry was taken");
    assert_eq!(
        (done.taken, done.sent, done.read, done.sum),
        (totals.0, totals.0, totals.0, totals.1),
        "every entry was answered once, and the client read every answer"
    );
    assert!(service.answers());
    println!(
        "{} entries answered, {} replies tried again",
        ROUNDS * BATCH,
        done.retries
    );

    service.close(&mut client).expect("the service closes");
    client.close().expect("the service's client closes");
    egress.close().expect("the subscription closes");
    session.close().expect("the session's client closes");
    drop(log);
    member.close().expect("the member's client closes");
    driver.close().expect("the driver closes");
    let _ = std::fs::remove_dir_all(&dir);
}
