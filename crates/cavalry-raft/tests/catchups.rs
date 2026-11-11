//! What a leader does for a member that catches up within its term, on one core driven
//! by hand (`spec/cluster.md`, "Messages outside an election"; ADR 0087). A catch-up
//! position from the leader's term base to its own append position starts a replay of
//! its live recording, with no end. The leader stops it, and says so with stop
//! catch-up, when the member reports from the replay an append position at or past
//! the leader's own.
//!
//! The leader of every test here is member 0 of three, leading term 2 from 96 over a
//! record of terms (0, 0), (1, 64), (2, 96), with 64 bytes of its own term in its log:
//! its append position is 160.

mod common;

use cavalry_proto::cluster::{APPEND_POSITION_FLAG_CATCHUP, Message, Template};
use cavalry_raft::{Consensus, ElectionState, ReplayStopped, Role};
use common::{Call, Stub, at, canvass_position, leader_of_term_two};

fn work(member: &mut Consensus, io: &mut Stub, ms: u64) {
    member.do_work(at(ms), io).expect("no fault");
}

fn read(member: &mut Consensus, io: &mut Stub, ms: u64, message: &Message<'_>) {
    member.on_message(at(ms), message, io).expect("no fault");
}

fn catchup_position(member: i32, term: i64, position: i64) -> Message<'static> {
    Message::CatchupPosition {
        leadership_term_id: term,
        log_position: position,
        follower_member_id: member,
        catchup_endpoint: b"127.0.0.1:20104",
    }
}

/// `member`'s append position for `term`, with the catch-up flag when it was appended
/// from the leader's catch-up replay.
fn appended(member: i32, term: i64, position: i64, from_replay: bool) -> Message<'static> {
    Message::AppendPosition {
        leadership_term_id: term,
        log_position: position,
        follower_member_id: member,
        flags: if from_replay {
            APPEND_POSITION_FLAG_CATCHUP
        } else {
            0
        },
    }
}

/// The replays started, in order: the member each is to, its position and its end.
fn started(io: &Stub) -> Vec<(i32, i64, Option<i64>)> {
    io.calls
        .iter()
        .flatten()
        .filter_map(|call| match *call {
            Call::StartReplay(member, position, end) => Some((member, position, end)),
            _ => None,
        })
        .collect()
}

fn stops(io: &Stub, member: i32) -> usize {
    io.count(|call| *call == Call::StopReplay(member))
}

fn stop_catchups(io: &Stub, member: i32) -> usize {
    io.count(|call| {
        *call
            == Call::Send {
                to: member,
                template: Template::StopCatchup,
            }
    })
}

/// The leader, 64 bytes into its own term, with the calls of its election forgotten.
fn leading() -> (Consensus, Stub, u64) {
    let mut io = Stub::new();
    let (mut leader, ms) = leader_of_term_two(&mut io);
    io.append_position = 160;
    work(&mut leader, &mut io, ms);
    assert_eq!(leader.append_position(), 160);
    io.clear();
    (leader, io, ms + 1)
}

/// The position tells the two requests apart. From the leader's term base to its own
/// append position it is a catch-up: a replay with no end, counted in
/// `cluster.catchups`. Below the base it is a replication, to the end of the term the
/// position lies in, counted in `cluster.log_replications`. Above the leader's append
/// position it asks for what the leader does not hold, and starts nothing.
#[test]
fn a_position_in_the_leaders_term_starts_a_replay_with_no_end() {
    let (mut leader, mut io, ms) = leading();
    read(&mut leader, &mut io, ms, &catchup_position(2, 2, 96));
    assert_eq!(started(&io), vec![(2, 96, None)]);
    assert_eq!(leader.counts().catchups, 1);
    assert_eq!(leader.counts().log_replications, 0);

    read(&mut leader, &mut io, ms + 1, &catchup_position(1, 2, 160));
    assert_eq!(started(&io), vec![(2, 96, None), (1, 160, None)]);
    assert_eq!(leader.counts().catchups, 2);

    let (mut leader, mut io, ms) = leading();
    read(&mut leader, &mut io, ms, &catchup_position(2, 2, 192));
    read(&mut leader, &mut io, ms, &catchup_position(1, 2, 95));
    assert_eq!(started(&io), vec![(1, 95, Some(96))]);
    assert_eq!(leader.counts().catchups, 0);
    assert_eq!(leader.counts().log_replications, 1);
}

/// The member reports, from the replay, the leader's own append position: the leader
/// stops the replay, once, and sends stop catch-up. Until the member has read it, it
/// goes on reporting with the flag, and each report is answered with stop catch-up
/// again and no second stop. A report below the leader's position, or one without the
/// flag, a follower on the live log, stops nothing.
#[test]
fn a_member_that_reports_the_leaders_position_from_the_replay_is_told_to_stop() {
    let (mut leader, mut io, ms) = leading();
    read(&mut leader, &mut io, ms, &catchup_position(2, 2, 96));
    read(&mut leader, &mut io, ms + 1, &appended(2, 2, 128, true));
    read(&mut leader, &mut io, ms + 2, &appended(2, 2, 160, false));
    assert_eq!(stops(&io, 2), 0);
    assert_eq!(stop_catchups(&io, 2), 0);

    read(&mut leader, &mut io, ms + 3, &appended(2, 2, 160, true));
    assert_eq!(stops(&io, 2), 1);
    assert_eq!(stop_catchups(&io, 2), 1);
    let stop = io
        .first(|call| *call == Call::StopReplay(2))
        .expect("the stop");
    let told = io
        .first(|call| {
            *call
                == Call::Send {
                    to: 2,
                    template: Template::StopCatchup,
                }
        })
        .expect("stop catch-up");
    assert!(stop < told);

    read(&mut leader, &mut io, ms + 4, &appended(2, 2, 160, true));
    assert_eq!(stops(&io, 2), 1, "the replay was asked to stop once");
    assert_eq!(
        stop_catchups(&io, 2),
        2,
        "the member is told until it has read it"
    );

    // The replay has stopped; nothing follows it.
    io.stopped[2] = Some(ReplayStopped {
        position: 160,
        ended: false,
    });
    work(&mut leader, &mut io, ms + 5);
    assert_eq!(started(&io).len(), 1);
    assert_eq!(leader.role(), Role::Leader);
}

/// A report with the flag for another term than the leader's stops nothing: it is of
/// a log the leader does not count.
#[test]
fn a_report_from_a_replay_for_another_term_stops_nothing() {
    let (mut leader, mut io, ms) = leading();
    read(&mut leader, &mut io, ms, &catchup_position(2, 2, 96));
    read(&mut leader, &mut io, ms + 1, &appended(2, 1, 160, true));
    assert_eq!(stops(&io, 2), 0);
    assert_eq!(stop_catchups(&io, 2), 0);
}

/// A catch-up's replay that ended, however it ended, is followed by nothing: it has
/// no recording after it to go on with.
#[test]
fn a_catch_up_replay_that_ended_is_followed_by_nothing() {
    let (mut leader, mut io, ms) = leading();
    read(&mut leader, &mut io, ms, &catchup_position(2, 2, 96));
    io.stopped[2] = Some(ReplayStopped {
        position: 128,
        ended: true,
    });
    work(&mut leader, &mut io, ms + 1);
    read(&mut leader, &mut io, ms + 2, &appended(2, 2, 128, true));
    work(&mut leader, &mut io, ms + 3);
    assert_eq!(started(&io), vec![(2, 96, None)]);
    // The member asks again, and is served at once.
    read(&mut leader, &mut io, ms + 4, &catchup_position(2, 2, 128));
    assert_eq!(started(&io), vec![(2, 96, None), (2, 128, None)]);
    assert_eq!(leader.counts().catchups, 2);
    assert_eq!(stops(&io, 2), 0);
}

/// A member that canvasses, or asks again, while its catch-up runs is served as one
/// that replicates is: the replay is stopped, and a new request, the newest, is
/// started when it has stopped, whichever of the two kinds it is.
#[test]
fn a_catch_up_is_stopped_by_a_canvass_position_and_replaced_by_a_new_request() {
    let (mut leader, mut io, ms) = leading();
    read(&mut leader, &mut io, ms, &catchup_position(2, 2, 96));
    read(&mut leader, &mut io, ms + 1, &canvass_position(2, 2, 128));
    assert_eq!(stops(&io, 2), 1);
    io.stopped[2] = Some(ReplayStopped {
        position: 128,
        ended: false,
    });
    work(&mut leader, &mut io, ms + 2);
    assert_eq!(
        started(&io).len(),
        1,
        "no replay follows a canvass position"
    );

    read(&mut leader, &mut io, ms + 3, &catchup_position(1, 2, 96));
    read(&mut leader, &mut io, ms + 4, &catchup_position(1, 2, 32));
    read(&mut leader, &mut io, ms + 5, &catchup_position(1, 2, 128));
    assert_eq!(stops(&io, 1), 1);
    io.stopped[1] = Some(ReplayStopped {
        position: 112,
        ended: false,
    });
    work(&mut leader, &mut io, ms + 6);
    assert_eq!(
        started(&io),
        vec![(2, 96, None), (1, 96, None), (1, 128, None)]
    );
    assert_eq!(leader.counts().catchups, 3);
    assert_eq!(leader.counts().log_replications, 0);
}

/// A leader that holds an election stops a catch-up's replay in init, as it stops a
/// replication's, and sends no stop catch-up as a follower.
#[test]
fn a_leader_that_holds_an_election_stops_the_catch_up_it_served() {
    let (mut leader, mut io, ms) = leading();
    read(&mut leader, &mut io, ms, &catchup_position(2, 2, 96));
    work(&mut leader, &mut io, ms + 10_001);
    assert_eq!(leader.election_state(), ElectionState::Init);
    work(&mut leader, &mut io, ms + 10_002);
    assert_eq!(leader.election_state(), ElectionState::Canvass);
    assert_eq!(stops(&io, 2), 1);
    read(
        &mut leader,
        &mut io,
        ms + 10_003,
        &appended(2, 2, 160, true),
    );
    assert_eq!(stop_catchups(&io, 2), 0);
    assert_eq!(stops(&io, 2), 1);
}

/// A stop that fails is a fault, returned from the report that asked for it; the
/// replay still counts as running, and the member's next report stops it again.
#[test]
fn a_stop_of_a_catch_up_that_fails_is_returned_and_asked_again() {
    let (mut leader, mut io, ms) = leading();
    read(&mut leader, &mut io, ms, &catchup_position(2, 2, 96));
    io.fail_stop = true;
    assert_eq!(
        leader.on_message(at(ms + 1), &appended(2, 2, 160, true), &mut io),
        Err("the replay cannot be stopped")
    );
    assert_eq!(
        stop_catchups(&io, 2),
        0,
        "not told before the replay is stopped"
    );
    io.fail_stop = false;
    read(&mut leader, &mut io, ms + 201, &appended(2, 2, 160, true));
    assert_eq!(stops(&io, 2), 2);
    assert_eq!(stop_catchups(&io, 2), 1);
}

/// The appointed leader serves no catch-up and sends no stop catch-up.
#[test]
fn an_appointed_leader_serves_no_catch_up() {
    let mut io = Stub::new();
    let mut leader = Consensus::new(&[0, 1, 2], 0).expect("a member");
    work(&mut leader, &mut io, 1);
    for other in [1, 2] {
        read(&mut leader, &mut io, 2, &canvass_position(other, -1, 0));
    }
    work(&mut leader, &mut io, 3);
    work(&mut leader, &mut io, 4);
    assert_eq!(leader.election_state(), ElectionState::LeaderLogReplication);
    io.clear();
    read(&mut leader, &mut io, 5, &catchup_position(2, 0, 0));
    read(&mut leader, &mut io, 6, &appended(2, 0, 0, true));
    assert_eq!(started(&io), vec![]);
    assert_eq!(stop_catchups(&io, 2), 0);
    assert_eq!(leader.counts().catchups, 0);
}
