//! Every move of the election and of the role, checked against `counters.toml`
//! (CLAUDE.md, invariant 4; ADR 0067, ADR 0081, ADR 0082). One call into a core, a duty cycle or
//! a message read, moves its election at most once and its role at most once. After
//! every call the test checks that each move it made is a `[[transition]]` of its
//! machine, `cluster_election` or `cluster_role`, and that a counter registered for it
//! rose, read through [`Counts::by_counter`]. The paths below make every registered
//! transition of both machines, and the test fails on a row that none of them made, so
//! the registry and the core cannot drift apart in either direction.

mod common;

use std::collections::BTreeSet;
use std::fmt::Debug;

use cavalry_proto::cluster::Message;
use cavalry_raft::{Consensus, Counts, ElectionState, Role};
use cavalry_shm::counters::generated::COUNTERS;
use common::{Stub, append_position, at, canvass_position, new_leadership_term};

/// The two machines a core moves.
const ELECTION: &str = "cluster_election";
const ROLE: &str = "cluster_role";

/// One `[[transition]]` row of a machine: from, to and counter.
type Row = (String, String, String);

/// The rows of `machine` in `counters.toml`, the file `just lint-counters` checks.
fn registered(machine: &str) -> Vec<Row> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../counters.toml");
    let text = std::fs::read_to_string(path).expect("counters.toml at the workspace root");
    let mut tables: Vec<Vec<(String, String)>> = Vec::new();
    let mut in_transition = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with("[[") {
            in_transition = line == "[[transition]]";
            if in_transition {
                tables.push(Vec::new());
            }
        } else if in_transition
            && let (Some(table), Some((key, value))) = (tables.last_mut(), line.split_once('='))
        {
            table.push((
                key.trim().to_owned(),
                value.trim().trim_matches('"').to_owned(),
            ));
        }
    }
    let field = |table: &[(String, String)], key: &str| {
        table
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .expect("every [[transition]] has machine, from, to and counter")
    };
    tables
        .iter()
        .filter(|table| field(table, "machine") == machine)
        .map(|table| {
            (
                field(table, "from"),
                field(table, "to"),
                field(table, "counter"),
            )
        })
        .collect()
}

/// The state's name in `counters.toml`: the variant, lower-cased, with an underscore
/// between words, as `just lint-counters` reads it.
fn name(state: impl Debug) -> String {
    let mut out = String::new();
    for (index, ch) in format!("{state:?}").chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if index > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// The value of the counter named `counter` among `counts`.
fn value(counts: Counts, counter: &str) -> u64 {
    let def = COUNTERS
        .iter()
        .find(|def| def.name == counter)
        .unwrap_or_else(|| panic!("{counter} is a [[counter]] of counters.toml"));
    counts
        .by_counter()
        .iter()
        .find(|(id, _)| *id == def.id)
        .map(|&(_, value)| value)
        .unwrap_or_else(|| panic!("{counter} is one of the core's counts"))
}

/// One machine's registry, and the rows the calls so far have made.
struct Machine {
    name: &'static str,
    rows: Vec<Row>,
    made: BTreeSet<Row>,
}

impl Machine {
    fn new(name: &'static str) -> Self {
        let rows = registered(name);
        assert!(!rows.is_empty(), "{name} has transitions");
        Self {
            name,
            rows,
            made: BTreeSet::new(),
        }
    }

    /// Checks the move, if any, from the state named `from` to the one named `to`, with
    /// the counts before the call and after it.
    fn check(&mut self, from: String, to: String, counted: Counts, counts: Counts) {
        if from == to {
            return;
        }
        let machine = self.name;
        let rows: Vec<Row> = self
            .rows
            .iter()
            .filter(|row| row.0 == from && row.1 == to)
            .cloned()
            .collect();
        assert!(
            !rows.is_empty(),
            "{from} -> {to} is no [[transition]] of {machine} in counters.toml"
        );
        let mut rose = false;
        for row in rows {
            if value(counts, &row.2) > value(counted, &row.2) {
                rose = true;
                self.made.insert(row);
            }
        }
        assert!(
            rose,
            "{from} -> {to} of {machine} added one to none of its counters"
        );
    }

    /// The rows no call has made.
    fn unmade(&self) -> Vec<Row> {
        let rows: BTreeSet<Row> = self.rows.iter().cloned().collect();
        rows.difference(&self.made).cloned().collect()
    }
}

/// The registry of both machines, and the rows the calls so far have made.
struct Watch {
    election: Machine,
    role: Machine,
}

impl Watch {
    fn new() -> Self {
        Self {
            election: Machine::new(ELECTION),
            role: Machine::new(ROLE),
        }
    }

    /// Checks the moves, if any, that one call made from `before` to `core`'s state
    /// and role.
    fn check(&mut self, before: (ElectionState, Role, Counts), core: &Consensus) {
        let (state, role, counted) = before;
        let counts = core.counts();
        self.election
            .check(name(state), name(core.election_state()), counted, counts);
        self.role
            .check(name(role), name(core.role()), counted, counts);
    }

    /// One duty cycle at `ms`, checked.
    fn work(&mut self, core: &mut Consensus, io: &mut Stub, ms: u64) {
        let before = (core.election_state(), core.role(), core.counts());
        core.do_work(at(ms), io).expect("no fault");
        self.check(before, core);
    }

    /// One message read at `ms`, checked.
    fn read(&mut self, core: &mut Consensus, io: &mut Stub, ms: u64, message: &Message<'_>) {
        let before = (core.election_state(), core.role(), core.counts());
        core.on_message(at(ms), message, io).expect("no fault");
        self.check(before, core);
    }
}

/// The leader's new leadership term to a member that reported term 0, as a member that
/// has followed the term and begun again reports it.
fn term_zero_again(log_position: i64) -> Message<'static> {
    Message::NewLeadershipTerm {
        log_leadership_term_id: 0,
        next_leadership_term_id: -1,
        next_term_base_log_position: -1,
        next_log_position: -1,
        leadership_term_id: 0,
        term_base_log_position: 0,
        log_position,
        leader_recording_id: 5,
        timestamp: 1_700_000_000_000,
        leader_member_id: 0,
        log_session_id: 77,
        app_version: 0,
        is_startup: true,
        commit_position: 0,
    }
}

/// A follower of `[0, 1, 2]` with id 2, taken from init to follower log await by the
/// leader's term at position 0, from `ms` on; returns the next millisecond.
fn to_follower_log_await(
    watch: &mut Watch,
    follower: &mut Consensus,
    io: &mut Stub,
    ms: u64,
    term: &Message<'_>,
) -> u64 {
    watch.work(follower, io, ms);
    assert_eq!(follower.election_state(), ElectionState::Canvass);
    watch.read(follower, io, ms + 1, term);
    watch.work(follower, io, ms + 2);
    watch.work(follower, io, ms + 3);
    assert_eq!(follower.election_state(), ElectionState::FollowerLogAwait);
    ms + 4
}

/// A member of `[0, 1, 2]` with id 0 that elects, taken from init to nominate by the
/// other two members' canvass positions, neither ahead of it, from `ms` on; returns the
/// next millisecond.
fn to_nominate(watch: &mut Watch, member: &mut Consensus, io: &mut Stub, ms: u64) -> u64 {
    watch.work(member, io, ms);
    assert_eq!(member.election_state(), ElectionState::Canvass);
    for other in [1, 2] {
        watch.read(member, io, ms + 1, &canvass_position(other, -1, 0));
    }
    watch.work(member, io, ms + 2);
    assert_eq!(member.election_state(), ElectionState::Nominate);
    ms + 3
}

/// The same member taken on to candidate ballot: nominate's delay is below half the
/// election timeout, 500 ms. Returns the millisecond its ballot began at.
fn to_candidate_ballot(watch: &mut Watch, member: &mut Consensus, io: &mut Stub, ms: u64) -> u64 {
    let ms = to_nominate(watch, member, io, ms) + 500;
    watch.work(member, io, ms);
    assert_eq!(member.election_state(), ElectionState::CandidateBallot);
    ms
}

/// The same member taken on to leader log replication as the leader of term 0, on both
/// votes, with its log added. Returns the next millisecond; the other members
/// reported last in canvass, 505 ms before it.
fn to_leader_log_replication(
    watch: &mut Watch,
    member: &mut Consensus,
    io: &mut Stub,
    ms: u64,
) -> u64 {
    let ms = to_candidate_ballot(watch, member, io, ms);
    for voter in [1, 2] {
        let vote = Message::Vote {
            candidate_term_id: 0,
            log_leadership_term_id: -1,
            log_position: 0,
            candidate_member_id: 0,
            follower_member_id: voter,
            vote: true,
        };
        watch.read(member, io, ms + 1, &vote);
    }
    watch.work(member, io, ms + 2);
    assert_eq!(member.election_state(), ElectionState::LeaderLogReplication);
    watch.work(member, io, ms + 3);
    ms + 4
}

/// The leader taken on to leader ready, where it waits: member 1 has joined its log,
/// and member 2, heard in canvass, has not. Returns the next millisecond.
fn to_leader_ready(watch: &mut Watch, member: &mut Consensus, io: &mut Stub, ms: u64) -> u64 {
    let ms = to_leader_log_replication(watch, member, io, ms);
    watch.read(member, io, ms, &append_position(1, 0));
    for step in 0..4 {
        watch.work(member, io, ms + step);
    }
    assert_eq!(member.election_state(), ElectionState::LeaderReady);
    ms + 4
}

/// The leader taken on to closed: both members have joined its log. Returns the next
/// millisecond.
fn to_closed_leader(watch: &mut Watch, member: &mut Consensus, io: &mut Stub, ms: u64) -> u64 {
    let ms = to_leader_log_replication(watch, member, io, ms);
    for step in 0..6 {
        for other in [1, 2] {
            watch.read(member, io, ms + step, &append_position(other, 0));
        }
        watch.work(member, io, ms + step);
    }
    assert_eq!(member.election_state(), ElectionState::Closed);
    assert_eq!(member.role(), Role::Leader);
    ms + 6
}

/// Member 1's new leadership term for term 0 with its base at 64, to a member whose
/// log holds no term and ends at 0. The member is below the term's base and would
/// replicate what it lacks (state 9), which this path does not have, so its election
/// fails.
fn term_past_the_log() -> Message<'static> {
    let mut term = new_leadership_term(1, 64);
    if let Message::NewLeadershipTerm {
        next_term_base_log_position,
        term_base_log_position,
        ..
    } = &mut term
    {
        *next_term_base_log_position = 64;
        *term_base_log_position = 64;
    }
    term
}

/// Member 1's request vote for `term`, with a log that holds no term.
fn request_vote(term: i64) -> Message<'static> {
    Message::RequestVote {
        log_leadership_term_id: -1,
        log_position: 0,
        candidate_term_id: term,
        candidate_member_id: 1,
        protocol_version: 0,
    }
}

/// A member that elects, with id 0 among `[0, 1, 2]` and a seed of its own.
fn elector(seed: u64) -> Consensus {
    Consensus::with_election(&[0, 1, 2], 0, seed).expect("a member")
}

#[test]
fn every_transition_of_the_election_and_the_role_adds_one_to_a_counter_registered_for_it() {
    let mut watch = Watch::new();

    // The leader of three: init, canvass, then leader log replication, replay, init and
    // ready, and closed once both members have joined at its log position.
    let mut io = Stub::new();
    let mut leader = Consensus::new(&[0, 1, 2], 0).expect("a member");
    watch.work(&mut leader, &mut io, 1);
    for member in [1, 2] {
        watch.read(&mut leader, &mut io, 2, &canvass_position(member, -1, 0));
    }
    for ms in 3..=20 {
        for member in [1, 2] {
            watch.read(&mut leader, &mut io, ms, &append_position(member, 0));
        }
        watch.work(&mut leader, &mut io, ms);
    }
    assert_eq!(leader.election_state(), ElectionState::Closed);

    // A cluster of one leads from init.
    let mut io = Stub::new();
    let mut alone = Consensus::new(&[5], 5).expect("a member");
    for ms in 1..=10 {
        watch.work(&mut alone, &mut io, ms);
    }
    assert_eq!(alone.election_state(), ElectionState::Closed);

    // A follower joins and closes; loses its image and begins again; joins again and
    // closes; then hears nothing for the leader heartbeat timeout and begins again.
    let mut io = Stub::new();
    let mut follower = Consensus::new(&[0, 1, 2], 2).expect("a member");
    let ms = to_follower_log_await(
        &mut watch,
        &mut follower,
        &mut io,
        1,
        &new_leadership_term(0, 0),
    );
    io.image = Some(0);
    watch.work(&mut follower, &mut io, ms);
    watch.work(&mut follower, &mut io, ms + 1);
    assert_eq!(follower.election_state(), ElectionState::Closed);
    io.image = None;
    watch.work(&mut follower, &mut io, ms + 2);
    assert_eq!(follower.election_state(), ElectionState::Init);
    let ms = to_follower_log_await(
        &mut watch,
        &mut follower,
        &mut io,
        ms + 3,
        &term_zero_again(0),
    );
    io.image = Some(0);
    watch.work(&mut follower, &mut io, ms);
    watch.work(&mut follower, &mut io, ms + 1);
    assert_eq!(follower.election_state(), ElectionState::Closed);
    watch.work(&mut follower, &mut io, ms + 1 + 10_000);
    assert_eq!(follower.election_state(), ElectionState::Init);

    // A follower ahead of the leader's term base needs a truncation: it fails in
    // canvass.
    let mut io = Stub::new();
    io.append_position = 64;
    let mut ahead = Consensus::new(&[0, 1, 2], 1).expect("a member");
    watch.work(&mut ahead, &mut io, 1);
    watch.read(&mut ahead, &mut io, 2, &new_leadership_term(0, 0));
    assert_eq!(ahead.election_state(), ElectionState::Init);

    // A follower behind the leader's log position needs a catch-up: it fails in
    // follower replay.
    let mut io = Stub::new();
    let mut behind = Consensus::new(&[0, 1, 2], 2).expect("a member");
    watch.work(&mut behind, &mut io, 1);
    watch.read(&mut behind, &mut io, 2, &new_leadership_term(0, 64));
    assert_eq!(behind.election_state(), ElectionState::FollowerReplay);
    watch.work(&mut behind, &mut io, 3);
    assert_eq!(behind.election_state(), ElectionState::Init);

    // An image that joins past the append position: the follower fails in follower
    // log await.
    let mut io = Stub::new();
    let mut late = Consensus::new(&[0, 1, 2], 2).expect("a member");
    let ms = to_follower_log_await(
        &mut watch,
        &mut late,
        &mut io,
        1,
        &new_leadership_term(0, 0),
    );
    io.image = Some(64);
    watch.work(&mut late, &mut io, ms);
    assert_eq!(late.election_state(), ElectionState::Init);

    // A follower whose append position cannot be sent for the leader heartbeat timeout
    // fails in follower ready.
    let mut io = Stub::new();
    let mut unheard = Consensus::new(&[0, 1, 2], 2).expect("a member");
    let ms = to_follower_log_await(
        &mut watch,
        &mut unheard,
        &mut io,
        1,
        &new_leadership_term(0, 0),
    );
    io.image = Some(0);
    io.refuse_send = true;
    watch.work(&mut unheard, &mut io, ms);
    assert_eq!(unheard.election_state(), ElectionState::FollowerReady);
    watch.work(&mut unheard, &mut io, ms + 1);
    watch.work(&mut unheard, &mut io, ms + 10_000);
    assert_eq!(unheard.election_state(), ElectionState::Init);

    // A member that elects: canvass, nominate, candidate ballot, and the leader of term
    // 0 on both votes, each from a member it heard.
    let mut io = Stub::new();
    let mut candidate = elector(1);
    let ms = to_candidate_ballot(&mut watch, &mut candidate, &mut io, 1);
    for voter in [1, 2] {
        let vote = Message::Vote {
            candidate_term_id: 0,
            log_leadership_term_id: -1,
            log_position: 0,
            candidate_member_id: 0,
            follower_member_id: voter,
            vote: true,
        };
        watch.read(&mut candidate, &mut io, ms + 1, &vote);
    }
    watch.work(&mut candidate, &mut io, ms + 2);
    assert_eq!(
        candidate.election_state(),
        ElectionState::LeaderLogReplication
    );

    // A candidate with no vote but its own canvasses again at the election timeout.
    let mut io = Stub::new();
    let mut unanswered = elector(2);
    let ms = to_candidate_ballot(&mut watch, &mut unanswered, &mut io, 1);
    watch.work(&mut unanswered, &mut io, ms + 1_000);
    assert_eq!(unanswered.election_state(), ElectionState::Canvass);

    // A candidate that reads a request vote for a higher term from a log at least its
    // own votes for it and waits for it; its new leadership term then takes it to
    // follower replay.
    let mut io = Stub::new();
    let mut outbid = elector(3);
    let ms = to_candidate_ballot(&mut watch, &mut outbid, &mut io, 1);
    watch.read(&mut outbid, &mut io, ms + 1, &request_vote(1));
    assert_eq!(outbid.election_state(), ElectionState::FollowerBallot);
    let mut term_one = new_leadership_term(1, 0);
    if let Message::NewLeadershipTerm {
        leadership_term_id,
        next_leadership_term_id,
        ..
    } = &mut term_one
    {
        *leadership_term_id = 1;
        *next_leadership_term_id = 1;
    }
    watch.read(&mut outbid, &mut io, ms + 2, &term_one);
    assert_eq!(outbid.election_state(), ElectionState::FollowerReplay);

    // A candidate that reads the new leadership term of its own candidate term from
    // another member follows it, a follower by the step to follower replay. Its ballot
    // ended with a leader, so it counts no ballot lost.
    let mut io = Stub::new();
    let mut beaten = elector(4);
    let ms = to_candidate_ballot(&mut watch, &mut beaten, &mut io, 1);
    watch.read(&mut beaten, &mut io, ms + 1, &new_leadership_term(1, 0));
    assert_eq!(beaten.election_state(), ElectionState::FollowerReplay);
    assert_eq!(beaten.role(), Role::Follower);
    assert_eq!(beaten.leader_member_id(), 1);
    assert_eq!(beaten.counts().ballots_lost, 0);

    // A candidate below that leader's term base would replicate what it lacks, which
    // this path does not have: it fails, a follower from the same call, and is one
    // still when init begins its next election.
    let mut io = Stub::new();
    let mut below = elector(5);
    let ms = to_candidate_ballot(&mut watch, &mut below, &mut io, 1);
    watch.read(&mut below, &mut io, ms + 1, &term_past_the_log());
    assert_eq!(below.election_state(), ElectionState::Init);
    assert_eq!(below.role(), Role::Follower);
    watch.work(&mut below, &mut io, ms + 2);
    assert_eq!(below.election_state(), ElectionState::Canvass);
    assert_eq!(below.role(), Role::Follower);

    // A candidate ahead of that leader's term base truncates to it, which moves
    // neither its election nor its role, and follows from there.
    let mut io = Stub::new();
    io.append_position = 64;
    let mut ahead = elector(5);
    let ms = to_candidate_ballot(&mut watch, &mut ahead, &mut io, 1);
    watch.read(&mut ahead, &mut io, ms + 1, &new_leadership_term(1, 0));
    assert_eq!(ahead.election_state(), ElectionState::FollowerReplay);
    assert_eq!(ahead.counts().log_truncations, 1);

    // A member in canvass votes for a candidate and waits for it; with no new
    // leadership term for the election timeout it canvasses again.
    let mut io = Stub::new();
    let mut voter = elector(6);
    watch.work(&mut voter, &mut io, 1);
    watch.read(&mut voter, &mut io, 2, &request_vote(0));
    assert_eq!(voter.election_state(), ElectionState::FollowerBallot);
    watch.work(&mut voter, &mut io, 2 + 1_000);
    assert_eq!(voter.election_state(), ElectionState::Canvass);

    // A member in nominate votes for a candidate, as one in canvass does.
    let mut io = Stub::new();
    let mut nominee = elector(7);
    let ms = to_nominate(&mut watch, &mut nominee, &mut io, 1);
    watch.read(&mut nominee, &mut io, ms, &request_vote(0));
    assert_eq!(nominee.election_state(), ElectionState::FollowerBallot);

    // A member in nominate reads a leader's term as canvass does: it follows, or
    // fails where it would need a replication.
    let mut io = Stub::new();
    let mut nominee = elector(8);
    let ms = to_nominate(&mut watch, &mut nominee, &mut io, 1);
    watch.read(&mut nominee, &mut io, ms, &new_leadership_term(1, 0));
    assert_eq!(nominee.election_state(), ElectionState::FollowerReplay);
    let mut io = Stub::new();
    let mut nominee = elector(9);
    let ms = to_nominate(&mut watch, &mut nominee, &mut io, 1);
    watch.read(&mut nominee, &mut io, ms, &term_past_the_log());
    assert_eq!(nominee.election_state(), ElectionState::Init);

    // A member in follower ballot below its candidate's term base fails.
    let mut io = Stub::new();
    let mut voter = elector(10);
    watch.work(&mut voter, &mut io, 1);
    watch.read(&mut voter, &mut io, 2, &request_vote(0));
    assert_eq!(voter.election_state(), ElectionState::FollowerBallot);
    watch.read(&mut voter, &mut io, 3, &term_past_the_log());
    assert_eq!(voter.election_state(), ElectionState::Init);

    // A leader that elects, its election closed, holds an election on a term above its
    // own, a follower from the same call: on a canvass position, a request vote and a
    // commit position.
    let higher_canvass = Message::CanvassPosition {
        log_leadership_term_id: 0,
        log_position: 0,
        leadership_term_id: 1,
        follower_member_id: 2,
        protocol_version: 0,
    };
    let higher_commit = Message::CommitPosition {
        leadership_term_id: 1,
        log_position: 0,
        leader_member_id: 2,
    };
    for message in [higher_canvass, request_vote(1), higher_commit] {
        let mut io = Stub::new();
        let mut leader = elector(11);
        let ms = to_closed_leader(&mut watch, &mut leader, &mut io, 1);
        watch.read(&mut leader, &mut io, ms, &message);
        assert_eq!(leader.election_state(), ElectionState::Init);
        assert_eq!(leader.role(), Role::Follower);
        watch.work(&mut leader, &mut io, ms + 1);
        assert_eq!(leader.election_state(), ElectionState::Canvass);
    }

    // Its active members fall below a quorum: closed, it holds an election; in leader
    // log replication and in leader ready, its election fails.
    let mut io = Stub::new();
    let mut leader = elector(12);
    let ms = to_closed_leader(&mut watch, &mut leader, &mut io, 1);
    watch.work(&mut leader, &mut io, ms + 10_000);
    assert_eq!(leader.election_state(), ElectionState::Init);
    assert_eq!(leader.role(), Role::Follower);
    let mut io = Stub::new();
    let mut leader = elector(13);
    let ms = to_leader_log_replication(&mut watch, &mut leader, &mut io, 1);
    watch.work(&mut leader, &mut io, ms + 10_000);
    assert_eq!(leader.election_state(), ElectionState::Init);
    assert_eq!(leader.role(), Role::Follower);
    let mut io = Stub::new();
    let mut leader = elector(14);
    let ms = to_leader_ready(&mut watch, &mut leader, &mut io, 1);
    watch.work(&mut leader, &mut io, ms + 10_000);
    assert_eq!(leader.election_state(), ElectionState::Init);
    assert_eq!(leader.role(), Role::Follower);

    // A leader whose election has not closed fails it on a canvass position with a
    // term above its own, in leader log replication and in leader ready.
    let mut io = Stub::new();
    let mut leader = elector(15);
    let ms = to_leader_log_replication(&mut watch, &mut leader, &mut io, 1);
    watch.read(&mut leader, &mut io, ms, &higher_canvass);
    assert_eq!(leader.election_state(), ElectionState::Init);
    assert_eq!(leader.counts().leader_quorum_losses, 0);
    let mut io = Stub::new();
    let mut leader = elector(16);
    let ms = to_leader_ready(&mut watch, &mut leader, &mut io, 1);
    watch.read(&mut leader, &mut io, ms, &higher_canvass);
    assert_eq!(leader.election_state(), ElectionState::Init);
    assert_eq!(leader.counts().leader_quorum_losses, 0);

    // A follower that elects, member 2, its election closed behind member 0, holds an
    // election on member 1's request vote and on member 1's commit position, each of a
    // term above its own.
    let commit_of_another = Message::CommitPosition {
        leadership_term_id: 1,
        log_position: 0,
        leader_member_id: 1,
    };
    for message in [request_vote(1), commit_of_another] {
        let mut io = Stub::new();
        let mut follower = Consensus::with_election(&[0, 1, 2], 2, 17).expect("a member");
        let ms = to_follower_log_await(
            &mut watch,
            &mut follower,
            &mut io,
            1,
            &new_leadership_term(0, 0),
        );
        io.image = Some(0);
        watch.work(&mut follower, &mut io, ms);
        watch.work(&mut follower, &mut io, ms + 1);
        assert_eq!(follower.election_state(), ElectionState::Closed);
        watch.read(&mut follower, &mut io, ms + 2, &message);
        assert_eq!(follower.election_state(), ElectionState::Init);
        assert_eq!(follower.counts().higher_terms_seen, 1);
    }

    for machine in [&watch.election, &watch.role] {
        let unmade = machine.unmade();
        assert!(
            unmade.is_empty(),
            "registered for {} in counters.toml and made by no path of this test: {unmade:?}",
            machine.name
        );
    }
}
