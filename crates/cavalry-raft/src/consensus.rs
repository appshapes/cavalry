//! The core of a cluster member: the state machine of `spec/cluster.md`, "Roles and the
//! election". A member made with [`Consensus::new`] takes the appointed leader's path
//! ("The appointed leader"; ADR 0063), which is what a member over a driver runs. One
//! made with [`Consensus::with_election`] canvasses, nominates itself, wins or loses a
//! ballot, and as the leader holds an election when it loses its quorum or reads a
//! higher term (ADR 0077, ADR 0081, ADR 0082); the rest of the election is built on
//! that path first, in memory, and the module moves to it when its I/O can follow. The
//! core does no I/O of its own. It is given the time and the consensus messages, and
//! reaches everything else through [`Io`]: the module gives it those operations over
//! the client and the `terms` file, and [`crate::memory`] gives it the same operations
//! in memory.
//!
//! One member is one [`Consensus`], driven by one thread: [`Consensus::do_work`] once
//! per duty cycle and [`Consensus::on_message`] for each message of the consensus
//! stream. Neither allocates, locks or blocks, and every table is an array of
//! [`MAX_MEMBERS`].

use cavalry_proto::cluster::{
    CanvassAction, CanvassTerms, ELECTION_STATUS_INTERVAL_NS, ELECTION_TIMEOUT_NS,
    LEADER_HEARTBEAT_INTERVAL_NS, LEADER_HEARTBEAT_TIMEOUT_NS, LogReport, MAX_MEMBERS,
    MemberPosition, Message, PROTOCOL_VERSION, STARTUP_CANVASS_TIMEOUT_NS, TermAction, TermFields,
    TermSpan, TimeUnit, Vote, VoteRequest, appointed_canvass, appointed_leader, ballot, candidate,
    canvass_position_action, commit_position, leader_ready, log_at_least,
    new_leadership_term_action, next_fields, quorum, request_vote,
};
use cavalry_shm::counters::CounterId;
use cavalry_shm::counters::generated::{
    CLUSTER_APPOINTED_LEADER_BEHIND, CLUSTER_BALLOTS_LOST, CLUSTER_CANDIDACIES,
    CLUSTER_ELECTION_STEPS, CLUSTER_ELECTIONS, CLUSTER_ELECTIONS_CLOSED, CLUSTER_ELECTIONS_FAILED,
    CLUSTER_HIGHER_TERMS_SEEN, CLUSTER_JOIN_POSITION_MISMATCHES, CLUSTER_LEADER_HEARTBEAT_TIMEOUTS,
    CLUSTER_LEADER_QUORUM_LOSSES, CLUSTER_LEADER_STEP_DOWNS, CLUSTER_LEADERSHIP_TERMS_IGNORED,
    CLUSTER_LOG_IMAGES_LOST, CLUSTER_LOG_TRUNCATIONS, CLUSTER_TERMS_LED, CLUSTER_VOTES_AGAINST,
    CLUSTER_VOTES_FOR,
};

/// The cluster session id of a session message of no session: one the leader's own
/// process appended (`spec/cluster.md`, "The appointed leader").
pub const NO_SESSION: i64 = -1;

// state machine: cluster_role
/// A member's role, with the value of the `cluster.role` gauge (`spec/cluster.md`,
/// "Counters").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Role {
    /// Records the leader's log and reports its append position.
    Follower = 0,
    /// A follower asking for votes: a member that elects, from its nomination until it
    /// leads, loses its ballot or votes for another. A member whose leader is
    /// appointed is never one.
    Candidate = 1,
    /// Writes the log and commits it.
    Leader = 2,
}

// state machine: cluster_election
/// The states of the election a member of this version enters, each with the code
/// `spec/cluster.md` gives it, the value of the `cluster.election_state` gauge. Every
/// move from one to another adds one to a count of [`Counts`], the counter
/// `counters.toml` registers for it as the machine `cluster_election` (ADR 0067). A
/// member whose leader is appointed never enters nominate or a ballot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ElectionState {
    /// Takes its candidate term, leaves the live log when it had joined one, gives up
    /// the log of a term it led, and canvasses.
    Init = 0,
    /// Reports its log to every other member. The appointed leader leads once canvass
    /// makes it a candidate, every member having reported with no log ahead of its own,
    /// or a quorum at the startup canvass timeout; every other member waits for the
    /// leader's new leadership term.
    Canvass = 1,
    /// A member that elects waits a random delay of up to half the election timeout,
    /// canvassing still, then stores candidate term + 1 and votes for itself.
    Nominate = 2,
    /// A candidate asks every other member for its vote, and leads the term at once
    /// when a quorum has voted for it and every member it has heard is among them, or
    /// at the election timeout with a quorum and no vote against (ADR 0077).
    CandidateBallot = 3,
    /// A member that voted for a candidate waits for that candidate's new leadership
    /// term, for the election timeout at most.
    FollowerBallot = 4,
    /// The leader records its term, adds its log publication and waits for the quorum
    /// position to reach its append position.
    LeaderLogReplication = 5,
    /// The leader replays its own log; nothing in a log with no term before this one.
    LeaderReplay = 6,
    /// The leader joins its log for the term.
    LeaderInit = 7,
    /// The leader announces its term until every member counts for it, or a quorum
    /// once the leader heartbeat timeout has passed in this state, then appends the
    /// term's first entry.
    LeaderReady = 8,
    /// A follower replays its own log; nothing in a log with no term before this one.
    FollowerReplay = 10,
    /// A follower subscribes to the log and arms its recording.
    FollowerLogInit = 14,
    /// A follower waits for the leader's image and checks where it joins.
    FollowerLogAwait = 15,
    /// A follower sends its append position to the leader.
    FollowerReady = 16,
    /// No election runs: the leader leads its term, a follower follows it. A member
    /// that elects holds an election from here, beginning at init, when it reads a term
    /// above its own, and its leader when its active members fall below a quorum.
    Closed = 17,
}

impl ElectionState {
    /// The state's code in `spec/cluster.md`.
    pub const fn code(self) -> u8 {
        self as u8
    }
}

/// The two clocks of one duty cycle, read once by the caller: a monotonic clock for
/// every timeout, and the cluster's time for what the leader writes into the log
/// (`spec/cluster.md`, "Values").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Clock {
    /// Nanoseconds on a clock that never steps back.
    pub now_ns: u64,
    /// Milliseconds since 1970-01-01T00:00:00Z on the member's wall clock.
    pub time_ms: i64,
}

/// The leader's log once it exists: what [`Io::add_log`] returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Log {
    /// The session id of the log publication, which new leadership term carries and a
    /// follower finds the leader's image by.
    pub session_id: i32,
    /// The id of the leader's recording of it, or −1.
    pub recording_id: i64,
}

/// What the core needs done and cannot do itself. Every method returns at once: one
/// that cannot finish now says so, and the core asks again on a later cycle.
///
/// A fault is a step the member cannot go on without, a `terms` file that cannot be
/// written or a command the driver refused: the core returns it from the call that met
/// it and stays in the state it was in.
pub trait Io {
    /// What a failed step reports.
    type Fault;

    /// Sends `message` on the consensus stream to the member with id `member`. `false`
    /// when it was not sent, the publication back-pressured or not connected: every
    /// consensus message is sent again at its interval, so nothing is queued.
    fn send(&mut self, member: i32, message: &Message<'_>) -> bool;

    /// Stores `term` as the candidate term, durably, before the member proposes it
    /// (`spec/cluster.md`, "The log").
    fn store_candidate_term(&mut self, term: i64) -> Result<(), Self::Fault>;

    /// Records `term` with its term base log position in the member's record of terms,
    /// durably, before the member reports an append position for it.
    fn record_term(&mut self, term: i64, base: i64) -> Result<(), Self::Fault>;

    /// The leader's log for `term`: its publication on stream 100 at position `base`
    /// with every other member's `log` endpoint as a destination, and its recording.
    /// `Ok(None)` while a step is still under way; the core calls again each cycle
    /// until it is `Some`, and not after.
    fn add_log(&mut self, term: i64, base: i64) -> Result<Option<Log>, Self::Fault>;

    /// A follower's subscription to the log on its `log` endpoint, with a recording
    /// armed on it, so that the recording holds the leader's image from its join
    /// position.
    fn join_log(&mut self) -> Result<(), Self::Fault>;

    /// The join position of the image of the log with `session_id` on the follower's
    /// subscription: `None` before the image has arrived, and again once it is no
    /// longer available.
    fn log_image(&mut self, session_id: i32) -> Result<Option<i64>, Self::Fault>;

    /// Leaves the log: removes the follower's subscription and stops the recording its
    /// image started. A follower leaves in init, so that the image it joins next is a
    /// new one, checked against its append position where it joins. A recording whose
    /// image joined past the member's append position is not part of its contiguous
    /// log, and [`Io::append_position`] never counts it.
    fn leave_log(&mut self) -> Result<(), Self::Fault>;

    /// Gives the leader's log up: closes the log publication [`Io::add_log`] made and
    /// stops the leader's recording of it, whose stop is where the member's log ends.
    /// A leader that holds an election does it in init, as a follower leaves the log
    /// there, so that the log it leads or joins next is a new one and no follower's
    /// image of the old one goes on (ADR 0082). The core calls it only for a member
    /// that holds a log, and not again once it returned `Ok`.
    fn remove_log(&mut self) -> Result<(), Self::Fault>;

    /// Truncates the member's log to `position`, the base of a term in its leader's
    /// log: every byte its recordings hold above the position is gone, and every term
    /// recorded at a base above it leaves the record of terms, durably
    /// (`spec/cluster.md`, "Messages during an election", new leadership term).
    /// `Ok(Some(term))` once that is done, `term` the highest term the record then
    /// holds, −1 for none, and [`Io::append_position`] is `position` from then on.
    /// `Ok(None)` while a step is still under way: the core does nothing with the
    /// message that asked, and the leader's next asks again. The core calls it only
    /// for a member that is out of the log, with a position below its append position
    /// and at or above the commit position it knows (ADR 0083).
    fn truncate_log(&mut self, position: i64) -> Result<Option<i64>, Self::Fault>;

    /// The member's append position: the end of the contiguous span its recordings of
    /// the log hold from position 0, as a sync has made it durable (ADR 0060). 0 for a
    /// member with no recording.
    fn append_position(&mut self) -> i64;

    /// Appends `entry` to the leader's log. `Ok(false)` when the publication is
    /// back-pressured and nothing was written.
    fn append(&mut self, entry: &Message<'_>) -> Result<bool, Self::Fault>;

    /// The two lowest terms in the member's record of terms above `term`, each with its
    /// term base log position, the lower first; `None` where the record holds no such
    /// term. A leader reads them to tell a member whose log is of `term` which term
    /// its record holds next, where that term begins and where it ends, the base of
    /// the term after it (`spec/cluster.md`, "New leadership term, template 53"). The
    /// record is the one [`Io::record_term`] wrote, read from memory: it returns at
    /// once and cannot fail.
    fn terms_after(&mut self, term: i64) -> [Option<(i64, i64)>; 2];
}

/// Why [`Consensus::new`] refused its members. The members are checked in the order of
/// `spec/cluster.md`, "What a reader rejects", each check over every member, and each
/// variant is one check, named by [`MembersRefused::reason`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MembersRefused {
    /// `members-grammar`: no member was given, as an empty members string is.
    Empty,
    /// `members-grammar`: a member id below 0, which no members string can hold.
    Negative {
        /// The id.
        id: i32,
    },
    /// `members-duplicate`: two members with one id.
    Duplicate {
        /// The id.
        id: i32,
    },
    /// `members-count`: more than [`MAX_MEMBERS`] members.
    Count {
        /// How many were given.
        found: usize,
    },
    /// `members-self`: the member's own id is not among the members.
    NotAMember {
        /// The id.
        id: i32,
    },
}

impl MembersRefused {
    /// The name in `spec/cluster.md`, "What a reader rejects", of the check that
    /// refused the members.
    pub const fn reason(self) -> &'static str {
        match self {
            MembersRefused::Empty | MembersRefused::Negative { .. } => "members-grammar",
            MembersRefused::Duplicate { .. } => "members-duplicate",
            MembersRefused::Count { .. } => "members-count",
            MembersRefused::NotAMember { .. } => "members-self",
        }
    }
}

impl core::fmt::Display for MembersRefused {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let reason = self.reason();
        match *self {
            MembersRefused::Empty => write!(
                f,
                "members refused ({reason}): no member was given; give the members string \
                 of the cluster, 1 to {MAX_MEMBERS} members"
            ),
            MembersRefused::Negative { id } => write!(
                f,
                "members refused ({reason}): member id {id} is below 0; give each member an \
                 id from 0 to 2147483647"
            ),
            MembersRefused::Duplicate { id } => write!(
                f,
                "members refused ({reason}): two members have the id {id}; give each member \
                 its own id"
            ),
            MembersRefused::Count { found } => write!(
                f,
                "members refused ({reason}): {found} members, and a cluster has at most \
                 {MAX_MEMBERS}; give the members string of the cluster"
            ),
            MembersRefused::NotAMember { id } => write!(
                f,
                "members refused ({reason}): member id {id} is not in the members string; \
                 set the member id to one the string holds"
            ),
        }?;
        f.write_str("; see docs/reference/cluster.md#what-a-reader-rejects")
    }
}

impl core::error::Error for MembersRefused {}

/// Why [`Consensus::offer`] appended nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Refused {
    /// This member is not the leader: the leader's process alone appends.
    NotLeader,
    /// The leader's election has not closed: its term's first entry is not in the log
    /// yet. Offer again once [`Consensus::election_state`] is closed.
    Election,
    /// The log publication is back-pressured, a follower or the leader's recorder being
    /// behind. Offer again.
    BackPressured,
}

impl core::fmt::Display for Refused {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let (reason, what) = match self {
            Refused::NotLeader => (
                "not-leader",
                "this member does not lead; offer to the leader, the member whose role is \
                 leader: the member with the lowest id where the leader is appointed",
            ),
            Refused::Election => (
                "election",
                "the leader's election has not closed; offer again once every member has \
                 joined its log, which a member that never comes delays by 70 s",
            ),
            Refused::BackPressured => (
                "back-pressured",
                "the log is back-pressured by a follower or by the recorder; offer again",
            ),
        };
        write!(
            f,
            "offer refused ({reason}): {what}; see docs/reference/cluster.md#roles-and-the-election"
        )
    }
}

impl core::error::Error for Refused {}

/// What a member has counted, each field the counter of `spec/cluster.md`, "Counters",
/// with its name, registered in `counters.toml`. The module stores them in the
/// counters file, by [`Counts::by_counter`]; they only rise.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Counts {
    /// `cluster.elections`: an election begun, from init.
    pub elections: u64,
    /// `cluster.elections_failed`: an election that failed in a state and began again
    /// at init.
    pub elections_failed: u64,
    /// `cluster.elections_closed`: an election that closed, from leader ready or
    /// follower ready (ADR 0067).
    pub elections_closed: u64,
    /// `cluster.election_steps`: a step of an election from one state to the next that
    /// neither begins it, fails it, closes it nor makes the member a leader (ADR 0067).
    pub election_steps: u64,
    /// `cluster.terms_led`: a term the member began to lead.
    pub terms_led: u64,
    /// `cluster.leader_quorum_losses`: a leader whose active members fell below a
    /// quorum. A leader that elects holds an election; the appointed leader stays the
    /// leader, and its commit position stops.
    pub leader_quorum_losses: u64,
    /// `cluster.appointed_leader_behind`: a member the appointed leader found holding a
    /// log ahead of its own, in canvass or in leader ready, once for each such member
    /// in an election.
    pub appointed_leader_behind: u64,
    /// `cluster.leader_heartbeat_timeouts`: a follower that began again at init for
    /// want of its leader's heartbeat.
    pub leader_heartbeat_timeouts: u64,
    /// `cluster.leadership_terms_ignored`: a new leadership term ignored for its term.
    pub leadership_terms_ignored: u64,
    /// `cluster.join_position_mismatches`: an image of the log that joined at a
    /// position other than the member's append position.
    pub join_position_mismatches: u64,
    /// `cluster.log_images_lost`: a follower whose image of the leader's log was no
    /// longer available, so that it began again at init.
    pub log_images_lost: u64,
    /// `cluster.candidacies`: a ballot the member began as a candidate (ADR 0081).
    pub candidacies: u64,
    /// `cluster.ballots_lost`: a ballot that ended with no leader for the member, which
    /// canvasses again: a candidate at the election timeout short of a quorum or with
    /// a vote against, a member in follower ballot whose candidate's new leadership
    /// term did not come within the election timeout, or either that reads a new
    /// leadership term for a log term it did not report. A candidate that follows
    /// another member's new leadership term for its own candidate term has a leader
    /// and counts none (ADR 0081).
    pub ballots_lost: u64,
    /// `cluster.votes_for`: a vote the member sent for a candidate.
    pub votes_for: u64,
    /// `cluster.votes_against`: a vote the member sent against a candidate.
    pub votes_against: u64,
    /// `cluster.leader_step_downs`: a leader whose election had closed that held an
    /// election on a higher term: a canvass position with either term, a request vote
    /// whose candidate term id or a commit position whose leadership term id above its
    /// own term (ADR 0082).
    pub leader_step_downs: u64,
    /// `cluster.higher_terms_seen`: a follower whose election had closed that held an
    /// election on a request vote whose candidate term id or a commit position whose
    /// leadership term id was above its own term (ADR 0082).
    pub higher_terms_seen: u64,
    /// `cluster.log_truncations`: a member that truncated its log to the base of the
    /// term its leader's record holds next after its own, on a new leadership term:
    /// the tail of an older term that no quorum recorded (ADR 0083).
    pub log_truncations: u64,
}

impl Counts {
    /// How many counts a member keeps: one counter of `counters.toml` each.
    pub const LEN: usize = 18;

    /// Each count with the id of its counter in `counters.toml`, the constant
    /// `cavalry-shm` generates from it: what the consensus module stores, each as a
    /// client counter keyed by the member's id (ADR 0062; ADR 0067).
    ///
    /// ```
    /// use cavalry_raft::Counts;
    /// use cavalry_shm::counters::generated::{CLUSTER_ELECTIONS, COUNTERS};
    ///
    /// let counts = Counts { elections: 3, ..Counts::default() };
    /// let pairs = counts.by_counter();
    /// assert!(pairs.contains(&(CLUSTER_ELECTIONS, 3)));
    /// // Every one is a counter `counters.toml` registers.
    /// for (id, _) in pairs {
    ///     assert!(COUNTERS.iter().any(|def| def.id == id));
    /// }
    /// ```
    pub const fn by_counter(&self) -> [(CounterId, u64); Counts::LEN] {
        [
            (CLUSTER_ELECTIONS, self.elections),
            (CLUSTER_ELECTIONS_FAILED, self.elections_failed),
            (CLUSTER_ELECTIONS_CLOSED, self.elections_closed),
            (CLUSTER_ELECTION_STEPS, self.election_steps),
            (CLUSTER_TERMS_LED, self.terms_led),
            (CLUSTER_LEADER_QUORUM_LOSSES, self.leader_quorum_losses),
            (
                CLUSTER_APPOINTED_LEADER_BEHIND,
                self.appointed_leader_behind,
            ),
            (
                CLUSTER_LEADER_HEARTBEAT_TIMEOUTS,
                self.leader_heartbeat_timeouts,
            ),
            (
                CLUSTER_LEADERSHIP_TERMS_IGNORED,
                self.leadership_terms_ignored,
            ),
            (
                CLUSTER_JOIN_POSITION_MISMATCHES,
                self.join_position_mismatches,
            ),
            (CLUSTER_LOG_IMAGES_LOST, self.log_images_lost),
            (CLUSTER_CANDIDACIES, self.candidacies),
            (CLUSTER_BALLOTS_LOST, self.ballots_lost),
            (CLUSTER_VOTES_FOR, self.votes_for),
            (CLUSTER_VOTES_AGAINST, self.votes_against),
            (CLUSTER_LEADER_STEP_DOWNS, self.leader_step_downs),
            (CLUSTER_HIGHER_TERMS_SEEN, self.higher_terms_seen),
            (CLUSTER_LOG_TRUNCATIONS, self.log_truncations),
        ]
    }
}

/// The member a consensus message came from, by the field that names its sender:
/// `None` for a message that names only the member it is for, stop catch-up, and for
/// one that is not a consensus message.
fn sender(message: &Message<'_>) -> Option<i32> {
    match *message {
        Message::CanvassPosition {
            follower_member_id, ..
        }
        | Message::Vote {
            follower_member_id, ..
        }
        | Message::AppendPosition {
            follower_member_id, ..
        }
        | Message::CatchupPosition {
            follower_member_id, ..
        } => Some(follower_member_id),
        Message::RequestVote {
            candidate_member_id,
            ..
        } => Some(candidate_member_id),
        Message::NewLeadershipTerm {
            leader_member_id, ..
        }
        | Message::CommitPosition {
            leader_member_id, ..
        } => Some(leader_member_id),
        _ => None,
    }
}

/// The two lowest terms of `record` above `term`, each with its term base log
/// position, the lower first, and `None` where the record holds no such term: what
/// [`Io::terms_after`] returns, from a record of terms read in any order.
///
/// ```
/// use cavalry_raft::consensus::terms_above;
///
/// let record = [(0, 0), (1, 96), (3, 160)];
/// assert_eq!(terms_above(record, 0), [Some((1, 96)), Some((3, 160))]);
/// assert_eq!(terms_above(record, 1), [Some((3, 160)), None]);
/// assert_eq!(terms_above(record, 3), [None, None]);
/// ```
pub fn terms_above(
    record: impl IntoIterator<Item = (i64, i64)>,
    term: i64,
) -> [Option<(i64, i64)>; 2] {
    let mut lowest: Option<(i64, i64)> = None;
    let mut next: Option<(i64, i64)> = None;
    for recorded in record {
        if recorded.0 <= term {
            continue;
        }
        match lowest {
            Some(held) if recorded.0 == held.0 => {}
            Some(held) if recorded.0 > held.0 => {
                if next.is_none_or(|after| recorded.0 < after.0) {
                    next = Some(recorded);
                }
            }
            _ => {
                next = lowest;
                lowest = Some(recorded);
            }
        }
    }
    [lowest, next]
}

/// One more in a count, which stays at its highest value sooner than it wraps.
fn count(counter: &mut u64) {
    *counter = counter.saturating_add(1);
}

/// One member as the others know it: its last report, the term the report was for and
/// when it arrived.
#[derive(Clone, Copy, Debug)]
struct Peer {
    id: i32,
    reported: bool,
    position: i64,
    /// The log leadership term id the report carried, −1 for none.
    term: i64,
    at_ns: u64,
    /// Whether the last report was an append position, which a member sends once it
    /// has joined its leader's log, and not a canvass position.
    appended: bool,
    /// Whether this election has counted the member as ahead of the appointed leader.
    ahead_counted: bool,
}

/// One cluster member's consensus state: its role, its election, its terms and the
/// positions it knows. See the module's page for how it is driven.
#[derive(Debug)]
pub struct Consensus {
    peers: [Peer; MAX_MEMBERS],
    count: usize,
    /// This member's place in `peers`.
    own: usize,
    /// The member this member leads as or follows. Where the leader is appointed, the
    /// appointed leader's id from the start; where it is elected, −1 from init until
    /// the member leads a term or takes a leader's new leadership term.
    leader_id: i32,
    /// `Some` for a member that elects: the state of the generator it draws its
    /// nomination delays from, never 0. `None` where the leader is appointed.
    election: Option<u64>,
    /// Whether the member's first election since it started has yet to close: until it
    /// has, the canvass deadline is the startup canvass timeout.
    startup: bool,
    /// Whether a consensus message from each member has been read since this member's
    /// election began at init (ADR 0077): the members a candidate waits for.
    heard: [bool; MAX_MEMBERS],
    /// The vote each member cast in this member's ballot, with the term it answers.
    votes: [Option<Vote>; MAX_MEMBERS],
    /// Whether this member's request vote for its candidate term was sent to each.
    requested: [bool; MAX_MEMBERS],
    /// When nominate's delay ends.
    nomination_deadline_ns: u64,
    role: Role,
    state: ElectionState,
    state_since_ns: u64,
    /// The highest term this member has proposed or followed.
    candidate_term: i64,
    /// The highest term in this member's record of terms.
    log_term: i64,
    /// The term this member leads or follows, −1 for none.
    term: i64,
    term_base: i64,
    append_position: i64,
    commit_position: i64,
    counts: Counts,
    // The leader's part.
    log: Option<Log>,
    /// Whether a quorum of members was active at the leader's last cycle.
    quorate: bool,
    last_heartbeat_ns: Option<u64>,
    last_commit_sent: i64,
    // A follower's part.
    leader_log_position: i64,
    log_session_id: i32,
    /// Whether this member has asked for the log: subscribed, with a recording armed.
    joined: bool,
    last_leader_word_ns: u64,
    last_canvass_ns: Option<u64>,
    last_append_sent_ns: Option<u64>,
    last_append_sent: i64,
}

impl Consensus {
    /// A member with id `member_id` among the members with `member_ids`, in init, with
    /// no term and no vote: a member of this version starts on a log that holds
    /// neither (`spec/cluster.md`, "The appointed leader"), which its caller checks.
    pub fn new(member_ids: &[i32], member_id: i32) -> Result<Self, MembersRefused> {
        // `spec/cluster.md`, "What a reader rejects": grammar, duplicate, count and self,
        // in that order, each over every member.
        if member_ids.is_empty() {
            return Err(MembersRefused::Empty);
        }
        if let Some(&id) = member_ids.iter().find(|&&id| id < 0) {
            return Err(MembersRefused::Negative { id });
        }
        for (index, &id) in member_ids.iter().enumerate() {
            if member_ids.iter().take(index).any(|&other| other == id) {
                return Err(MembersRefused::Duplicate { id });
            }
        }
        let count = member_ids.len();
        if count > MAX_MEMBERS {
            return Err(MembersRefused::Count { found: count });
        }
        let mut peers = [Peer {
            id: -1,
            reported: false,
            position: 0,
            term: -1,
            at_ns: 0,
            appended: false,
            ahead_counted: false,
        }; MAX_MEMBERS];
        let mut own = None;
        for (index, (&id, peer)) in member_ids.iter().zip(peers.iter_mut()).enumerate() {
            peer.id = id;
            if id == member_id {
                own = Some(index);
            }
        }
        let Some(own) = own else {
            return Err(MembersRefused::NotAMember { id: member_id });
        };
        Ok(Self {
            peers,
            count,
            own,
            leader_id: appointed_leader(member_ids),
            election: None,
            startup: true,
            heard: [false; MAX_MEMBERS],
            votes: [None; MAX_MEMBERS],
            requested: [false; MAX_MEMBERS],
            nomination_deadline_ns: 0,
            role: Role::Follower,
            state: ElectionState::Init,
            state_since_ns: 0,
            candidate_term: -1,
            log_term: -1,
            term: -1,
            term_base: 0,
            append_position: 0,
            commit_position: 0,
            counts: Counts::default(),
            log: None,
            quorate: true,
            last_heartbeat_ns: None,
            last_commit_sent: -1,
            leader_log_position: 0,
            log_session_id: 0,
            joined: false,
            last_leader_word_ns: 0,
            last_canvass_ns: None,
            last_append_sent_ns: None,
            last_append_sent: -1,
        })
    }

    /// A member with id `member_id` among the members with `member_ids` that elects its
    /// leader (`spec/cluster.md`, "Roles and the election"; ADR 0077), in init, with no
    /// term and no vote. `seed` is where its nomination delays come from: two members
    /// with different seeds draw different delays, and one seed draws the same delays
    /// at every run. Refused as [`Consensus::new`] refuses.
    ///
    /// This path has nominate, the two ballots and the elections a leader holds, on
    /// the loss of its quorum and on a higher term (ADR 0082). It has no truncation and
    /// no catch-up yet, so the consensus module does not use it: see the crate's README
    /// for what a member on it does where the election would do more.
    pub fn with_election(
        member_ids: &[i32],
        member_id: i32,
        seed: u64,
    ) -> Result<Self, MembersRefused> {
        let mut member = Self::new(member_ids, member_id)?;
        // A xorshift generator holds at 0, so a seed of 0 is given a state of its own.
        member.election = Some(if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        });
        member.leader_id = -1;
        Ok(member)
    }

    /// This member's id.
    pub fn member_id(&self) -> i32 {
        self.peer(self.own).id
    }

    /// The id of the member this member leads as or follows. Where the leader is
    /// appointed, the appointed leader's from the start: the lowest among the
    /// members. Where it is elected, −1 from the start of an election until this
    /// member leads a term or takes a leader's new leadership term.
    pub fn leader_member_id(&self) -> i32 {
        self.leader_id
    }

    /// This member's role.
    pub fn role(&self) -> Role {
        self.role
    }

    /// The state of this member's election, closed when none runs.
    pub fn election_state(&self) -> ElectionState {
        self.state
    }

    /// The term this member leads or follows, −1 before the first.
    pub fn leadership_term_id(&self) -> i64 {
        self.term
    }

    /// This member's append position as of its last duty cycle.
    pub fn append_position(&self) -> i64 {
        self.append_position
    }

    /// The commit position this member knows: on the leader the position a quorum has
    /// appended, on a follower the one its leader told it. It never falls.
    pub fn commit_position(&self) -> i64 {
        self.commit_position
    }

    /// What this member has counted.
    pub fn counts(&self) -> Counts {
        self.counts
    }

    /// One duty cycle: what the member's state asks of it at `clock`. Returns how much
    /// it did, 0 for nothing, so that the caller's idle strategy can back off.
    pub fn do_work<I: Io>(&mut self, clock: Clock, io: &mut I) -> Result<usize, I::Fault> {
        self.append_position = self.append_position.max(io.append_position());
        match self.state {
            ElectionState::Init => self.init(clock, io),
            ElectionState::Canvass => self.canvass(clock, io),
            ElectionState::Nominate => self.nominate(clock, io),
            ElectionState::CandidateBallot => self.candidate_ballot(clock, io),
            ElectionState::FollowerBallot => Ok(self.follower_ballot(clock)),
            ElectionState::LeaderLogReplication => self.leader_log_replication(clock, io),
            ElectionState::LeaderReplay => Ok(self.step(ElectionState::LeaderInit, clock)),
            ElectionState::LeaderInit => Ok(self.step(ElectionState::LeaderReady, clock)),
            ElectionState::LeaderReady => self.leader_ready(clock, io),
            ElectionState::FollowerReplay => self.follower_replay(clock),
            ElectionState::FollowerLogInit => self.follower_log_init(clock, io),
            ElectionState::FollowerLogAwait => self.follower_log_await(clock, io),
            ElectionState::FollowerReady => Ok(self.follower_ready(clock, io)),
            ElectionState::Closed => match self.role {
                Role::Leader => Ok(self.commit(clock, io)),
                Role::Follower | Role::Candidate => self.follow(clock, io),
            },
        }
    }

    /// One message of the consensus stream, read at `clock`. A member in init reads
    /// nothing. A member whose leader is appointed ignores request vote and vote, which
    /// no such member sends, and every member ignores catch-up position and stop
    /// catch-up, which this version's members do not send (`spec/cluster.md`, "The
    /// appointed leader"). A member that elects notes the sender of whatever it reads
    /// as heard, for its ballot (ADR 0077). Once its election has closed it holds an
    /// election, beginning at init, on a request vote or a commit position of a term
    /// above its own from another member, and its leader on a canvass position with
    /// either term above its own too (`spec/cluster.md`, "Messages outside an
    /// election"; ADR 0082).
    pub fn on_message<I: Io>(
        &mut self,
        clock: Clock,
        message: &Message<'_>,
        io: &mut I,
    ) -> Result<(), I::Fault> {
        if self.state == ElectionState::Init {
            return Ok(());
        }
        if self.election.is_some()
            && let Some(index) = sender(message).and_then(|id| self.other(id))
            && let Some(heard) = self.heard.get_mut(index)
        {
            *heard = true;
        }
        match *message {
            Message::RequestVote {
                log_leadership_term_id,
                log_position,
                candidate_term_id,
                candidate_member_id,
                ..
            } if self.election.is_some() => self.on_request_vote(
                clock,
                VoteRequest {
                    candidate_term_id,
                    log_leadership_term_id,
                    log_position,
                },
                candidate_member_id,
                io,
            ),
            Message::Vote {
                candidate_term_id,
                candidate_member_id,
                follower_member_id,
                vote,
                ..
            } => {
                // A vote for this member's ballot, kept with the term it answers: the
                // ballot counts the votes for the candidate's current term alone.
                if self.state == ElectionState::CandidateBallot
                    && candidate_member_id == self.member_id()
                    && let Some(index) = self.other(follower_member_id)
                    && let Some(slot) = self.votes.get_mut(index)
                {
                    *slot = Some(Vote {
                        vote,
                        term: candidate_term_id,
                    });
                }
                Ok(())
            }
            Message::CanvassPosition {
                log_leadership_term_id,
                log_position,
                leadership_term_id,
                follower_member_id,
                ..
            } => {
                let Some(index) = self.other(follower_member_id) else {
                    return Ok(());
                };
                self.report(index, log_position, log_leadership_term_id, false, clock);
                if self.role != Role::Leader {
                    return Ok(());
                }
                let action = canvass_position_action(
                    self.term,
                    self.state.code(),
                    CanvassTerms {
                        leadership_term_id,
                        log_leadership_term_id,
                    },
                );
                match action {
                    CanvassAction::Answer => self.announce_to(index, clock, io),
                    // Either term is above this leader's own. A leader that elects
                    // holds an election: its election closed, that is the step-down;
                    // in leader log replication or leader ready, its election fails.
                    CanvassAction::Election if self.election.is_some() => {
                        if self.state == ElectionState::Closed {
                            self.hold_election_on_higher_term(clock);
                        } else {
                            self.fail(clock);
                        }
                    }
                    // An election on a term above its own is not the appointed
                    // leader's to hold: no election can replace it, and no member
                    // beside an appointed leader holds such a term.
                    CanvassAction::Election | CanvassAction::Table => {}
                }
                Ok(())
            }
            Message::AppendPosition {
                leadership_term_id,
                log_position,
                follower_member_id,
                ..
            } => {
                if self.role == Role::Leader
                    && let Some(index) = self.other(follower_member_id)
                {
                    self.report(index, log_position, leadership_term_id, true, clock);
                }
                Ok(())
            }
            Message::NewLeadershipTerm {
                log_leadership_term_id,
                next_leadership_term_id,
                next_term_base_log_position,
                next_log_position,
                leadership_term_id,
                term_base_log_position,
                log_position,
                leader_member_id,
                log_session_id,
                commit_position,
                ..
            } => {
                // A leader reads none: it learns of a later term by a canvass position,
                // a request vote or a commit position, and holds its election on those
                // ("Messages outside an election"). Where the leader is appointed, no
                // other member's is read. A member that elects reads one
                // from another member alone, as it reads no consensus message from an
                // id that is no other member's: it records no term that a host started
                // with another members string announces, nor one in its own name.
                let sender_read = if self.election.is_some() {
                    self.other(leader_member_id).is_some()
                } else {
                    leader_member_id == self.leader_id
                };
                if self.role == Role::Leader || !sender_read {
                    return Ok(());
                }
                let voting = matches!(
                    self.state,
                    ElectionState::Canvass
                        | ElectionState::Nominate
                        | ElectionState::CandidateBallot
                        | ElectionState::FollowerBallot
                );
                if !voting {
                    // Its leader's word while it joins or follows; any other term's,
                    // or another member's, is ignored and counted.
                    if leadership_term_id == self.term && leader_member_id == self.leader_id {
                        self.leader_word(clock, log_position, commit_position);
                    } else {
                        count(&mut self.counts.leadership_terms_ignored);
                    }
                    return Ok(());
                }
                // Nominate is canvass with a delay running: it reads a leader's term as
                // canvass does (ADR 0081).
                let in_canvass =
                    matches!(self.state, ElectionState::Canvass | ElectionState::Nominate);
                let action = new_leadership_term_action(
                    self.log_term,
                    self.append_position,
                    self.candidate_term,
                    in_canvass,
                    TermFields {
                        leadership_term_id,
                        log_leadership_term_id,
                        next_leadership_term_id,
                        next_term_base_log_position,
                        next_log_position,
                        term_base_log_position,
                    },
                );
                // The message is for this member's log term, and names a next term
                // whose base is below this member's append position: the member holds
                // a tail of its term that the leader's log does not, and truncates it
                // first. Then it reads the message with what it holds. A member whose
                // leader is appointed has no truncation, and fails below.
                if action.truncate_to >= 0
                    && self.election.is_some()
                    && !self.truncate(action.truncate_to, clock, io)?
                {
                    return Ok(());
                }
                match action.action {
                    TermAction::Ignore => {
                        count(&mut self.counts.leadership_terms_ignored);
                        Ok(())
                    }
                    // The message is for a log term this member did not report: the
                    // member takes neither the leader nor the term, and canvasses, a
                    // ballot it was in being lost.
                    TermAction::Canvass => {
                        if !in_canvass {
                            self.lose_ballot(clock);
                        }
                        Ok(())
                    }
                    TermAction::Replay if action.truncate_to < 0 || self.election.is_some() => {
                        // The term at its base, recorded before any position is
                        // reported for it; a term already in the record stays as it is.
                        if let Some((term, base)) = action.records
                            && term > self.log_term
                        {
                            io.record_term(term, base)?;
                            self.log_term = term;
                        }
                        self.candidate_term = self.candidate_term.max(leadership_term_id);
                        self.term = leadership_term_id;
                        self.term_base = term_base_log_position;
                        // The sender is this member's leader from here. A candidate
                        // that reads it follows the member that won its candidate
                        // term: its ballot did not end with no leader, so no ballot
                        // is lost (`cluster.ballots_lost`), and its move to follower
                        // is the step to follower replay that `cluster.election_steps`
                        // counts (ADR 0081).
                        self.leader_id = leader_member_id;
                        self.role = Role::Follower;
                        self.log_session_id = log_session_id;
                        self.leader_log_position = log_position;
                        self.leader_word(clock, log_position, commit_position);
                        self.step(ElectionState::FollowerReplay, clock);
                        Ok(())
                    }
                    // A replication, a wait or a log the leader's does not reach, and
                    // where the leader is appointed a truncation: nothing this version
                    // can join with.
                    TermAction::Replay
                    | TermAction::Replicate
                    | TermAction::Wait
                    | TermAction::Fail => {
                        self.fail(clock);
                        Ok(())
                    }
                }
            }
            Message::CommitPosition {
                leadership_term_id,
                log_position,
                leader_member_id,
            } => {
                // A commit position of a term above this member's own, from another
                // member, is a leader the cluster elected without it: its election
                // closed, a member that elects holds an election.
                if self.election.is_some()
                    && self.state == ElectionState::Closed
                    && leadership_term_id > self.term
                    && self.other(leader_member_id).is_some()
                {
                    self.hold_election_on_higher_term(clock);
                    return Ok(());
                }
                if self.role != Role::Leader
                    && self.follows()
                    && leader_member_id == self.leader_id
                    && leadership_term_id == self.term
                {
                    self.leader_word(clock, self.leader_log_position, log_position);
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Appends a session message of no session with `payload` to the log, for the
    /// leader's own process, at the cluster's time `clock` gives (`spec/cluster.md`,
    /// "The appointed leader"). The entry is committed once a quorum has recorded it,
    /// which [`Consensus::commit_position`] says.
    pub fn offer<I: Io>(
        &mut self,
        clock: Clock,
        payload: &[u8],
        io: &mut I,
    ) -> Result<Result<(), Refused>, I::Fault> {
        if self.member_id() != self.leader_id {
            return Ok(Err(Refused::NotLeader));
        }
        if self.role != Role::Leader || self.state != ElectionState::Closed {
            return Ok(Err(Refused::Election));
        }
        let appended = io.append(&Message::SessionMessage {
            leadership_term_id: self.term,
            cluster_session_id: NO_SESSION,
            timestamp: clock.time_ms,
            payload,
        })?;
        Ok(if appended {
            Ok(())
        } else {
            Err(Refused::BackPressured)
        })
    }

    fn peer(&self, index: usize) -> Peer {
        self.peers.get(index).copied().unwrap_or(Peer {
            id: -1,
            reported: false,
            position: 0,
            term: -1,
            at_ns: 0,
            appended: false,
            ahead_counted: false,
        })
    }

    /// The place of another member with `id`, `None` for this member's own id and for
    /// an id no member has.
    fn other(&self, id: i32) -> Option<usize> {
        self.peers
            .iter()
            .take(self.count)
            .position(|peer| peer.id == id)
            .filter(|&index| index != self.own)
    }

    /// A member's report: its log's term and position, when it arrived, and whether it
    /// was an append position.
    fn report(&mut self, index: usize, position: i64, term: i64, appended: bool, clock: Clock) {
        if let Some(peer) = self.peers.get_mut(index) {
            peer.reported = true;
            peer.position = position;
            peer.term = term;
            peer.appended = appended;
            peer.at_ns = clock.now_ns;
        }
    }

    /// Moves the election to `state`. Every caller counts the move, in the count
    /// `counters.toml` registers for it (`cluster_election`).
    fn enter(&mut self, state: ElectionState, clock: Clock) -> usize {
        self.state = state;
        self.state_since_ns = clock.now_ns;
        1
    }

    /// A step of the election to `state`, the next on its way to closed, counted in
    /// `cluster.election_steps`.
    fn step(&mut self, state: ElectionState, clock: Clock) -> usize {
        count(&mut self.counts.election_steps);
        self.enter(state, clock)
    }

    /// The election closed, from leader ready or follower ready, counted in
    /// `cluster.elections_closed`.
    fn close(&mut self, clock: Clock) -> usize {
        count(&mut self.counts.elections_closed);
        self.startup = false;
        self.enter(ElectionState::Closed, clock)
    }

    /// Whether this member has taken a leader: in a follower's states after canvass.
    fn follows(&self) -> bool {
        matches!(
            self.state,
            ElectionState::FollowerReplay
                | ElectionState::FollowerLogInit
                | ElectionState::FollowerLogAwait
                | ElectionState::FollowerReady
                | ElectionState::Closed
        )
    }

    fn leader_word(&mut self, clock: Clock, log_position: i64, commit_position: i64) {
        self.last_leader_word_ns = clock.now_ns;
        self.leader_log_position = self.leader_log_position.max(log_position);
        self.commit_position = self.commit_position.max(commit_position);
    }

    /// An election that failed in its state: counted, and begun again at init. A
    /// candidate's fails when it reads the new leadership term of its candidate term
    /// and could join it only with a truncation, a replication or a wait. A leader's,
    /// where it elects, fails in leader log replication or leader ready, on a canvass
    /// position with a term above its own or when its active members fall below a
    /// quorum; init gives its log up. Either is a follower from here, the moves
    /// `candidate -> follower` and `leader -> follower` that `cluster.elections_failed`
    /// counts, so that the role never reads candidate or leader while the election
    /// reads init. The appointed leader fails no election.
    fn fail(&mut self, clock: Clock) {
        count(&mut self.counts.elections_failed);
        self.role = Role::Follower;
        self.enter(ElectionState::Init, clock);
    }

    /// Truncates this member's log to `position`, the base of the term its leader's
    /// record holds next after this member's own, which a new leadership term named
    /// (`spec/cluster.md`, "Messages during an election"; ADR 0083). Returns whether
    /// the member goes on with that message. It does not while the truncation is under
    /// way, and it does not when the position is below the commit position it knows:
    /// a committed entry is never truncated, so no leader that holds what was
    /// committed names such a base, and the member refuses and fails its election.
    /// Once truncated, its append position is the position and its log term the
    /// highest its record still holds, before anything is recorded or reported.
    fn truncate<I: Io>(
        &mut self,
        position: i64,
        clock: Clock,
        io: &mut I,
    ) -> Result<bool, I::Fault> {
        if position < self.commit_position {
            self.fail(clock);
            return Ok(false);
        }
        let Some(log_term) = io.truncate_log(position)? else {
            return Ok(false);
        };
        self.append_position = position;
        self.log_term = log_term;
        count(&mut self.counts.log_truncations);
        Ok(true)
    }

    /// A member that elects and whose election had closed holds an election on a term
    /// above its own, beginning at init (`spec/cluster.md`, "Messages outside an
    /// election"): counted in `cluster.leader_step_downs` when it led, Raft's
    /// step-down, and in `cluster.higher_terms_seen` when it followed (ADR 0082). A
    /// leader is a follower from this call on, and init gives its log up.
    fn hold_election_on_higher_term(&mut self, clock: Clock) {
        count(if self.role == Role::Leader {
            &mut self.counts.leader_step_downs
        } else {
            &mut self.counts.higher_terms_seen
        });
        self.role = Role::Follower;
        self.enter(ElectionState::Init, clock);
    }

    /// A ballot that ended with no leader, this member's own or its candidate's, at the
    /// election timeout or on a new leadership term for a log term it did not report:
    /// counted, and the member canvasses again as a follower, with the reports and the
    /// members heard that it has.
    fn lose_ballot(&mut self, clock: Clock) -> usize {
        count(&mut self.counts.ballots_lost);
        self.role = Role::Follower;
        self.enter(ElectionState::Canvass, clock)
    }

    /// The next value of the member's generator, a xorshift: what a nomination delay is
    /// drawn from. 0 for a member whose leader is appointed, which draws none.
    fn next_random(&mut self) -> u64 {
        let Some(mut state) = self.election else {
            return 0;
        };
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        self.election = Some(state);
        state
    }

    /// A request vote, read by a member that elects (`spec/cluster.md`, "Messages
    /// during an election" and, for a member whose election has closed, "Messages
    /// outside an election"). The candidate term the request raises is stored before
    /// the vote is sent, so that a restart between the two loses the vote and never
    /// the term; a vote for that is sent moves the member to follower ballot, to wait
    /// for that candidate.
    fn on_request_vote<I: Io>(
        &mut self,
        clock: Clock,
        request: VoteRequest,
        candidate_member_id: i32,
        io: &mut I,
    ) -> Result<(), I::Fault> {
        if self.other(candidate_member_id).is_none() {
            return Ok(());
        }
        // Its election closed, a member answers no request vote and keeps its candidate
        // term. One for a term above its own is a ballot the cluster holds without
        // it, and it holds an election ("Messages outside an election").
        if self.state == ElectionState::Closed {
            if request.candidate_term_id > self.term {
                self.hold_election_on_higher_term(clock);
            }
            return Ok(());
        }
        let answer = request_vote(
            self.candidate_term,
            LogReport {
                term: self.log_term,
                position: self.append_position,
            },
            self.state.code(),
            request,
        );
        if answer.candidate_term != self.candidate_term {
            io.store_candidate_term(answer.candidate_term)?;
            self.candidate_term = answer.candidate_term;
        }
        let Some(vote) = answer.vote else {
            return Ok(());
        };
        let sent = io.send(
            candidate_member_id,
            &Message::Vote {
                candidate_term_id: request.candidate_term_id,
                log_leadership_term_id: self.log_term,
                log_position: self.append_position,
                candidate_member_id,
                follower_member_id: self.member_id(),
                vote,
            },
        );
        if sent {
            count(if vote {
                &mut self.counts.votes_for
            } else {
                &mut self.counts.votes_against
            });
        }
        // A vote for that left moves the member to follower ballot, to wait for this
        // candidate. One that did not leave moves nothing and counts nothing: the term
        // is stored all the same, so the member votes for no other candidate in it,
        // and it waits for no one, since no candidate has its vote.
        if vote && sent {
            self.role = Role::Follower;
            self.enter(ElectionState::FollowerBallot, clock);
        }
        Ok(())
    }

    fn due(last: Option<u64>, interval_ns: u64, clock: Clock) -> bool {
        last.is_none_or(|last| clock.now_ns.saturating_sub(last) >= interval_ns)
    }

    /// State 0. The member takes its candidate term, leaves the live log when it had
    /// joined one, gives up the log of a term it led, and canvasses with no report
    /// held from the election before; a cluster of one member leads at once. A fault at
    /// either step leaves it in init, and the next cycle tries the step again.
    fn init<I: Io>(&mut self, clock: Clock, io: &mut I) -> Result<usize, I::Fault> {
        // Out of the live log, so that the image joined next is a new one whose join
        // position follower log await checks.
        if self.joined {
            io.leave_log()?;
            self.joined = false;
        }
        // A leader that began again holds the log publication of the term it led. It
        // gives it up, so that no follower's image of it goes on and the term it leads
        // next has a publication of its own, at that term's base (ADR 0082).
        if self.log.is_some() {
            io.remove_log()?;
            self.log = None;
        }
        self.candidate_term = self.candidate_term.max(self.log_term);
        for peer in self.peers.iter_mut().take(self.count) {
            peer.reported = false;
            peer.ahead_counted = false;
        }
        // A member that elects begins with no member heard, no vote held and no
        // leader taken.
        if self.election.is_some() {
            self.heard = [false; MAX_MEMBERS];
            self.votes = [None; MAX_MEMBERS];
            self.requested = [false; MAX_MEMBERS];
            self.leader_id = -1;
        }
        // A cluster of one member leads from init, with nobody to canvass. A fault
        // at its writes leaves it in init with nothing counted.
        if self.count == 1 {
            let work = self.lead(clock, io)?;
            count(&mut self.counts.elections);
            return Ok(work);
        }
        // The canvass interval runs on from the last election: a member whose
        // elections fail one after another, one that cannot join without the
        // catch-up, still sends at most one round every election status interval.
        count(&mut self.counts.elections);
        self.role = Role::Follower;
        Ok(self.enter(ElectionState::Canvass, clock))
    }

    /// State 1. Canvass position to every other member, every election status
    /// interval. The appointed leader leads once canvass's rule makes it a candidate,
    /// every member having reported, or a quorum at the startup canvass timeout, and
    /// no member that has reported holds a log ahead of its own, whatever the deadline.
    /// So an appointed leader whose log is behind a member's, one that lost its log
    /// directory, never leads, and each member ahead of it is counted.
    fn canvass<I: Io>(&mut self, clock: Clock, io: &mut I) -> Result<usize, I::Fault> {
        let work = self.send_canvass_position(clock, io);
        if self.election.is_some() {
            return Ok(work.max(self.nominate_when_a_candidate(clock)));
        }
        if self.member_id() != self.leader_id {
            return Ok(work);
        }
        let own = self.own_report();
        let reports = self.reports(own);
        // A member of this version starts with no term, so its canvass deadline is the
        // startup canvass timeout after the state began.
        let deadline_passed =
            clock.now_ns.saturating_sub(self.state_since_ns) >= STARTUP_CANVASS_TIMEOUT_NS;
        self.count_ahead(own);
        if !appointed_canvass(self.count, &reports, own, deadline_passed).leads {
            return Ok(work);
        }
        self.lead(clock, io)
    }

    /// Canvass position to every other member, once every election status interval:
    /// what canvass and nominate both send.
    fn send_canvass_position<I: Io>(&mut self, clock: Clock, io: &mut I) -> usize {
        if !Self::due(self.last_canvass_ns, ELECTION_STATUS_INTERVAL_NS, clock) {
            return 0;
        }
        self.last_canvass_ns = Some(clock.now_ns);
        let message = Message::CanvassPosition {
            log_leadership_term_id: self.log_term,
            log_position: self.append_position,
            leadership_term_id: self.candidate_term,
            follower_member_id: self.member_id(),
            protocol_version: PROTOCOL_VERSION,
        };
        for index in 0..self.count {
            if index != self.own {
                io.send(self.peer(index).id, &message);
            }
        }
        1
    }

    /// This member's own log, as it reports it.
    fn own_report(&self) -> LogReport {
        LogReport {
            term: self.log_term,
            position: self.append_position,
        }
    }

    /// The logs the members have reported in this election, this member's own among
    /// them: `None` for a member that has not reported.
    fn reports(&self, own: LogReport) -> [Option<LogReport>; MAX_MEMBERS] {
        let mut reports = [None; MAX_MEMBERS];
        for (index, (slot, peer)) in reports.iter_mut().zip(self.peers.iter()).enumerate() {
            if index == self.own {
                *slot = Some(own);
            } else if peer.reported {
                *slot = Some(LogReport {
                    term: peer.term,
                    position: peer.position,
                });
            }
        }
        reports
    }

    /// State 1 for a member that elects: it goes to nominate at once when every member
    /// has reported and none holds a log ahead of its own, or once the canvass
    /// deadline has passed when the members that have reported with a log not ahead
    /// of its own, itself among them, make a quorum. The deadline is the startup
    /// canvass timeout after the state began until the member's first election has
    /// closed, and the leader heartbeat timeout after the last word of a leader from
    /// then on, which has passed when a follower's election begins for want of that
    /// word. The delay nominate waits is drawn here, up to half the election timeout.
    fn nominate_when_a_candidate(&mut self, clock: Clock) -> usize {
        let deadline_passed = if self.startup {
            clock.now_ns.saturating_sub(self.state_since_ns) >= STARTUP_CANVASS_TIMEOUT_NS
        } else {
            clock.now_ns.saturating_sub(self.last_leader_word_ns) >= LEADER_HEARTBEAT_TIMEOUT_NS
        };
        let own = self.own_report();
        if !candidate(self.count, &self.reports(own), own, deadline_passed).nominate {
            return 0;
        }
        let delay_ns = self
            .next_random()
            .checked_rem(ELECTION_TIMEOUT_NS / 2)
            .unwrap_or(0);
        self.nomination_deadline_ns = clock.now_ns.saturating_add(delay_ns);
        self.step(ElectionState::Nominate, clock)
    }

    /// State 2. Canvass position goes on; when the delay has ended the member takes
    /// candidate term + 1, stores it before it sends anything, votes for itself and
    /// asks for the others' votes in candidate ballot. A fault at the store leaves it
    /// here, and the next cycle stores the same term again.
    fn nominate<I: Io>(&mut self, clock: Clock, io: &mut I) -> Result<usize, I::Fault> {
        let work = self.send_canvass_position(clock, io);
        if clock.now_ns < self.nomination_deadline_ns {
            return Ok(work);
        }
        let term = self.candidate_term.saturating_add(1);
        io.store_candidate_term(term)?;
        self.candidate_term = term;
        self.votes = [None; MAX_MEMBERS];
        self.requested = [false; MAX_MEMBERS];
        if let Some(own) = self.votes.get_mut(self.own) {
            *own = Some(Vote { vote: true, term });
        }
        count(&mut self.counts.candidacies);
        self.role = Role::Candidate;
        Ok(self.enter(ElectionState::CandidateBallot, clock))
    }

    /// State 3. Request vote for the term the candidate nominated itself for to every
    /// member not yet sent one, then the ballot: the candidate leads its candidate term
    /// at once when the votes for that term, its own among them, make a quorum and every
    /// member it has heard has cast one of them, or at the election timeout with a
    /// quorum for and none against (ADR 0077). At the election timeout with no such
    /// ballot it canvasses again.
    fn candidate_ballot<I: Io>(&mut self, clock: Clock, io: &mut I) -> Result<usize, I::Fault> {
        let mut work = 0;
        // A candidate whose term rose during its ballot, by a request vote for a
        // higher term from a log behind its own, holds no vote for that term, its own
        // included (`spec/cluster.md`, state 3). It asks no member for a vote in it,
        // which would spend that member's one vote in the term on a candidate that
        // cannot win it, and no votes cast in it make it the leader.
        let own_vote = self.holds_own_vote();
        let request = Message::RequestVote {
            log_leadership_term_id: self.log_term,
            log_position: self.append_position,
            candidate_term_id: self.candidate_term,
            candidate_member_id: self.member_id(),
            protocol_version: PROTOCOL_VERSION,
        };
        for index in 0..self.count {
            let unsent = self.requested.get(index).is_some_and(|sent| !sent);
            if own_vote && index != self.own && unsent && io.send(self.peer(index).id, &request) {
                if let Some(sent) = self.requested.get_mut(index) {
                    *sent = true;
                }
                work = 1;
            }
        }
        let timeout_passed =
            clock.now_ns.saturating_sub(self.state_since_ns) >= ELECTION_TIMEOUT_NS;
        // `decided_at_once` is the vectors' `unanimous`: every member heard voted for.
        let outcome = ballot(
            self.count,
            self.candidate_term,
            &self.votes,
            &self.heard,
            timeout_passed,
        );
        if outcome.leader && own_vote {
            return self.lead_elected(clock, io);
        }
        if timeout_passed {
            return Ok(self.lose_ballot(clock));
        }
        Ok(work)
    }

    /// Whether this member, a candidate, holds its own vote for its candidate term: from
    /// nominate, until a request vote for a higher term raises that term during its
    /// ballot.
    fn holds_own_vote(&self) -> bool {
        self.votes
            .get(self.own)
            .copied()
            .flatten()
            .is_some_and(|own| own.vote && own.term == self.candidate_term)
    }

    /// The winner of a ballot leads its candidate term, which it stored when it
    /// nominated itself: the term is recorded at its append position, the term's base,
    /// before anything in memory moves. A fault at the write leaves the member in
    /// candidate ballot, and the next cycle records the same term again.
    fn lead_elected<I: Io>(&mut self, clock: Clock, io: &mut I) -> Result<usize, I::Fault> {
        io.record_term(self.candidate_term, self.append_position)?;
        self.log_term = self.candidate_term;
        self.term = self.candidate_term;
        self.term_base = self.append_position;
        self.leader_id = self.member_id();
        // Init gave up the log of any term this member led before, so `log` is `None`
        // and leader log replication adds this term's.
        count(&mut self.counts.terms_led);
        self.role = Role::Leader;
        self.quorate = true;
        Ok(self.enter(ElectionState::LeaderLogReplication, clock))
    }

    /// State 4. The member voted for a candidate and waits for its new leadership term,
    /// which [`Consensus::on_message`] reads; at the election timeout it canvasses
    /// again.
    fn follower_ballot(&mut self, clock: Clock) -> usize {
        if clock.now_ns.saturating_sub(self.state_since_ns) >= ELECTION_TIMEOUT_NS {
            return self.lose_ballot(clock);
        }
        0
    }

    /// Leads candidate term + 1, where an elected member would nominate itself and
    /// win: the term it proposes is stored, and recorded at its append position, before
    /// anything in memory moves. A fault at either write leaves the member in the
    /// state it was in, and the next cycle proposes the same term again.
    fn lead<I: Io>(&mut self, clock: Clock, io: &mut I) -> Result<usize, I::Fault> {
        let term = self.candidate_term.saturating_add(1);
        io.store_candidate_term(term)?;
        io.record_term(term, self.append_position)?;
        self.candidate_term = term;
        self.log_term = term;
        self.term = term;
        self.term_base = self.append_position;
        self.leader_id = self.member_id();
        // No log is held here, as in `lead_elected`: an appointed leader leads once,
        // from canvass or, alone, from init, and has added none before.
        count(&mut self.counts.terms_led);
        self.role = Role::Leader;
        self.quorate = true;
        Ok(self.enter(ElectionState::LeaderLogReplication, clock))
    }

    /// Counts each member whose last report holds a log ahead of `own`, the appointed
    /// leader's, once in an election, and returns how many there are now: such a member
    /// holds a history the leader lost, and the leader neither leads nor closes its
    /// election while one reports so.
    fn count_ahead(&mut self, own: LogReport) -> usize {
        let mut ahead = 0;
        let mut newly = 0;
        for (index, peer) in self.peers.iter_mut().take(self.count).enumerate() {
            if index == self.own
                || !peer.reported
                || log_at_least(own.term, own.position, peer.term, peer.position)
            {
                continue;
            }
            ahead += 1;
            if !peer.ahead_counted {
                peer.ahead_counted = true;
                newly += 1;
            }
        }
        for _ in 0..newly {
            count(&mut self.counts.appointed_leader_behind);
        }
        ahead
    }

    /// Counts the leader's loss of its quorum, once each time its active members, those
    /// whose last report is younger than the leader heartbeat timeout, itself among
    /// them, fall below one, and returns whether they fell on this cycle.
    fn watch_quorum(&mut self, clock: Clock) -> bool {
        let active = self
            .peers
            .iter()
            .take(self.count)
            .filter(|peer| {
                peer.reported
                    && clock.now_ns.saturating_sub(peer.at_ns) < LEADER_HEARTBEAT_TIMEOUT_NS
            })
            .count();
        let quorate = active >= quorum(self.count);
        let lost = self.quorate && !quorate;
        if lost {
            count(&mut self.counts.leader_quorum_losses);
        }
        self.quorate = quorate;
        lost
    }

    /// The leader's own place in its table, set on every cycle, and its quorum
    /// watched. Returns whether a leader that elects lost its quorum on this cycle, and
    /// so holds an election (`spec/cluster.md`, "The commit position"); the appointed
    /// leader stays the leader whatever it lost, and this is `false` for it.
    fn report_own(&mut self, clock: Clock) -> bool {
        self.report(self.own, self.append_position, self.term, true, clock);
        self.watch_quorum(clock) && self.election.is_some()
    }

    /// The members' positions as the commit position counts them: one for each member,
    /// a member that has not reported as one that counts for nothing.
    fn positions(&self, clock: Clock) -> [MemberPosition; MAX_MEMBERS] {
        let mut positions = [MemberPosition {
            position: 0,
            age_ns: u64::MAX,
            term: -1,
        }; MAX_MEMBERS];
        for (slot, peer) in positions.iter_mut().zip(self.peers.iter()).take(self.count) {
            if peer.reported {
                *slot = MemberPosition {
                    position: peer.position,
                    age_ns: clock.now_ns.saturating_sub(peer.at_ns),
                    term: peer.term,
                };
            }
        }
        positions
    }

    /// New leadership term to the member at `index`, for the term it reported.
    fn announce_to<I: Io>(&mut self, index: usize, clock: Clock, io: &mut I) {
        let Some(log) = self.log else {
            return;
        };
        let peer = self.peer(index);
        // The lowest term this leader's record holds above the one the member
        // reports, with its base and its end, the base of the term after it. The
        // member truncates what it holds above that base, so the term named must be
        // the next one the record holds and never a later one: a member told a later
        // term keeps a tail of its own between the two bases and joins on it. An
        // appointed leader's record holds its own term alone, which is then the next
        // of any other, with no end.
        let [next, after] = io.terms_after(peer.term);
        let span = next.map(|(term, base)| TermSpan {
            term,
            base,
            end: after.map_or(-1, |(_, base)| base),
        });
        let next = next_fields(span.as_slice(), peer.term, self.term, self.term_base);
        io.send(
            peer.id,
            &Message::NewLeadershipTerm {
                log_leadership_term_id: peer.term,
                next_leadership_term_id: next.next_leadership_term_id,
                next_term_base_log_position: next.next_term_base_log_position,
                next_log_position: next.next_log_position,
                leadership_term_id: self.term,
                term_base_log_position: self.term_base,
                log_position: self.append_position,
                leader_recording_id: log.recording_id,
                timestamp: clock.time_ms,
                leader_member_id: self.member_id(),
                log_session_id: log.session_id,
                app_version: 0,
                is_startup: true,
                commit_position: self.commit_position,
            },
        );
    }

    fn send_commit_position<I: Io>(&mut self, io: &mut I) {
        let message = Message::CommitPosition {
            leadership_term_id: self.term,
            log_position: self.commit_position,
            leader_member_id: self.member_id(),
        };
        for index in 0..self.count {
            if index != self.own {
                io.send(self.peer(index).id, &message);
            }
        }
        self.last_commit_sent = self.commit_position;
    }

    /// The leader's heartbeat during its election: new leadership term and commit
    /// position to every other member, every leader heartbeat interval.
    fn announce<I: Io>(&mut self, clock: Clock, io: &mut I) -> usize {
        if !Self::due(self.last_heartbeat_ns, LEADER_HEARTBEAT_INTERVAL_NS, clock) {
            return 0;
        }
        self.last_heartbeat_ns = Some(clock.now_ns);
        for index in 0..self.count {
            if index != self.own {
                self.announce_to(index, clock, io);
            }
        }
        self.send_commit_position(io);
        1
    }

    /// State 5. The log publication at the term's base, then the heartbeat, until the
    /// quorum position has reached the append position. A leader that elects fails its
    /// election when its active members fall below a quorum.
    fn leader_log_replication<I: Io>(
        &mut self,
        clock: Clock,
        io: &mut I,
    ) -> Result<usize, I::Fault> {
        if self.log.is_none() {
            self.log = io.add_log(self.term, self.term_base)?;
            return Ok(1);
        }
        if self.report_own(clock) {
            self.fail(clock);
            return Ok(1);
        }
        let work = self.announce(clock, io);
        let quorum_position = commit_position(
            self.positions(clock).get(..self.count).unwrap_or(&[]),
            self.append_position,
            self.term,
            self.term_base,
            LEADER_HEARTBEAT_TIMEOUT_NS,
        )
        .quorum_position;
        if quorum_position >= self.append_position {
            return Ok(self.step(ElectionState::LeaderReplay, clock));
        }
        Ok(work)
    }

    /// State 8. The heartbeat until every member the leader has heard counts for the
    /// term at the leader's log position, or a quorum once the leader heartbeat timeout
    /// has passed in this state, and no member's last report holds a log ahead of the
    /// leader's; then the term's first entry, the new leadership term event. A member
    /// is heard once it has reported since the canvass began, in a canvass position or
    /// an append position; it counts by an append position alone, which it sends once
    /// it has joined the log. A leader that elects fails its election when its active
    /// members fall below a quorum, as in state 5.
    fn leader_ready<I: Io>(&mut self, clock: Clock, io: &mut I) -> Result<usize, I::Fault> {
        if self.report_own(clock) {
            self.fail(clock);
            return Ok(1);
        }
        let work = self.announce(clock, io);
        let members = || self.peers.iter().take(self.count);
        let heard = members().filter(|peer| peer.reported).count();
        let counted = members()
            .filter(|peer| {
                // At the leader's log position and no other: a position past it is of
                // entries this leader's log does not hold.
                peer.reported
                    && peer.appended
                    && peer.term == self.term
                    && peer.position == self.append_position
                    && clock.now_ns.saturating_sub(peer.at_ns) < LEADER_HEARTBEAT_TIMEOUT_NS
            })
            .count();
        let deadline_passed =
            clock.now_ns.saturating_sub(self.state_since_ns) >= LEADER_HEARTBEAT_TIMEOUT_NS;
        let ahead = self.count_ahead(LogReport {
            term: self.term,
            position: self.append_position,
        });
        if !leader_ready(self.count, heard, counted, ahead, deadline_passed) {
            return Ok(work);
        }
        let Some(log) = self.log else {
            return Ok(work);
        };
        let appended = io.append(&Message::NewLeadershipTermEvent {
            leadership_term_id: self.term,
            log_position: self.term_base,
            timestamp: clock.time_ms,
            term_base_log_position: self.term_base,
            leader_member_id: self.member_id(),
            log_session_id: log.session_id,
            time_unit: TimeUnit::Millis,
            app_version: 0,
        })?;
        if !appended {
            return Ok(work);
        }
        Ok(self.close(clock))
    }

    /// The leader, its election closed: the commit position from the members' append
    /// positions, sent when it rises and every leader heartbeat interval. A leader that
    /// elects and whose active members fall below a quorum holds an election, beginning
    /// at init, a follower from that cycle on; the appointed leader stays the leader,
    /// its commit position where it was.
    fn commit<I: Io>(&mut self, clock: Clock, io: &mut I) -> usize {
        if self.report_own(clock) {
            // Counted in `cluster.leader_quorum_losses` by the quorum's watch.
            self.role = Role::Follower;
            return self.enter(ElectionState::Init, clock);
        }
        let commit = commit_position(
            self.positions(clock).get(..self.count).unwrap_or(&[]),
            self.append_position,
            self.term,
            self.term_base,
            LEADER_HEARTBEAT_TIMEOUT_NS,
        )
        .commit_position;
        self.commit_position = self.commit_position.max(commit);
        if self.commit_position > self.last_commit_sent
            || Self::due(self.last_heartbeat_ns, LEADER_HEARTBEAT_INTERVAL_NS, clock)
        {
            self.last_heartbeat_ns = Some(clock.now_ns);
            self.send_commit_position(io);
            return 1;
        }
        0
    }

    /// State 10. Nothing to replay in a log with no term before the leader's. A member
    /// behind the leader's log position would catch up, which this version cannot.
    fn follower_replay<F>(&mut self, clock: Clock) -> Result<usize, F> {
        if self.append_position < self.leader_log_position {
            self.fail(clock);
            return Ok(1);
        }
        Ok(self.step(ElectionState::FollowerLogInit, clock))
    }

    /// State 14. The log subscription and its armed recording. No member of this
    /// version is already subscribed here: that is the catch-up's live add, state 13.
    fn follower_log_init<I: Io>(&mut self, clock: Clock, io: &mut I) -> Result<usize, I::Fault> {
        io.join_log()?;
        self.joined = true;
        Ok(self.step(ElectionState::FollowerLogAwait, clock))
    }

    /// State 15. The leader's image, which must join at this member's append position.
    fn follower_log_await<I: Io>(&mut self, clock: Clock, io: &mut I) -> Result<usize, I::Fault> {
        match io.log_image(self.log_session_id)? {
            Some(join_position) if join_position == self.append_position => {
                Ok(self.step(ElectionState::FollowerReady, clock))
            }
            Some(_) => {
                // Init leaves the log this image is on.
                count(&mut self.counts.join_position_mismatches);
                self.fail(clock);
                Ok(1)
            }
            None => {
                if clock.now_ns.saturating_sub(self.state_since_ns) >= LEADER_HEARTBEAT_TIMEOUT_NS {
                    self.fail(clock);
                    return Ok(1);
                }
                Ok(0)
            }
        }
    }

    fn send_append_position<I: Io>(&mut self, clock: Clock, io: &mut I) -> bool {
        let sent = io.send(
            self.leader_id,
            &Message::AppendPosition {
                leadership_term_id: self.log_term,
                log_position: self.append_position,
                follower_member_id: self.member_id(),
                flags: 0,
            },
        );
        if sent {
            self.last_append_sent = self.append_position;
            self.last_append_sent_ns = Some(clock.now_ns);
        }
        sent
    }

    /// State 16. The append position to the leader, which closes the election.
    fn follower_ready<I: Io>(&mut self, clock: Clock, io: &mut I) -> usize {
        if self.send_append_position(clock, io) {
            self.last_leader_word_ns = clock.now_ns;
            return self.close(clock);
        }
        if clock.now_ns.saturating_sub(self.state_since_ns) >= LEADER_HEARTBEAT_TIMEOUT_NS {
            self.fail(clock);
            return 1;
        }
        0
    }

    /// A follower, its election closed: its append position to the leader on every
    /// change and every leader heartbeat interval, and an election begun again when
    /// the leader has said nothing for the leader heartbeat timeout or its image of
    /// the log is no longer available.
    fn follow<I: Io>(&mut self, clock: Clock, io: &mut I) -> Result<usize, I::Fault> {
        if clock.now_ns.saturating_sub(self.last_leader_word_ns) >= LEADER_HEARTBEAT_TIMEOUT_NS {
            count(&mut self.counts.leader_heartbeat_timeouts);
            return Ok(self.enter(ElectionState::Init, clock));
        }
        if io.log_image(self.log_session_id)?.is_none() {
            count(&mut self.counts.log_images_lost);
            return Ok(self.enter(ElectionState::Init, clock));
        }
        if self.append_position != self.last_append_sent
            || Self::due(
                self.last_append_sent_ns,
                LEADER_HEARTBEAT_INTERVAL_NS,
                clock,
            )
        {
            return Ok(usize::from(self.send_append_position(clock, io)));
        }
        Ok(0)
    }
}
