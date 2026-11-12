//! The catch-up within a leader's term, states 11 to 13, on one core driven by hand
//! (`spec/cluster.md`, "Roles and the election", rows 10 to 14; ADR 0088). A member
//! that has joined its leader's term and is behind the leader's log asks for the log
//! from its append position, records the leader's replay, adds the live log when it
//! is near, and joins when the leader says the catch-up is over.
//!
//! The member of every test here is member 0 of three. It followed term 0, led by
//! member 1 from position 0, to 64 bytes, and lost that leader. Member 2 leads term 1
//! from 64, and its log is at 160 when it tells the member so.

mod common;

use cavalry_proto::cluster::{
    APPEND_POSITION_FLAG_CATCHUP, LEADER_HEARTBEAT_TIMEOUT_NS, Message, Template,
};
use cavalry_raft::{Consensus, ElectionState};
use common::{Call, Stub, at, follow_term, lose_the_leader, new_leadership_term};

const TIMEOUT_MS: u64 = LEADER_HEARTBEAT_TIMEOUT_NS / 1_000_000;

fn work(member: &mut Consensus, io: &mut Stub, ms: u64) {
    member.do_work(at(ms), io).expect("no fault");
}

fn read(member: &mut Consensus, io: &mut Stub, ms: u64, message: &Message<'_>) {
    member.on_message(at(ms), message, io).expect("no fault");
}

/// Member 2's new leadership term for term 1, which begins at 64, to a member whose
/// log is of term 0, with the leader's log at `log_position` and its commit position
/// at `commit`.
fn term_one(log_position: i64, commit: i64) -> Message<'static> {
    Message::NewLeadershipTerm {
        log_leadership_term_id: 0,
        next_leadership_term_id: 1,
        next_term_base_log_position: 64,
        next_log_position: -1,
        leadership_term_id: 1,
        term_base_log_position: 64,
        log_position,
        leader_recording_id: 5,
        timestamp: 1_700_000_000_000,
        leader_member_id: 2,
        log_session_id: 78,
        app_version: 0,
        is_startup: false,
        commit_position: commit,
    }
}

fn stop_catchup(term: i64, member: i32) -> Message<'static> {
    Message::StopCatchup {
        leadership_term_id: term,
        follower_member_id: member,
    }
}

/// Member 0 in follower catch-up init: at the leader's term base, 64, with the
/// leader's term recorded there, told that the leader's log is at `log_position`.
/// Returns the member and the next millisecond.
fn behind(io: &mut Stub, log_position: i64, commit: i64) -> (Consensus, u64) {
    let mut member = Consensus::with_election(&[0, 1, 2], 0, 1).expect("a member");
    work(&mut member, io, 1);
    let closed = follow_term(&mut member, io, 2, &new_leadership_term(1, 0), 0);
    let ms = lose_the_leader(&mut member, io, closed, 64);
    read(&mut member, io, ms, &term_one(log_position, commit));
    assert_eq!(member.election_state(), ElectionState::FollowerReplay);
    work(&mut member, io, ms + 1);
    assert_eq!(member.election_state(), ElectionState::FollowerCatchupInit);
    io.clear();
    io.last_append = None;
    (member, ms + 2)
}

/// The same member taken on to follower catch-up, the replay's image joined at 64.
fn catching_up(io: &mut Stub, log_position: i64, commit: i64) -> (Consensus, u64) {
    let (mut member, ms) = behind(io, log_position, commit);
    work(&mut member, io, ms);
    io.catchup_image = Some(64);
    work(&mut member, io, ms + 1);
    assert_eq!(member.election_state(), ElectionState::FollowerCatchup);
    (member, ms + 2)
}

fn place(io: &Stub, call: Call) -> usize {
    io.first(|made| *made == call)
        .unwrap_or_else(|| panic!("{call:?} was not made"))
}

/// The whole of a catch-up. The member subscribes on its catch-up endpoint, then
/// asks from its append position, counted. The image joins there. It reports with
/// its leader's term, and adds the live log at once, being within the live-add window
/// of the leader's position. The replay brings it to 160, past where the live image
/// joined, 128: from there its report carries the catch-up flag. The leader says
/// stop: the catch-up subscription goes. It is then on the live log, at or past the
/// commit position it was told, and goes through follower log init, already
/// subscribed, to follower ready, where its report without the flag closes its
/// election.
#[test]
fn a_member_behind_its_leaders_log_catches_up_and_joins() {
    let mut io = Stub::new();
    let (mut member, ms) = behind(&mut io, 160, 64);
    let steps = member.counts().election_steps;
    work(&mut member, &mut io, ms);
    assert!(place(&io, Call::AddCatchup) < place(&io, Call::CatchupPosition(2, 1, 64)));
    assert_eq!(member.counts().catchups, 1);
    assert_eq!(member.election_state(), ElectionState::FollowerCatchupAwait);

    io.catchup_image = Some(64);
    work(&mut member, &mut io, ms + 1);
    assert_eq!(member.election_state(), ElectionState::FollowerCatchup);
    work(&mut member, &mut io, ms + 2);
    assert_eq!(io.last_append, Some((2, 1, 64)));
    assert_eq!(io.last_append_flags, 0, "not on the live log yet");
    assert_eq!(io.count(|call| *call == Call::JoinLog), 1, "the live add");

    // The replay brings the log to 160; the live image joined at 128.
    io.append_position = 160;
    io.image = Some(128);
    work(&mut member, &mut io, ms + 3);
    assert_eq!(io.last_append, Some((2, 1, 160)));
    assert_eq!(io.last_append_flags, APPEND_POSITION_FLAG_CATCHUP);
    assert_eq!(
        member.election_state(),
        ElectionState::FollowerCatchup,
        "until the leader says stop"
    );

    read(&mut member, &mut io, ms + 4, &stop_catchup(1, 0));
    assert_eq!(io.count(|call| *call == Call::RemoveCatchup), 1);
    work(&mut member, &mut io, ms + 5);
    assert_eq!(member.election_state(), ElectionState::FollowerLogInit);
    work(&mut member, &mut io, ms + 6);
    assert_eq!(member.election_state(), ElectionState::FollowerReady);
    assert_eq!(
        io.count(|call| *call == Call::JoinLog),
        1,
        "subscribed once"
    );
    work(&mut member, &mut io, ms + 7);
    assert_eq!(member.election_state(), ElectionState::Closed);
    assert_eq!(io.last_append, Some((2, 1, 160)));
    assert_eq!(io.last_append_flags, 0, "on the live log");
    // 11 to 12, 12 to 13, 13 to 14, 14 to 16.
    assert_eq!(member.counts().election_steps, steps + 4);
    assert_eq!(member.counts().elections_failed, 0);
    assert_eq!(member.counts().catchups, 1);
}

/// The live log is added only within the live-add window of the leader's position,
/// 1,048,576 bytes: a member 2 MiB behind records the replay alone until it is.
#[test]
fn the_live_log_is_added_within_the_live_add_window() {
    let far = 64 + 2 * 1_048_576;
    let mut io = Stub::new();
    let (mut member, ms) = catching_up(&mut io, far, 64);
    work(&mut member, &mut io, ms);
    assert_eq!(io.count(|call| *call == Call::JoinLog), 0);
    io.append_position = far - 1_048_576 - 32;
    work(&mut member, &mut io, ms + 1);
    assert_eq!(io.count(|call| *call == Call::JoinLog), 0, "32 bytes short");
    io.append_position = far - 1_048_576;
    work(&mut member, &mut io, ms + 2);
    assert_eq!(io.count(|call| *call == Call::JoinLog), 1);
}

/// A catch-up position that does not leave within the leader heartbeat timeout fails
/// the election, and counts no catch-up.
#[test]
fn a_catch_up_position_that_never_leaves_fails_the_election() {
    let mut io = Stub::new();
    let (mut member, ms) = behind(&mut io, 160, 64);
    io.refuse_send = true;
    work(&mut member, &mut io, ms);
    work(&mut member, &mut io, ms + TIMEOUT_MS - 2);
    assert_eq!(member.election_state(), ElectionState::FollowerCatchupInit);
    work(&mut member, &mut io, ms + TIMEOUT_MS);
    assert_eq!(member.election_state(), ElectionState::Init);
    assert_eq!(member.counts().catchups, 0);
    assert_eq!(member.counts().elections_failed, 1);
    io.refuse_send = false;
    work(&mut member, &mut io, ms + TIMEOUT_MS + 1);
    assert_eq!(io.count(|call| *call == Call::RemoveCatchup), 1);
}

/// The replay's image must join at the position asked. One that joins elsewhere, a
/// stale replay's, fails the election, counted; so does none for the leader heartbeat
/// timeout.
#[test]
fn an_image_that_joins_elsewhere_or_never_fails_the_election() {
    let mut io = Stub::new();
    let (mut member, ms) = behind(&mut io, 160, 64);
    work(&mut member, &mut io, ms);
    io.catchup_image = Some(32);
    work(&mut member, &mut io, ms + 1);
    assert_eq!(member.election_state(), ElectionState::Init);
    assert_eq!(member.counts().join_position_mismatches, 1);

    let mut io = Stub::new();
    let (mut member, ms) = behind(&mut io, 160, 64);
    work(&mut member, &mut io, ms);
    work(&mut member, &mut io, ms + TIMEOUT_MS - 1);
    assert_eq!(member.election_state(), ElectionState::FollowerCatchupAwait);
    work(&mut member, &mut io, ms + TIMEOUT_MS);
    assert_eq!(member.election_state(), ElectionState::Init);
    assert_eq!(member.counts().join_position_mismatches, 0);
    assert_eq!(member.counts().elections_failed, 1);
}

/// A member's report carries the catch-up flag only once it is on the live log: its
/// live image has joined and its append position has reached where it joined. The
/// flag tells the leader that it may stop its replay. A member told to stop sooner
/// would hold the log to its append position and from the live image's join
/// position, and nothing between: here the leader has appended to 192 and synced to
/// 160, its replay reads no further than 160, and the member at 160 reports without
/// the flag until the replay has brought it to 192. The report that first carries
/// the flag is sent in the cycle the member is first on the live log, whatever the
/// interval.
#[test]
fn a_report_carries_the_catch_up_flag_only_once_the_member_is_on_the_live_log() {
    let mut io = Stub::new();
    let (mut member, ms) = catching_up(&mut io, 160, 64);

    // No live image yet: no flag, at 64 and at 160.
    work(&mut member, &mut io, ms);
    assert_eq!(io.last_append, Some((2, 1, 64)));
    assert_eq!(io.last_append_flags, 0);
    io.append_position = 160;
    work(&mut member, &mut io, ms + 1);
    assert_eq!(io.last_append, Some((2, 1, 160)));
    assert_eq!(io.last_append_flags, 0);

    // The live image joined at 192, past its append position: still no flag, and a
    // stop catch-up is not the answer to anything it sent.
    io.image = Some(192);
    work(&mut member, &mut io, ms + 2);
    assert_eq!(io.last_append_flags, 0);
    read(&mut member, &mut io, ms + 3, &stop_catchup(1, 0));
    assert_eq!(io.count(|call| *call == Call::RemoveCatchup), 0);

    // The replay brings it to 192: the flag, in that cycle.
    io.clear();
    io.append_position = 192;
    work(&mut member, &mut io, ms + 4);
    assert_eq!(io.last_append, Some((2, 1, 192)));
    assert_eq!(io.last_append_flags, APPEND_POSITION_FLAG_CATCHUP);
    assert_eq!(member.election_state(), ElectionState::FollowerCatchup);

    // A member already at the join position when the image comes reports at once
    // with the flag, though its position has not moved and no interval has passed.
    let mut io = Stub::new();
    let (mut member, ms) = catching_up(&mut io, 160, 64);
    io.append_position = 160;
    work(&mut member, &mut io, ms);
    assert_eq!(io.last_append_flags, 0);
    io.clear();
    io.image = Some(160);
    work(&mut member, &mut io, ms + 1);
    assert_eq!(
        io.count(|call| matches!(
            call,
            Call::Send {
                to: 2,
                template: Template::AppendPosition
            }
        )),
        1
    );
    assert_eq!(io.last_append_flags, APPEND_POSITION_FLAG_CATCHUP);
}

/// What holds a member in the catch-up after it has the leader's log: a stop
/// catch-up read before it has reported with the flag, one that is not for its term
/// or not for it; the live image not yet there; an append position below where the
/// live image joined; an append position below the commit position it was told.
#[test]
fn a_member_leaves_the_catch_up_only_stopped_on_the_live_log_and_committed() {
    let mut io = Stub::new();
    let (mut member, ms) = catching_up(&mut io, 160, 64);
    io.append_position = 160;
    work(&mut member, &mut io, ms);

    // For its term and for it, before it has reported with the flag: the answer to
    // an earlier catch-up, which it does not take.
    read(&mut member, &mut io, ms + 1, &stop_catchup(1, 0));
    assert_eq!(io.count(|call| *call == Call::RemoveCatchup), 0);
    io.image = Some(128);
    work(&mut member, &mut io, ms + 2);
    assert_eq!(io.last_append_flags, APPEND_POSITION_FLAG_CATCHUP);
    assert_eq!(member.election_state(), ElectionState::FollowerCatchup);

    // Not for its term, and not for it.
    read(&mut member, &mut io, ms + 2, &stop_catchup(0, 0));
    read(&mut member, &mut io, ms + 2, &stop_catchup(1, 1));
    assert_eq!(io.count(|call| *call == Call::RemoveCatchup), 0);

    // Stopped, with no live image yet.
    io.image = None;
    read(&mut member, &mut io, ms + 3, &stop_catchup(1, 0));
    read(&mut member, &mut io, ms + 3, &stop_catchup(1, 0));
    assert_eq!(io.count(|call| *call == Call::RemoveCatchup), 1, "once");
    work(&mut member, &mut io, ms + 4);
    assert_eq!(member.election_state(), ElectionState::FollowerCatchup);

    // The live image joined above its append position.
    io.image = Some(192);
    work(&mut member, &mut io, ms + 5);
    assert_eq!(member.election_state(), ElectionState::FollowerCatchup);

    // On the live log, below the commit position it was told.
    io.image = Some(128);
    let told_commit = Message::CommitPosition {
        leadership_term_id: 1,
        log_position: 192,
        leader_member_id: 2,
    };
    read(&mut member, &mut io, ms + 6, &told_commit);
    work(&mut member, &mut io, ms + 7);
    assert_eq!(member.election_state(), ElectionState::FollowerCatchup);

    io.append_position = 192;
    work(&mut member, &mut io, ms + 8);
    assert_eq!(member.election_state(), ElectionState::FollowerLogInit);
}

/// A catch-up whose append position has not moved for the leader heartbeat timeout
/// fails the election; each move starts the time again. Init then leaves the live log
/// the member had added and removes its catch-up subscription.
#[test]
fn a_catch_up_that_does_not_move_for_the_heartbeat_timeout_fails() {
    let mut io = Stub::new();
    let (mut member, ms) = catching_up(&mut io, 160, 64);
    work(&mut member, &mut io, ms);
    io.append_position = 96;
    work(&mut member, &mut io, ms + 9_000);
    work(&mut member, &mut io, ms + 9_000 + TIMEOUT_MS - 1);
    assert_eq!(member.election_state(), ElectionState::FollowerCatchup);
    work(&mut member, &mut io, ms + 9_000 + TIMEOUT_MS);
    assert_eq!(member.election_state(), ElectionState::Init);
    assert_eq!(member.counts().elections_failed, 1);
    io.clear();
    work(&mut member, &mut io, ms + 9_001 + TIMEOUT_MS);
    assert_eq!(member.election_state(), ElectionState::Canvass);
    assert_eq!(io.count(|call| *call == Call::LeaveLog), 1);
    assert_eq!(io.count(|call| *call == Call::RemoveCatchup), 1);
}

/// A member whose leader is appointed has no catch-up: behind its leader's log, it
/// fails in follower replay, as before.
#[test]
fn an_appointed_member_behind_its_leaders_log_fails() {
    let mut io = Stub::new();
    let mut member = Consensus::new(&[0, 1, 2], 1).expect("a member");
    work(&mut member, &mut io, 1);
    read(&mut member, &mut io, 2, &new_leadership_term(0, 96));
    assert_eq!(member.election_state(), ElectionState::FollowerReplay);
    io.clear();
    work(&mut member, &mut io, 3);
    assert_eq!(member.election_state(), ElectionState::Init);
    assert_eq!(member.counts().elections_failed, 1);
    assert_eq!(
        io.count(|call| matches!(
            call,
            Call::AddCatchup
                | Call::CatchupPosition(..)
                | Call::Send {
                    template: Template::AppendPosition,
                    ..
                }
        )),
        0
    );
}
