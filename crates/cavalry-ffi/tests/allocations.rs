//! The allocation gate over the ABI's message path (CLAUDE.md, invariant 2 and gate
//! 2; ADR 0051): every per-message function, `cavalry_publication_try_claim`,
//! `cavalry_buffer_claim_set_reserved_value`, `cavalry_buffer_claim_commit`,
//! `cavalry_buffer_claim_abort`, `cavalry_publication_offer`,
//! `cavalry_subscription_poll`, `cavalry_header_values`,
//! `cavalry_committed_log_poll`, `cavalry_committed_log_entry_position`,
//! `cavalry_cluster_session_offer`, `cavalry_cluster_session_poll` and
//! `cavalry_cluster_reply_values`, counted with `stats_alloc`, allocations and
//! reallocations both, over a window after the image has joined, with none at all.
//! The committed log's two have a window of their own, beside three cluster members
//! in this process whose duty cycles run inside it (ADR 0070). The cluster session's
//! three have a third, against a leader this thread plays (ADR 0122). Alone in its
//! binary, so that no other test's threads are counted.

use std::alloc::System;
use std::ffi::{CString, c_void};
use std::path::Path;
use std::ptr;
use std::time::{Duration, Instant};

use cavalry_client::{Cavalry, Offer, Publication, Subscription};
use cavalry_driver::Driver;
use cavalry_ffi::*;
use cavalry_proto::cluster::{
    EGRESS_STREAM_ID, EventCode, INGRESS_STREAM_ID, LOG_RECORDINGS_FILE_PREFIX, LogRecordings,
    Message, Stream,
};
use cavalry_raft::{ConsensusModule, ElectionState, Role};
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

static mut SEEN: usize = 0;

unsafe extern "C" fn count(
    _clientd: *mut c_void,
    _buffer: *const u8,
    _length: usize,
    header: *const cavalry_header_t,
) {
    // SAFETY: the header is valid for the call; the counter is this test's, on one
    // thread.
    unsafe {
        let mut values = std::mem::zeroed::<cavalry_header_values_t>();
        assert_eq!(cavalry_header_values(header, &mut values), 0);
        SEEN += 1;
    }
}

static mut ENTRIES: usize = 0;

unsafe extern "C" fn count_entry(
    _clientd: *mut c_void,
    _buffer: *const u8,
    _length: usize,
    header: *const cavalry_header_t,
) {
    // SAFETY: the header is valid for the call; the counter is this test's, on one
    // thread.
    unsafe {
        let mut values = std::mem::zeroed::<cavalry_header_values_t>();
        assert_eq!(cavalry_header_values(header, &mut values), 0);
        assert!(cavalry_committed_log_entry_position(header) >= 0);
        ENTRIES += 1;
    }
}

static mut REPLIES: usize = 0;

unsafe extern "C" fn count_reply(
    _clientd: *mut c_void,
    _buffer: *const u8,
    length: usize,
    header: *const cavalry_header_t,
) {
    // SAFETY: the header is valid for the call; the counter is this test's, on one
    // thread.
    unsafe {
        let mut values = std::mem::zeroed::<cavalry_cluster_reply_values_t>();
        assert_eq!(cavalry_cluster_reply_values(header, &mut values), 0);
        assert_eq!((length, values.leadership_term_id), (8, SESSION_TERM));
        REPLIES += 1;
    }
}

/// The id and the term the played leader gives the session of the third window.
const SESSION_ID: i64 = 11;
const SESSION_TERM: i64 = 1;

/// Sends `message` on the played leader's egress publication, from a buffer on the
/// stack, until it is taken. Nothing here allocates.
fn send(egress: &mut Publication, message: &Message<'_>) {
    let mut bytes = [0u8; 256];
    let length = message.write(&mut bytes).expect("fits");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match egress.offer(&bytes[..length]) {
            Ok(_) => return,
            Err(Offer::NotConnected | Offer::BackPressured | Offer::AdminAction) => {
                assert!(Instant::now() < deadline, "the played leader's offer");
                std::thread::sleep(Duration::from_micros(50));
            }
            Err(other) => panic!("{other}"),
        }
    }
}

/// The played leader's duty cycle: a session message it reads is answered with the
/// same payload, the session's term and a time. Nothing here allocates.
fn serve(ingress: &mut Subscription, egress: &mut Publication) {
    let mut reply = [0u8; 8];
    let mut read = false;
    ingress
        .poll(
            |body, _| {
                if let Ok(Message::SessionMessage { payload, .. }) =
                    Message::parse(Stream::Ingress, body)
                    && payload.len() == reply.len()
                {
                    reply.copy_from_slice(payload);
                    read = true;
                }
            },
            1,
        )
        .expect("the ingress polls");
    if read {
        send(
            egress,
            &Message::SessionMessage {
                leadership_term_id: SESSION_TERM,
                cluster_session_id: SESSION_ID,
                timestamp: 1_700_000_000_000,
                payload: &reply,
            },
        );
    }
}

// A cluster's ports, from the one file that says how they are taken and why, which
// is `cavalry-raft`'s: `crates/cavalry-raft/tests/ports/mod.rs`.
#[path = "../../cavalry-raft/tests/ports/mod.rs"]
mod ports;
use ports::free_ports;

/// Three cluster members over three drivers on the loopback, their elections closed,
/// the member that leads first and its followers after it, whichever member the
/// cluster has for a leader (ADR 0100, change 3). They start in the order 1, 2, 0,
/// so that no member's place is its id, before the leader is put first or after. The
/// follower in place 1 joined the leader's log on time, and its log is one recording.
fn cluster(dir: &Path) -> Vec<(Driver, ConsensusModule)> {
    let ports = free_ports(15);
    let members_string = (0..3)
        .map(|member| {
            let endpoints: Vec<String> = ports[member * 5..member * 5 + 5]
                .iter()
                .map(|port| format!("127.0.0.1:{port}"))
                .collect();
            format!("{member},{}", endpoints.join(","))
        })
        .collect::<Vec<_>>()
        .join("|");
    let mut members: Vec<(Driver, ConsensusModule)> = [1, 2, 0]
        .into_iter()
        .map(|id| {
            let driver_dir = dir.join(format!("member-{id}"));
            let log_dir = dir.join(format!("member-{id}-log"));
            let driver = Driver::start(&driver_dir, &log_dir).expect("the member's driver");
            let module = ConsensusModule::start(&driver_dir, &log_dir, &members_string, id)
                .expect("the member");
            (driver, module)
        })
        .collect();
    let deadline = Instant::now() + Duration::from_secs(60);
    while !members
        .iter()
        .all(|(_, module)| module.election_state() == ElectionState::Closed)
    {
        assert!(Instant::now() < deadline, "the elections closed");
        work(&mut members);
    }
    let leader = members
        .iter()
        .position(|(_, module)| module.role() == Role::Leader)
        .expect("one member leads");
    members.swap(0, leader);
    // The service reads beside `members[1]` from 0 through one handle, so that is a
    // follower whose log is one recording, from 0. A follower that joined the leader's
    // log late holds what lies below its live image in its recording of the replay it
    // caught up by (ADR 0103; ADR 0104), and a handle opened at 0 beside it replays
    // that recording and ends where it stops, after the term's first entry. The leader
    // closes its election on a quorum of members that joined its log at its position,
    // so one follower at least did. Each member writes its file before it names its
    // recording.
    while !members
        .iter()
        .all(|(_, module)| module.log_recording_id().is_some())
    {
        assert!(
            Instant::now() < deadline,
            "every member named its recording"
        );
        work(&mut members);
    }
    let on_time = (1..3)
        .find(|&place| {
            let member_id = members[place].1.member_id();
            let bytes = std::fs::read(
                dir.join(format!("member-{member_id}"))
                    .join(format!("{LOG_RECORDINGS_FILE_PREFIX}{member_id}")),
            )
            .expect("the member's file of its log's recordings");
            let file = LogRecordings::read(&bytes, member_id).expect("the file passes its checks");
            file.is_empty() && file.live_start_position() == 0
        })
        .expect("a follower that joined the log on time");
    members.swap(1, on_time);
    members
}

fn work(members: &mut [(Driver, ConsensusModule)]) {
    for (_, module) in members.iter_mut() {
        module.do_work().expect("the member's duty cycle");
    }
}

#[test]
fn the_message_path_allocates_nothing() {
    let dir = std::env::temp_dir().join(format!("cavalry-ffi-allocations-{}", std::process::id()));
    let log_dir = dir.with_extension("log");
    let cluster_dir = dir.with_extension("cluster");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&log_dir);
    let _ = std::fs::remove_dir_all(&cluster_dir);
    // The cluster starts before either window, so that neither holds a start.
    let mut members = cluster(&cluster_dir);
    // The service reads beside a follower, `members[1]`, in that member's directory.
    let follower = format!("member-{}", members[1].1.member_id());
    let member_dir = CString::new(cluster_dir.join(follower).to_str().unwrap()).unwrap();
    let driver = Driver::start(&dir, &log_dir).expect("the driver");
    let dir_c = CString::new(dir.to_str().unwrap()).unwrap();
    let channel = c"cavalry:ipc";
    // SAFETY: the ABI's conventions, held by this test.
    unsafe {
        let mut context = ptr::null_mut();
        assert_eq!(cavalry_context_init_v(&mut context, CAVALRY_ABI_VERSION), 0);
        assert_eq!(cavalry_context_set_dir(context, dir_c.as_ptr()), 0);
        let mut client = ptr::null_mut();
        assert_eq!(cavalry_init(&mut client, context), 0);
        let mut subscription = ptr::null_mut();
        assert_eq!(
            cavalry_add_subscription(&mut subscription, client, channel.as_ptr(), 1001),
            0
        );
        let mut publication = ptr::null_mut();
        assert_eq!(
            cavalry_add_publication(&mut publication, client, channel.as_ptr(), 1001),
            0
        );
        let mut claim = std::mem::zeroed::<cavalry_buffer_claim_t>();
        // Warm up: the image joins, the first frames cross, and the subscription has
        // applied its events, so the window below holds the message path alone.
        let mut warmed = 0;
        while warmed < 100 {
            let position = cavalry_publication_try_claim(publication, 32, &mut claim);
            if position >= 0 {
                assert_eq!(cavalry_buffer_claim_commit(&mut claim), position);
                warmed += 1;
            }
            assert!(cavalry_subscription_poll(subscription, Some(count), ptr::null_mut(), 16) >= 0);
        }
        while SEEN < 100 {
            assert!(cavalry_subscription_poll(subscription, Some(count), ptr::null_mut(), 16) >= 0);
        }
        let before = SEEN;
        let region = Region::new(GLOBAL);
        for i in 0..10_000u32 {
            let position = cavalry_publication_try_claim(publication, 32, &mut claim);
            if position < 0 {
                assert_ne!(position, CAVALRY_PUBLICATION_ERROR);
                // Back pressure: drain and try again, inside the window.
                assert!(
                    cavalry_subscription_poll(subscription, Some(count), ptr::null_mut(), 64) >= 0
                );
                continue;
            }
            assert_eq!(
                cavalry_buffer_claim_set_reserved_value(&mut claim, i64::from(i)),
                0
            );
            if i % 7 == 0 {
                assert_eq!(cavalry_buffer_claim_abort(&mut claim), position);
            } else {
                assert_eq!(cavalry_buffer_claim_commit(&mut claim), position);
            }
            let message = [i as u8; 32];
            let offered = cavalry_publication_offer(publication, message.as_ptr(), message.len());
            if offered < 0 {
                assert_ne!(offered, CAVALRY_PUBLICATION_ERROR);
            }
            assert!(cavalry_subscription_poll(subscription, Some(count), ptr::null_mut(), 8) >= 0);
        }
        while cavalry_subscription_poll(subscription, Some(count), ptr::null_mut(), 64) > 0 {}
        let change = region.change();
        let delivered = SEEN - before;
        assert_eq!(cavalry_publication_close(publication), 0);
        assert_eq!(cavalry_subscription_close(subscription), 0);
        assert_eq!(cavalry_close(client), 0);
        assert_eq!(cavalry_context_close(context), 0);
        assert!(
            delivered > 10_000,
            "the window delivered {delivered} fragments"
        );
        assert_eq!(
            change.allocations + change.reallocations,
            0,
            "the message path allocated across {delivered} fragments, {change:?}; see \
             .claude/rules/data-plane.md"
        );

        // The committed log's window: a service through the ABI beside a follower.
        // The leader appends an entry, the members run, and the C handler
        // is handed what a quorum committed, with its header's values and its
        // position, two thousand times.
        let mut context = ptr::null_mut();
        assert_eq!(cavalry_context_init_v(&mut context, CAVALRY_ABI_VERSION), 0);
        assert_eq!(cavalry_context_set_dir(context, member_dir.as_ptr()), 0);
        let mut client = ptr::null_mut();
        assert_eq!(cavalry_init(&mut client, context), 0);
        // A follower's recording starts with the leader's first frame, which can be
        // after its election closes.
        let deadline = Instant::now() + Duration::from_secs(60);
        while members[1].1.log_recording_id().is_none() {
            assert!(Instant::now() < deadline, "the follower recorded the log");
            work(&mut members);
        }
        let mut log = ptr::null_mut();
        assert_eq!(cavalry_committed_log(&mut log, client, 0), 0);
        assert!(!log.is_null(), "the follower has a recording of the log");
        let payload = [9u8; 8];
        let mut offered = 0;
        // Warm up: the replay's image joins and the first entries cross.
        let deadline = Instant::now() + Duration::from_secs(60);
        while ENTRIES < 100 {
            assert!(
                Instant::now() < deadline,
                "the service read its first entries"
            );
            if offered < 100 && members[0].1.offer(&payload).is_ok() {
                offered += 1;
            }
            work(&mut members);
            assert!(cavalry_committed_log_poll(log, Some(count_entry), ptr::null_mut(), 16) >= 0);
        }
        let before = ENTRIES;
        let region = Region::new(GLOBAL);
        let mut offered = 0;
        let deadline = Instant::now() + Duration::from_secs(120);
        while ENTRIES - before < 2_000 {
            assert!(
                Instant::now() < deadline,
                "the service read two thousand entries"
            );
            if offered < 2_000 && members[0].1.offer(&payload).is_ok() {
                offered += 1;
            }
            work(&mut members);
            assert!(cavalry_committed_log_poll(log, Some(count_entry), ptr::null_mut(), 16) >= 0);
        }
        let change = region.change();
        let read = ENTRIES - before;
        assert_eq!(cavalry_committed_log_close(log, client), 0);
        assert_eq!(cavalry_close(client), 0);
        assert_eq!(cavalry_context_close(context), 0);
        assert_eq!(
            change.allocations + change.reallocations,
            0,
            "the committed log's path allocated across {read} entries, {change:?}; see \
             .claude/rules/data-plane.md"
        );
    }

    // The cluster session's window (ADR 0122): a session through the ABI on the first
    // driver, against a leader this thread plays with a subscription on its ingress
    // endpoint and a publication to the session's response channel. A message is
    // offered, the played leader answers it, and the C handler is handed the reply
    // and reads its term, two thousand times; a stale reply is passed over and
    // counted after every five hundred.
    let ports = free_ports(3);
    let session_members = CString::new(
        (0..2)
            .map(|id| {
                format!(
                    "{id},127.0.0.1:{},127.0.0.1:1,127.0.0.1:2,127.0.0.1:3,127.0.0.1:4",
                    ports[id]
                )
            })
            .collect::<Vec<_>>()
            .join("|"),
    )
    .unwrap();
    let response_channel = format!("cavalry:udp?endpoint=127.0.0.1:{}", ports[2]);
    let response_channel_c = CString::new(response_channel.clone()).unwrap();
    let mut played = Cavalry::connect_to(&dir).expect("the played leader connects");
    let mut ingress = played
        .add_subscription(
            &format!("cavalry:udp?endpoint=127.0.0.1:{}", ports[0]),
            INGRESS_STREAM_ID,
        )
        .expect("the ingress subscription");
    let mut egress = played
        .add_exclusive_publication(&response_channel, EGRESS_STREAM_ID)
        .expect("the egress publication");
    // The connect blocks until the leader answers, which this thread does, so a
    // thread of its own makes it and has ended before anything is counted. The
    // handles cross back as addresses: a handle may move between threads between
    // calls (`spec/c-abi.md`, "Threads").
    let opening = {
        let (dir_c, members, channel) = (
            dir_c.clone(),
            session_members.clone(),
            response_channel_c.clone(),
        );
        std::thread::spawn(move || {
            // SAFETY: the ABI's conventions, held by this test.
            unsafe {
                let mut context = ptr::null_mut();
                assert_eq!(cavalry_context_init_v(&mut context, CAVALRY_ABI_VERSION), 0);
                assert_eq!(cavalry_context_set_dir(context, dir_c.as_ptr()), 0);
                let mut client = ptr::null_mut();
                assert_eq!(cavalry_init(&mut client, context), 0);
                let mut session = ptr::null_mut();
                assert_eq!(
                    cavalry_connect_cluster(
                        &mut session,
                        client,
                        members.as_ptr(),
                        channel.as_ptr()
                    ),
                    0
                );
                (context as usize, client as usize, session as usize)
            }
        })
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut correlation = None;
    while correlation.is_none() {
        assert!(Instant::now() < deadline, "no session connect arrived");
        ingress
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
            cluster_session_id: SESSION_ID,
            correlation_id: correlation.expect("read"),
            leadership_term_id: SESSION_TERM,
            leader_member_id: 0,
            code: EventCode::Ok,
            version: 0,
            leader_heartbeat_timeout_ns: 10_000_000_000,
            detail: b"",
        },
    );
    let (context, client, session) = opening.join().expect("the opening thread ended");
    let (context, client, session) = (
        context as *mut cavalry_context_t,
        client as *mut cavalry_t,
        session as *mut cavalry_cluster_session_t,
    );
    // SAFETY: the ABI's conventions, held by this test; the three handles are the
    // opening thread's, which has ended.
    unsafe {
        // One round: a message to the leader, and its reply through the C handler.
        let round = |ingress: &mut Subscription, egress: &mut Publication, sequence: u64| {
            let message = sequence.to_le_bytes();
            let deadline = Instant::now() + Duration::from_secs(30);
            loop {
                let offered =
                    cavalry_cluster_session_offer(session, message.as_ptr(), message.len());
                if offered >= 0 {
                    break;
                }
                assert_ne!(offered, CAVALRY_PUBLICATION_ERROR);
                assert_ne!(offered, CAVALRY_PUBLICATION_CLOSED);
                assert!(Instant::now() < deadline, "the session's offer was refused");
                std::thread::sleep(Duration::from_micros(50));
            }
            let before = REPLIES;
            while REPLIES == before {
                assert!(Instant::now() < deadline, "no reply to message {sequence}");
                serve(ingress, egress);
                assert!(
                    cavalry_cluster_session_poll(session, Some(count_reply), ptr::null_mut(), 1)
                        >= 0
                );
            }
        };
        // Warm up: the images join and every first-time path is taken.
        for sequence in 0..200 {
            round(&mut ingress, &mut egress, sequence);
        }
        let stale_before = cavalry_cluster_session_stale_replies(session);
        let before = REPLIES;
        let region = Region::new(GLOBAL);
        for sequence in 0..2_000u64 {
            round(&mut ingress, &mut egress, sequence);
            if sequence % 500 == 499 {
                // A reply of the term before: stale, passed over and counted.
                send(
                    &mut egress,
                    &Message::SessionMessage {
                        leadership_term_id: SESSION_TERM - 1,
                        cluster_session_id: SESSION_ID,
                        timestamp: 1_700_000_000_000,
                        payload: b"too late",
                    },
                );
                let passed = cavalry_cluster_session_stale_replies(session);
                let deadline = Instant::now() + Duration::from_secs(30);
                while cavalry_cluster_session_stale_replies(session) == passed {
                    assert!(Instant::now() < deadline, "the stale reply was not read");
                    assert_eq!(
                        cavalry_cluster_session_poll(
                            session,
                            Some(count_reply),
                            ptr::null_mut(),
                            1
                        ),
                        0,
                        "a stale reply is not handed over"
                    );
                }
            }
        }
        let change = region.change();
        let read = REPLIES - before;
        let stale = cavalry_cluster_session_stale_replies(session) - stale_before;
        assert_eq!(cavalry_cluster_session_close(session), 0);
        assert_eq!(cavalry_close(client), 0);
        assert_eq!(cavalry_context_close(context), 0);
        assert_eq!((read, stale), (2_000, 4));
        assert_eq!(
            change.allocations + change.reallocations,
            0,
            "the cluster session's path allocated across {read} replies and {stale} stale \
             ones, {change:?}; see .claude/rules/data-plane.md"
        );
    }
    drop(egress);
    drop(ingress);
    played.close().expect("the played leader closes");
    driver.close().expect("the driver closes");
    drop(members);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&log_dir);
    let _ = std::fs::remove_dir_all(&cluster_dir);
}
