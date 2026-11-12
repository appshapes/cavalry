//! What a leader does for a member that replicates a term it missed, on one core
//! driven by hand (`spec/cluster.md`, "Messages outside an election"; ADR 0085). The
//! leader answers a catch-up position below its term base log position with a replay
//! of the recording of its own that holds the position, to the first term base above
//! it or to that recording's end, one replay a member at a time. Where the span lies
//! in several recordings the member asks for each, and the leader starts none by
//! itself (ADR 0092). It stops the replay of a member that canvasses or asks again,
//! and every replay when it holds an election.
//!
//! The leader of every test here is member 0 of three, leading term 2 from 96 over a
//! record of terms (0, 0), (1, 64), (2, 96): term 0 is the log from 0 to 64, term 1
//! from 64 to 96.

mod common;

use cavalry_proto::cluster::{Message, Template, TermSpan, replay_end};
use cavalry_raft::consensus::base_above;
use cavalry_raft::{Consensus, ElectionState, ReplayStopped, Role};
use common::{Call, Stub, at, canvass_position, leader_of_term_two};

fn work(member: &mut Consensus, io: &mut Stub, ms: u64) {
    member.do_work(at(ms), io).expect("no fault");
}

fn read(member: &mut Consensus, io: &mut Stub, ms: u64, message: &Message<'_>) {
    member.on_message(at(ms), message, io).expect("no fault");
}

/// `member`'s catch-up position for the leader's `term`, asking for the log from
/// `position`.
fn catchup_position(member: i32, term: i64, position: i64) -> Message<'static> {
    Message::CatchupPosition {
        leadership_term_id: term,
        log_position: position,
        follower_member_id: member,
        catchup_endpoint: b"127.0.0.1:20104",
    }
}

/// `member`'s append position while it replicates: its log is of `term`, to
/// `position`.
fn replicated_to(member: i32, term: i64, position: i64) -> Message<'static> {
    Message::AppendPosition {
        leadership_term_id: term,
        log_position: position,
        follower_member_id: member,
        flags: 0,
    }
}

/// The replays started, in order: the member each is to, its position and its end.
fn started(io: &Stub) -> Vec<(i32, i64, i64)> {
    io.calls
        .iter()
        .flatten()
        .filter_map(|call| match *call {
            Call::StartReplay(member, position, Some(end)) => Some((member, position, end)),
            _ => None,
        })
        .collect()
}

fn stops(io: &Stub, member: i32) -> usize {
    io.count(|call| *call == Call::StopReplay(member))
}

/// A leader ready to serve, with the calls of its own election forgotten.
fn leader() -> (Consensus, Stub, u64) {
    let mut io = Stub::new();
    let (leader, ms) = leader_of_term_two(&mut io);
    io.clear();
    (leader, io, ms)
}

/// A position inside a term is replayed to that term's end, and one at a term's base
/// to the end of the term that begins there: a replication is one term at a time. Each
/// replay started is counted, and two members are served at once.
#[test]
fn a_leader_replays_from_the_position_asked_to_the_end_of_its_term() {
    let (mut leader, mut io, ms) = leader();
    read(&mut leader, &mut io, ms, &catchup_position(2, 2, 32));
    assert_eq!(started(&io), vec![(2, 32, 64)]);
    assert_eq!(leader.counts().log_replications, 1);

    read(&mut leader, &mut io, ms + 1, &catchup_position(1, 2, 64));
    assert_eq!(started(&io), vec![(2, 32, 64), (1, 64, 96)]);
    assert_eq!(leader.counts().log_replications, 2);
    // It leads on, in the state it was in.
    assert_eq!(leader.role(), Role::Leader);
    assert_eq!(leader.election_state(), ElectionState::LeaderLogReplication);
}

/// Where a replay ends is where the record of terms has its next base, the end
/// position `cavalry-proto`'s `replay_end` gives for the same record.
#[test]
fn a_replay_ends_where_the_arithmetic_of_the_page_ends_it() {
    let record = [(0, 0), (1, 64), (2, 96)];
    let spans = [
        TermSpan {
            term: 0,
            base: 0,
            end: 64,
        },
        TermSpan {
            term: 1,
            base: 64,
            end: 96,
        },
        TermSpan {
            term: 2,
            base: 96,
            end: -1,
        },
    ];
    for position in [0, 32, 63, 64, 95, 96, 200] {
        assert_eq!(
            base_above(record, position).unwrap_or(-1),
            replay_end(&spans, position).end_position,
            "from {position}"
        );
    }
}

/// What starts nothing: a position above the leader's own append position, which the
/// leader does not hold; a position below 0; a message for a term that is not the
/// leader's; one from an id that is no other member's. None is counted. A position
/// from the leader's term base to its append position is a catch-up within its term,
/// which `tests/catchups.rs` has.
#[test]
fn a_catch_up_position_that_is_no_request_of_this_leaders_starts_nothing() {
    let (mut leader, mut io, ms) = leader();
    for message in [
        catchup_position(2, 2, 128),
        catchup_position(2, 2, -1),
        catchup_position(2, 1, 32),
        catchup_position(2, 3, 32),
        catchup_position(0, 2, 32),
        catchup_position(7, 2, 32),
    ] {
        read(&mut leader, &mut io, ms, &message);
    }
    work(&mut leader, &mut io, ms + 1);
    assert_eq!(io.count(|call| matches!(call, Call::StartReplay(..))), 0);
    assert_eq!(leader.counts().log_replications, 0);
    assert_eq!(leader.counts().catchups, 0);
}

/// The appointed leader serves no replay: no member beside it replicates.
#[test]
fn an_appointed_leader_starts_no_replay() {
    let mut io = Stub::new();
    let mut leader = Consensus::new(&[0, 1, 2], 0).expect("a member");
    work(&mut leader, &mut io, 1);
    for other in [1, 2] {
        read(&mut leader, &mut io, 2, &canvass_position(other, -1, 0));
    }
    work(&mut leader, &mut io, 3);
    work(&mut leader, &mut io, 4);
    assert_eq!(leader.election_state(), ElectionState::LeaderLogReplication);
    io.append_position = 64;
    work(&mut leader, &mut io, 5);
    read(&mut leader, &mut io, 6, &catchup_position(2, 0, -1));
    read(&mut leader, &mut io, 6, &catchup_position(2, 0, 0));
    assert_eq!(started(&io), vec![]);
    assert_eq!(leader.counts().log_replications, 0);
}

/// A position the leader's recordings do not hold, purged by retention, is answered
/// by nothing and counted nowhere: the member's own timer counts it. The leader holds
/// nothing for the member afterwards, so a later request is served.
#[test]
fn a_position_the_recordings_do_not_hold_is_answered_by_nothing() {
    let (mut leader, mut io, ms) = leader();
    io.purged = true;
    read(&mut leader, &mut io, ms, &catchup_position(2, 2, 0));
    assert_eq!(started(&io), vec![(2, 0, 64)], "it asked its recordings");
    assert_eq!(leader.counts().log_replications, 0);
    work(&mut leader, &mut io, ms + 1);

    io.purged = false;
    read(&mut leader, &mut io, ms + 2, &catchup_position(2, 2, 32));
    assert_eq!(started(&io), vec![(2, 0, 64), (2, 32, 64)]);
    assert_eq!(leader.counts().log_replications, 1);
    assert_eq!(stops(&io, 2), 0, "nothing ran, so nothing was stopped");
}

/// A term whose bytes lie in two recordings: the first replay ends by itself at its
/// recording's end, 32, below the term's end. Nothing of the leader's own follows
/// it, whatever the member reports: the member asks for the next recording's replay
/// from where its log ends, once a recording is armed for the image (ADR 0092), and
/// the leader starts it then, with no replay to stop. Each replay is counted. The
/// second ends at the term's end, and nothing follows it either.
#[test]
fn the_next_recording_is_replayed_when_the_member_asks_for_it() {
    let (mut leader, mut io, ms) = leader();
    read(&mut leader, &mut io, ms, &catchup_position(2, 2, 0));
    assert_eq!(started(&io), vec![(2, 0, 64)]);

    // The replay runs: nothing new.
    work(&mut leader, &mut io, ms + 1);
    assert_eq!(started(&io).len(), 1);

    io.stopped[2] = Some(ReplayStopped {
        position: 32,
        ended: true,
    });
    work(&mut leader, &mut io, ms + 2);
    read(&mut leader, &mut io, ms + 3, &replicated_to(2, 0, 32));
    work(&mut leader, &mut io, ms + 4);
    assert_eq!(
        started(&io).len(),
        1,
        "the member has reported the end, and the leader starts nothing by itself"
    );

    read(&mut leader, &mut io, ms + 5, &catchup_position(2, 2, 32));
    assert_eq!(started(&io), vec![(2, 0, 64), (2, 32, 64)]);
    assert_eq!(
        stops(&io, 2),
        0,
        "the first had stopped, and its end was read"
    );
    assert_eq!(leader.counts().log_replications, 2);

    io.stopped[2] = Some(ReplayStopped {
        position: 64,
        ended: true,
    });
    work(&mut leader, &mut io, ms + 6);
    read(&mut leader, &mut io, ms + 7, &replicated_to(2, 0, 64));
    work(&mut leader, &mut io, ms + 8);
    assert_eq!(started(&io).len(), 2, "the term's end was reached");
    assert_eq!(stops(&io, 2), 0);
}

/// A replay that stopped for any reason but its end, no subscriber or a fault of the
/// recording, is followed by nothing either, whatever the member reports: the member
/// asks again when the replay brought it something, and else its progress timer
/// fails its election.
#[test]
fn a_replay_that_did_not_end_by_itself_is_followed_by_nothing() {
    let (mut leader, mut io, ms) = leader();
    read(&mut leader, &mut io, ms, &catchup_position(2, 2, 0));
    io.stopped[2] = Some(ReplayStopped {
        position: 32,
        ended: false,
    });
    work(&mut leader, &mut io, ms + 1);
    read(&mut leader, &mut io, ms + 2, &replicated_to(2, 0, 32));
    work(&mut leader, &mut io, ms + 3);
    assert_eq!(started(&io), vec![(2, 0, 64)]);

    // The member asks again, from where it is: nothing runs, so one starts at once.
    read(&mut leader, &mut io, ms + 4, &catchup_position(2, 2, 32));
    assert_eq!(started(&io), vec![(2, 0, 64), (2, 32, 64)]);
    assert_eq!(stops(&io, 2), 0);
    assert_eq!(leader.counts().log_replications, 2);
}

/// A member that asks while a replay runs to it failed its election and began again.
/// The leader stops the replay it runs, once, and starts none while that one still
/// counts as running. It keeps the member's request, the newest where two came, and
/// starts it when the replay has stopped: the member never finds two replays on its
/// endpoint, nor a stale one.
#[test]
fn a_member_that_asks_again_is_served_its_newest_request_once_the_old_replay_stopped() {
    let (mut leader, mut io, ms) = leader();
    read(&mut leader, &mut io, ms, &catchup_position(2, 2, 0));
    read(&mut leader, &mut io, ms + 1, &catchup_position(2, 2, 16));
    assert_eq!(stops(&io, 2), 1);
    read(&mut leader, &mut io, ms + 2, &catchup_position(2, 2, 32));
    assert_eq!(stops(&io, 2), 1, "it asked the one replay to stop once");
    work(&mut leader, &mut io, ms + 3);
    assert_eq!(started(&io), vec![(2, 0, 64)], "the old one still runs");

    io.stopped[2] = Some(ReplayStopped {
        position: 24,
        ended: false,
    });
    work(&mut leader, &mut io, ms + 4);
    assert_eq!(started(&io), vec![(2, 0, 64), (2, 32, 64)]);
    assert_eq!(leader.counts().log_replications, 2);

    // The new one is served as the first was: it ends at the term's end.
    io.stopped[2] = Some(ReplayStopped {
        position: 64,
        ended: true,
    });
    work(&mut leader, &mut io, ms + 5);
    work(&mut leader, &mut io, ms + 6);
    assert_eq!(started(&io).len(), 2);
}

/// A member that canvasses is in no replication. The leader first stops the replay
/// it runs to it, and starts none when that one has stopped. One it was waiting to
/// start, a request whose start faulted, is dropped: the append position that would
/// have started it starts nothing.
#[test]
fn a_canvass_position_stops_the_replay_and_drops_the_one_owed() {
    let (mut leader, mut io, ms) = leader();
    // To member 2 a replay runs; to member 1 one is owed, its start having faulted.
    read(&mut leader, &mut io, ms, &catchup_position(2, 2, 0));
    io.fail_start_to = Some(1);
    assert_eq!(
        leader.on_message(at(ms), &catchup_position(1, 2, 0), &mut io),
        Err("the replay cannot be started")
    );
    io.fail_start_to = None;
    work(&mut leader, &mut io, ms + 1);
    assert_eq!(
        started(&io).len(),
        2,
        "member 1 has reported nothing: its replay stays owed"
    );

    for member in [1, 2] {
        read(
            &mut leader,
            &mut io,
            ms + 2,
            &canvass_position(member, 0, 32),
        );
    }
    assert_eq!(stops(&io, 2), 1);
    assert_eq!(stops(&io, 1), 0, "nothing ran to member 1");
    // The stop comes before the leader's answer to the canvass position.
    let stop = io
        .first(|call| *call == Call::StopReplay(2))
        .expect("the stop");
    let answer = io
        .calls
        .iter()
        .rposition(|call| {
            matches!(
                call,
                Some(Call::Send {
                    to: 2,
                    template: Template::NewLeadershipTerm
                })
            )
        })
        .expect("the answer");
    assert!(stop < answer);

    io.stopped[2] = Some(ReplayStopped {
        position: 16,
        ended: false,
    });
    work(&mut leader, &mut io, ms + 3);
    for member in [1, 2] {
        read(&mut leader, &mut io, ms + 4, &replicated_to(member, 0, 32));
    }
    work(&mut leader, &mut io, ms + 5);
    assert_eq!(
        started(&io).len(),
        2,
        "no replay follows a canvass position"
    );
    assert_eq!(
        leader.counts().log_replications,
        1,
        "the start that faulted is no replay"
    );
}

/// A stop the driver refuses is a fault: the message's call returns it, the leader
/// still holds the replay as running, and the member's next canvass position stops
/// it again.
#[test]
fn a_stop_that_fails_is_returned_and_asked_again() {
    let (mut leader, mut io, ms) = leader();
    read(&mut leader, &mut io, ms, &catchup_position(2, 2, 0));
    io.fail_stop = true;
    assert_eq!(
        leader.on_message(at(ms + 1), &canvass_position(2, 0, 0), &mut io),
        Err("the replay cannot be stopped")
    );
    io.fail_stop = false;
    read(&mut leader, &mut io, ms + 101, &canvass_position(2, 0, 0));
    assert_eq!(stops(&io, 2), 2);
    // Stopped now, and asked no more.
    read(&mut leader, &mut io, ms + 201, &canvass_position(2, 0, 0));
    assert_eq!(stops(&io, 2), 2);
}

/// A start the driver refuses is a fault, and the request is not lost with it. The
/// leader keeps it as a replay owed, and starts it on a later cycle once the member
/// has reported the position it asked from, which a member that replicates does
/// every leader heartbeat interval. So it is after a stop too: how the stopped
/// replay ended is handed over once, and the request outlives the fault.
#[test]
fn a_start_that_fails_is_tried_again_when_the_member_reports() {
    let (mut leader, mut io, ms) = leader();
    io.fail_start = true;
    assert_eq!(
        leader.on_message(at(ms), &catchup_position(2, 2, 32), &mut io),
        Err("the replay cannot be started")
    );
    assert_eq!(leader.counts().log_replications, 0);
    io.fail_start = false;
    work(&mut leader, &mut io, ms + 1);
    assert_eq!(started(&io).len(), 1, "the member has reported nothing yet");
    read(&mut leader, &mut io, ms + 2, &replicated_to(2, 0, 32));
    work(&mut leader, &mut io, ms + 3);
    assert_eq!(started(&io), vec![(2, 32, 64), (2, 32, 64)]);
    assert_eq!(leader.counts().log_replications, 1);

    // The member asks again while that replay runs, and the start after the stop
    // faults once.
    read(&mut leader, &mut io, ms + 4, &catchup_position(2, 2, 48));
    assert_eq!(stops(&io, 2), 1);
    io.stopped[2] = Some(ReplayStopped {
        position: 40,
        ended: false,
    });
    io.fail_start = true;
    assert_eq!(
        leader.do_work(at(ms + 5), &mut io),
        Err("the replay cannot be started")
    );
    io.fail_start = false;
    work(&mut leader, &mut io, ms + 6);
    assert_eq!(started(&io).len(), 3, "the member is at 32, below 48");
    read(&mut leader, &mut io, ms + 7, &replicated_to(2, 0, 48));
    work(&mut leader, &mut io, ms + 8);
    assert_eq!(started(&io).last(), Some(&(2, 48, 64)));
    assert_eq!(started(&io).len(), 4);
    assert_eq!(leader.counts().log_replications, 2);
}

/// A replay's fault costs the leader nothing else. The leader looks after its replays
/// after its state's own work, so in a cycle where a start faults its heartbeat has
/// already left, its new leadership term and its commit position to both members, and
/// the fault is returned from the cycle. A fault that lasts does not keep the leader
/// from watching its quorum: when both members have been silent for the leader
/// heartbeat timeout it holds its election, a follower, and serves nobody.
#[test]
fn a_replay_that_faults_costs_the_leader_no_heartbeat_and_no_watch_of_its_quorum() {
    let (mut leader, mut io, ms) = leader();
    io.fail_start = true;
    assert_eq!(
        leader.on_message(at(ms), &catchup_position(2, 2, 32), &mut io),
        Err("the replay cannot be started")
    );
    // The member reports the position it asked from, so every cycle tries the start.
    read(&mut leader, &mut io, ms, &replicated_to(2, 0, 32));
    for cycle in [ms + 300, ms + 600] {
        io.clear();
        assert_eq!(
            leader.do_work(at(cycle), &mut io),
            Err("the replay cannot be started")
        );
        for member in [1, 2] {
            for template in [Template::NewLeadershipTerm, Template::CommitPosition] {
                assert_eq!(
                    io.count(|call| *call
                        == Call::Send {
                            to: member,
                            template
                        }),
                    1,
                    "{template:?} to member {member} at {cycle}"
                );
            }
        }
        let heartbeat = io
            .calls
            .iter()
            .rposition(|call| matches!(call, Some(Call::Send { .. })))
            .expect("the heartbeat");
        let start = io
            .first(|call| matches!(call, Call::StartReplay(..)))
            .expect("the start");
        assert!(heartbeat < start, "the state's work comes first");
    }
    assert_eq!(leader.counts().log_replications, 0);

    // Nothing from either member for the leader heartbeat timeout.
    assert_eq!(leader.do_work(at(ms + 10_001), &mut io), Ok(1));
    assert_eq!(leader.election_state(), ElectionState::Init);
    assert_eq!(leader.role(), Role::Follower);
    assert_eq!(leader.counts().leader_quorum_losses, 1);
}

/// A leader that holds an election serves no member from init on. Here its active
/// members fall below a quorum in leader log replication, and it begins again: in
/// init it gives its log up, stops the replay it ran, and forgets the one it owed. A
/// stop that fails leaves it in init, and the next cycle stops the replay again. As a
/// follower it reads no catch-up position.
#[test]
fn a_leader_that_holds_an_election_stops_the_replays_it_served() {
    let (mut leader, mut io, ms) = leader();
    read(&mut leader, &mut io, ms, &catchup_position(2, 2, 0));
    read(&mut leader, &mut io, ms, &catchup_position(1, 2, 64));
    io.stopped[1] = Some(ReplayStopped {
        position: 80,
        ended: true,
    });
    work(&mut leader, &mut io, ms + 1);
    assert_eq!(started(&io), vec![(2, 0, 64), (1, 64, 96)]);

    // Nothing from either member for the leader heartbeat timeout.
    let lost = ms + 10_001;
    work(&mut leader, &mut io, lost);
    assert_eq!(leader.election_state(), ElectionState::Init);
    assert_eq!(leader.role(), Role::Follower);
    assert_eq!(leader.counts().leader_quorum_losses, 1);
    assert_eq!(stops(&io, 2), 0, "not before init");

    io.fail_stop = true;
    assert_eq!(
        leader.do_work(at(lost + 1), &mut io),
        Err("the replay cannot be stopped")
    );
    assert_eq!(leader.election_state(), ElectionState::Init);
    io.fail_stop = false;
    work(&mut leader, &mut io, lost + 2);
    assert_eq!(leader.election_state(), ElectionState::Canvass);
    assert_eq!(stops(&io, 2), 2, "asked again after the fault");
    assert_eq!(stops(&io, 1), 0, "the one owed to member 1 ran nowhere");
    assert_eq!(io.count(|call| *call == Call::RemoveLog), 1);

    // A follower serves nobody, and owes member 1 nothing any more.
    read(&mut leader, &mut io, lost + 3, &replicated_to(1, 1, 80));
    read(&mut leader, &mut io, lost + 3, &catchup_position(1, 2, 80));
    work(&mut leader, &mut io, lost + 4);
    assert_eq!(started(&io).len(), 2);
}
