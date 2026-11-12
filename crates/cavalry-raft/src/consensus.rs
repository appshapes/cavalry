//! The core of a cluster member: the state machine of `spec/cluster.md`, "Roles and the
//! election". A member made with [`Consensus::new`] takes the appointed leader's path
//! ("The appointed leader"; ADR 0063), which is what a member over a driver runs. One
//! made with [`Consensus::with_election`] canvasses, nominates itself, wins or loses a
//! ballot, and as the leader holds an election when it loses its quorum or reads a
//! higher term and serves a member that replicates a term it missed with replays of its
//! own recordings, and as a member below its leader's term base replicates such a term
//! (ADR 0077, ADR 0081, ADR 0082, ADR 0085, ADR 0086); the rest of the election is
//! built on that path first, in memory, and the module moves to it when its I/O can
//! follow. The core does no I/O of its own. It is given the time and the consensus
//! messages, and reaches everything else through [`Io`]: the module gives it those
//! operations over the client and the `terms` file, and [`crate::memory`] gives it the
//! same operations in memory.
//!
//! One member is one [`Consensus`], driven by one thread: [`Consensus::do_work`] once
//! per duty cycle and [`Consensus::on_message`] for each message of the consensus
//! stream. Neither allocates, locks or blocks, and every table is an array of
//! [`MAX_MEMBERS`].

use cavalry_proto::cluster::{
    APPEND_POSITION_FLAG_CATCHUP, CanvassAction, CanvassTerms, ELECTION_STATUS_INTERVAL_NS,
    ELECTION_TIMEOUT_NS, LEADER_HEARTBEAT_INTERVAL_NS, LEADER_HEARTBEAT_TIMEOUT_NS,
    LIVE_ADD_MAX_WINDOW, LogReport, MAX_MEMBERS, MemberPosition, Message, PROTOCOL_VERSION,
    STARTUP_CANVASS_TIMEOUT_NS, TermAction, TermFields, TermSpan, TimeUnit, Vote, VoteRequest,
    appointed_canvass, appointed_leader, ballot, candidate, canvass_position_action,
    commit_position, leader_ready, log_at_least, new_leadership_term_action, next_fields, quorum,
    request_vote,
};
use cavalry_shm::counters::CounterId;
use cavalry_shm::counters::generated::{
    CLUSTER_APPOINTED_LEADER_BEHIND, CLUSTER_BALLOTS_LOST, CLUSTER_CANDIDACIES, CLUSTER_CATCHUPS,
    CLUSTER_ELECTION_STEPS, CLUSTER_ELECTIONS, CLUSTER_ELECTIONS_CLOSED, CLUSTER_ELECTIONS_FAILED,
    CLUSTER_HIGHER_TERMS_SEEN, CLUSTER_JOIN_POSITION_MISMATCHES, CLUSTER_LEADER_HEARTBEAT_TIMEOUTS,
    CLUSTER_LEADER_QUORUM_LOSSES, CLUSTER_LEADER_STEP_DOWNS, CLUSTER_LEADERSHIP_TERMS_IGNORED,
    CLUSTER_LOG_IMAGES_LOST, CLUSTER_LOG_REPLICATIONS, CLUSTER_LOG_REPLICATIONS_UNAVAILABLE,
    CLUSTER_LOG_TRUNCATIONS, CLUSTER_TERMS_DROPPED, CLUSTER_TERMS_LED, CLUSTER_TRUNCATIONS_REFUSED,
    CLUSTER_VOTES_AGAINST, CLUSTER_VOTES_FOR,
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
/// member whose leader is appointed never enters nominate, a ballot or follower log
/// replication.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ElectionState {
    /// Takes its candidate term, leaves the live log when it had joined one, gives up
    /// the log of a term it led, stops the replays it served, and canvasses.
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
    /// at the election timeout with a quorum and no vote against (ADR 0077). It
    /// canvasses again at once on a vote against, which no ballot survives (ADR 0105).
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
    /// The leader announces its term until a quorum counts for it, then appends the
    /// term's first entry. The appointed leader waits for every member it has heard,
    /// or for a quorum once the leader heartbeat timeout has passed in this state.
    LeaderReady = 8,
    /// A member below its leader's term base replicates one term it lacks from the
    /// leader's recordings, to that term's end, and canvasses again (ADR 0086).
    FollowerLogReplication = 9,
    /// A follower replays its own log; nothing in a log with no term before this one.
    FollowerReplay = 10,
    /// A follower behind its leader's log within the leader's term subscribes on its
    /// catch-up endpoint and asks the leader for the log from its append position
    /// (ADR 0088).
    FollowerCatchupInit = 11,
    /// It waits for the image of the leader's replay, which must join at its append
    /// position.
    FollowerCatchupAwait = 12,
    /// It records the replay, adds the live log once it is near the leader's
    /// position, and goes on to join when the leader has sent stop catch-up and it
    /// has reached the live log and the commit position it had been told when it read
    /// stop catch-up.
    FollowerCatchup = 13,
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

/// Why a member's last election began, with the value of the `cluster.election_cause`
/// gauge (`spec/cluster.md`, "Counters"; ADR 0077, ADR 0102): the election the member
/// is in, or the one that closed last. A member whose leader is appointed never has
/// [`ElectionCause::QuorumLost`] or [`ElectionCause::HigherTerm`]: its leader stays
/// the leader below a quorum, and it holds no election on a term above its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ElectionCause {
    /// The member started.
    Started = 0,
    /// It followed, and its leader said nothing for the leader heartbeat timeout.
    LeaderHeartbeatTimeout = 1,
    /// It followed, and its image of the leader's log was no longer available.
    LogImageLost = 2,
    /// It led, and its active members fell below a quorum.
    QuorumLost = 3,
    /// Its election had closed, and a message carried a term above its own.
    HigherTerm = 4,
    /// Its election before failed.
    ElectionFailed = 5,
}

impl ElectionCause {
    /// The cause's code, the value of the `cluster.election_cause` gauge.
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

/// How a replay the leader started ended: what [`Io::replay_stopped`] returns, the
/// driver's `replay stopped` event (`spec/client-protocol.md`, "Events").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ReplayStopped {
    /// The position after the last frame the replay wrote, where its image ends.
    pub position: i64,
    /// Whether the replay ended by itself, at the end it was given or at its
    /// recording's stop position, the event's reason 0. `false` for one that was
    /// stopped, found no subscriber or failed, reasons 1 to 3.
    pub ended: bool,
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

    /// The candidate term the member's log directory holds, −1 when none was stored:
    /// what [`Io::store_candidate_term`] wrote last, before this start or since. A
    /// member that elects reads it once, in its first init, so that it votes once in a
    /// term across a restart (`spec/cluster.md`, "The log"; ADR 0089). Read from
    /// memory: it returns at once and cannot fail.
    fn candidate_term(&mut self) -> i64;

    /// The highest term in the member's record of terms, its log leadership term id,
    /// −1 for a record with none: what [`Io::record_term`] wrote, before this start or
    /// since. Read once with [`Io::candidate_term`], and from memory as it is.
    fn log_leadership_term_id(&mut self) -> i64;

    /// The leader's log for `term`: its publication on stream 100 at position `base`
    /// with every other member's `log` endpoint as a destination, and its recording.
    /// `Ok(None)` while a step is still under way; the core calls again each cycle
    /// until it is `Some`, and not after.
    fn add_log(&mut self, term: i64, base: i64) -> Result<Option<Log>, Self::Fault>;

    /// A follower's subscription to the log on its `log` endpoint, with a recording
    /// armed on it, so that the recording holds the leader's image from its join
    /// position when the image arrives after the recording is armed. An image that
    /// arrived first is recorded from what it had received by then, and
    /// [`Io::log_image`] says from where (ADR 0093).
    fn join_log(&mut self) -> Result<(), Self::Fault>;

    /// The join position of the image of the log with `session_id` as the member
    /// holds it: where the member's recording of that image begins, which is the
    /// image's join position when the recording was armed first and past it when
    /// the image arrived first (ADR 0093). `None` before the image has arrived and
    /// its recording has started, and again once the image is no longer available.
    /// A recording counts in [`Io::append_position`] no sooner than this has
    /// reported its image, and one of another session's image never does.
    fn log_image(&mut self, session_id: i32) -> Result<Option<i64>, Self::Fault>;

    /// Leaves the log: removes the follower's subscription and stops the recording its
    /// image started. A follower leaves in init, so that the image it joins next is a
    /// new one, checked against its append position where it joins. A recording whose
    /// image joined past the member's append position is part of its contiguous log
    /// only once that log has reached where the recording begins, which a catch-up's
    /// replay brings about (ADR 0094): until then, and for ever when nothing brings
    /// it there, [`Io::append_position`] does not count it.
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

    /// The highest term in the member's record of terms at or below `term`, −1 when
    /// the record holds none. A leader reads it to answer a member for a term its own
    /// record holds, and not for one the member alone recorded (`spec/cluster.md`,
    /// "New leadership term, template 53"; ADR 0096). Read from memory, as
    /// [`Io::terms_after`] is.
    fn term_at_or_below(&mut self, term: i64) -> i64;

    /// Drops from the member's record of terms every term above `term`, durably, and
    /// returns the highest term the record then holds, −1 for none: the terms its
    /// leader's record lacks (`spec/cluster.md`, "Messages during an election"; ADR
    /// 0096). They are named by term and not by base, since the lowest of them is
    /// based where the member's log was cut, or ends. The core calls it for a member
    /// that is out of the log, after it has truncated what it held above the base of
    /// the lowest of them.
    fn drop_terms_above(&mut self, term: i64) -> Result<i64, Self::Fault>;

    /// The lowest term base log position in the member's record of terms above
    /// `position`, where the term that holds `position` ends in this member's log:
    /// `None` when the record holds no term based above it. A leader reads it for a
    /// replication's replays, which end there (`spec/cluster.md`, "Messages outside an
    /// election"). Read from memory, as [`Io::terms_after`] is.
    fn term_base_above(&mut self, position: i64) -> Option<i64>;

    /// Starts a replay of this member's own recordings of the log, on stream 100, to
    /// the `catchup` endpoint the members string gives the member with id `member`:
    /// from `position`, in the recording that begins at or below it and ends above
    /// it, the one that begins first where two do. With `end`, a replication's, it
    /// runs to `end` or to that recording's end, whichever is lower. With none, a
    /// catch-up's, it is of the recording of the term this member leads and follows
    /// it as it grows, until it is stopped (ADR 0087). `Ok(false)` when no recording
    /// of this member's holds `position`, purged by retention: nothing is started.
    /// `Ok(true)` once the replay is asked for: it runs from then until
    /// [`Io::replay_stopped`] says how it ended, and how any earlier replay to the
    /// member ended is forgotten. The core holds at most one replay a member (ADR
    /// 0085), and calls this for a member it holds none for. One case is the I/O's
    /// to order: a member that led, asked a replay to stop and held an election does
    /// not wait in init for that replay to have stopped, and may lead again and call
    /// this for the same member while the driver's `replay stopped` is still to
    /// come. The I/O then starts the new replay once the old one has stopped, as the
    /// page has a leader keep a request that arrives in that window.
    fn start_replay(
        &mut self,
        member: i32,
        position: i64,
        end: Option<i64>,
    ) -> Result<bool, Self::Fault>;

    /// Asks the replay running to `member` to stop, the driver's `stop replay`. The
    /// replay runs on until [`Io::replay_stopped`] says it stopped, unless the core
    /// stopped it in init, where it asks no more about it.
    fn stop_replay(&mut self, member: i32) -> Result<(), Self::Fault>;

    /// How the replay last started to `member` ended: `None` while it runs, and how
    /// it ended once, on the first call after it stopped.
    fn replay_stopped(&mut self, member: i32) -> Result<Option<ReplayStopped>, Self::Fault>;

    /// A member's subscription to stream 100 on its own `catchup` endpoint, with a
    /// recording armed on it, so that the recording of the replay's image holds it
    /// from its join position (`spec/cluster.md`, state 9). One recording is armed
    /// for one image: the core calls this before each catch-up position it sends,
    /// once for each replay of a replication (ADR 0092) and once for a catch-up,
    /// with no catch-up subscription held.
    fn add_catchup(&mut self) -> Result<(), Self::Fault>;

    /// Sends catch-up position to the member with id `leader`, for its term `term`,
    /// asking for the log from `position`, with this member's own `catchup`
    /// endpoint, which the core does not hold. `false` when it was not sent, as
    /// [`Io::send`] says; the core sends it once from a position, and again on a
    /// later cycle while it has not left.
    fn send_catchup_position(&mut self, leader: i32, term: i64, position: i64) -> bool;

    /// The join position of the image the member records on the catch-up
    /// subscription, the one that took the recording [`Io::add_catchup`] armed:
    /// `None` before that image has arrived, and again once it has gone, its replay
    /// ended or stopped, though never before it was reported once, so that a replay
    /// short enough to come and go between two duty cycles is still checked (ADR
    /// 0094). What an image carries counts in [`Io::append_position`] no sooner than
    /// this reports the image, so that the core checks where an image joined before
    /// it reports a position the image gave it.
    fn catchup_image(&mut self) -> Result<Option<i64>, Self::Fault>;

    /// Removes the catch-up subscription and stops the recording its image started,
    /// whose stop is where the member's log ends. The core calls it when a
    /// replication has reached its stop, when a replay's image has gone below it
    /// (ADR 0092), and in init for one that failed.
    fn remove_catchup(&mut self) -> Result<(), Self::Fault>;
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
    /// `cluster.join_position_mismatches`: an image of the live log that joined at a
    /// position other than the member's append position, in follower log await,
    /// which over a driver is a recording of it that begins elsewhere (ADR 0093);
    /// or, in follower log replication, a replay's image that joined at a position
    /// other than the one the member asked from (ADR 0086, ADR 0092); or, in
    /// follower catch-up await, the image of a catch-up's replay that joined at a
    /// position other than the one the member asked from (ADR 0088).
    pub join_position_mismatches: u64,
    /// `cluster.log_images_lost`: a follower whose image of the leader's log was no
    /// longer available, so that it began again at init.
    pub log_images_lost: u64,
    /// `cluster.candidacies`: a ballot the member began as a candidate (ADR 0081).
    pub candidacies: u64,
    /// `cluster.ballots_lost`: a ballot that ended with no leader for the member, which
    /// canvasses again: a candidate on a vote against its candidate term or once it
    /// holds no vote of its own for that term, at once (ADR 0105), or at the election
    /// timeout short of a quorum, a member in follower ballot whose candidate's new
    /// leadership term did not come within the election timeout, or either that reads
    /// a new leadership term for a log term that is not its own, once it has dropped
    /// any terms the message says its leader's record lacks (ADR 0096). A candidate
    /// that follows another member's new leadership term
    /// for its own candidate term has a leader and counts none (ADR 0081).
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
    /// `cluster.log_truncations`: a member that truncated its log on a new leadership
    /// term, to the base of the term its leader's record holds next after its own, the
    /// tail of an older term that no quorum recorded (ADR 0083); or, before it dropped
    /// the terms its leader's record lacks, to the base of the lowest of them, the
    /// drop counted in `terms_dropped` (ADR 0096).
    pub log_truncations: u64,
    /// `cluster.truncations_refused`: a member told to truncate below the commit
    /// position it knows, which it did not do: it failed its election and kept its
    /// log (ADR 0083). No leader that holds what was committed asks it.
    pub truncations_refused: u64,
    /// `cluster.log_replications`: on a member, a catch-up position it sent from
    /// follower log replication, one for each replay it asked for (ADR 0086, ADR
    /// 0092); on the leader, a replay started for such a member, one for each replay
    /// where the span lies in several recordings (ADR 0085).
    pub log_replications: u64,
    /// `cluster.log_replications_unavailable`: a member in follower log replication
    /// whose append position stayed below its stop position without moving for the
    /// leader heartbeat timeout, so that its election failed (ADR 0086).
    pub log_replications_unavailable: u64,
    /// `cluster.catchups`: on the leader, a replay started for a member that catches
    /// up within the leader's term (ADR 0087).
    pub catchups: u64,
    /// `cluster.terms_dropped`: a member that dropped from its record the terms its
    /// leader's record lacks, on a new leadership term that named a term below its
    /// own log term (ADR 0096).
    pub terms_dropped: u64,
}

impl Counts {
    /// How many counts a member keeps: one counter of `counters.toml` each.
    pub const LEN: usize = 23;

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
            (CLUSTER_TRUNCATIONS_REFUSED, self.truncations_refused),
            (CLUSTER_LOG_REPLICATIONS, self.log_replications),
            (
                CLUSTER_LOG_REPLICATIONS_UNAVAILABLE,
                self.log_replications_unavailable,
            ),
            (CLUSTER_CATCHUPS, self.catchups),
            (CLUSTER_TERMS_DROPPED, self.terms_dropped),
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

/// The highest term of `record` at or below `term`, −1 where the record holds none:
/// what [`Io::term_at_or_below`] returns, from a record of terms read in any order.
/// The rule is the page's, and `cavalry_proto::cluster::answered_term` is the copy of
/// it that the `answered-term` vectors pin (`spec/cluster.md`, "New leadership term,
/// template 53"; ADR 0096): this is the same rule over a member's record as its
/// `Io` holds it, and a change to one is a change to both.
///
/// ```
/// use cavalry_raft::consensus::term_at_or_below;
///
/// let record = [(0, 0), (1, 96), (3, 160)];
/// assert_eq!(term_at_or_below(record, 3), 3);
/// assert_eq!(term_at_or_below(record, 2), 1);
/// assert_eq!(term_at_or_below(record, -1), -1);
/// ```
pub fn term_at_or_below(record: impl IntoIterator<Item = (i64, i64)>, term: i64) -> i64 {
    record
        .into_iter()
        .map(|(recorded, _)| recorded)
        .filter(|&recorded| recorded <= term)
        .max()
        .unwrap_or(-1)
}

/// The lowest term base log position of `record` above `position`, `None` where the
/// record holds no term based above it: what [`Io::term_base_above`] returns, from a
/// record of terms read in any order. It is where the term that holds `position`
/// ends, `cavalry_proto::cluster::replay_end`'s end position.
///
/// ```
/// use cavalry_raft::consensus::base_above;
///
/// let record = [(0, 0), (1, 96), (3, 160)];
/// assert_eq!(base_above(record, 0), Some(96));
/// assert_eq!(base_above(record, 95), Some(96));
/// assert_eq!(base_above(record, 96), Some(160));
/// assert_eq!(base_above(record, 160), None);
/// ```
pub fn base_above(record: impl IntoIterator<Item = (i64, i64)>, position: i64) -> Option<i64> {
    record
        .into_iter()
        .map(|(_, base)| base)
        .filter(|&base| base > position)
        .min()
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

/// What a leader holds of one member that is behind: the replays of its own recordings
/// that it serves the member with, one at a time and one for each catch-up position
/// the member sends, for a replication (state 9) or for a catch-up within its term
/// (state 11) (`spec/cluster.md`, "Messages outside an election"; ADR 0085, ADR 0087,
/// ADR 0092).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Serving {
    /// No replay runs to the member and none is owed.
    Idle,
    /// A replay runs to the member. A replication's ends at the first term base log
    /// position above the position the member asked from, or at its recording's end
    /// where that comes first. A catch-up's has no end: it follows the leader's live
    /// recording until the leader stops it. However it ends, nothing follows it but
    /// the member's next request (ADR 0092).
    Running,
    /// The leader asked the replay to stop and waits for it to have stopped. `next`
    /// is the newest request the member has sent since: its position, and its end.
    Stopping { next: Option<(i64, Option<i64>)> },
    /// A replay is owed from `position`, and begins once the member's reported
    /// append position has reached it, so that the member's image of it joins at its
    /// append position. The member asked from there and the start faulted: the
    /// request is kept this way until a start succeeds.
    Next { position: i64, end: Option<i64> },
}

/// A member's replication of one term (state 9): from the new leadership term that
/// began it until the member canvasses again or its election fails.
#[derive(Clone, Copy, Debug)]
struct Replication {
    /// Where it stops: the end of the term replicated, the base of the term after it
    /// in the leader's record.
    stop: i64,
    /// The term the member begins at its base, with that base, until the member has
    /// recorded it: a term it holds none of yet. `None` where the term replicated is
    /// the member's own log term, which its record holds already.
    begins: Option<(i64, i64)>,
    /// Whether a catch-up position has left whose replay the member waits for or
    /// records: from the message leaving until that replay's image has gone.
    asked: bool,
    /// The position the member last asked from, `None` before its first catch-up
    /// position has left: where the image of the replay must join, and a position
    /// the member does not ask from a second time (ADR 0092).
    asked_from: Option<i64>,
    /// The join position of the image last checked, of the replay last asked for.
    image: Option<i64>,
    /// The append position at the last cycle, and when it last moved.
    position: i64,
    moved_ns: u64,
    /// When the append position was first seen at the stop.
    reached_ns: Option<u64>,
}

/// A member's catch-up within its leader's term (states 12 and 13): from the catch-up
/// position that began it until the member joins the live log or its election fails.
#[derive(Clone, Copy, Debug)]
struct Catchup {
    /// The position the member asked the log from, where the replay's image must
    /// join.
    asked: i64,
    /// Whether the member has reported with the catch-up flag: it holds the log to
    /// where its live image joined, and the leader may stop its replay. Only then is
    /// a stop catch-up the leader's answer to this catch-up.
    flagged: bool,
    /// Whether the leader's stop catch-up was read.
    stopped: bool,
    /// The commit position the member had been told when it read stop catch-up,
    /// which its append position must reach before it leaves the state. It is fixed
    /// then: a leader that goes on committing does not move it.
    commit_target: i64,
    /// The append position at the last cycle, and when it last moved.
    position: i64,
    moved_ns: u64,
}

/// How near the leader's position a member that catches up adds the live log: the
/// lesser of a quarter of the log's term length and [`LIVE_ADD_MAX_WINDOW`]
/// (`spec/cluster.md`, "Values").
const LIVE_ADD_WINDOW: i64 = {
    let quarter = (cavalry_proto::udp::UDP_TERM_LENGTH / 4) as i64;
    if quarter < LIVE_ADD_MAX_WINDOW {
        quarter
    } else {
        LIVE_ADD_MAX_WINDOW
    }
};

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
    /// Whether a member that elects has read what its log directory held when it
    /// started: its candidate term and the highest term of its record (ADR 0089).
    read_back: bool,
    /// Whether the term this member leads is the term of its first election since it
    /// started: what its new leadership term carries as `is startup`.
    led_from_start: bool,
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
    /// What this leader serves each member that replicates, by the member's place.
    serving: [Serving; MAX_MEMBERS],
    // A follower's part.
    leader_log_position: i64,
    log_session_id: i32,
    /// Whether this member has asked for the log: subscribed, with a recording armed.
    joined: bool,
    last_leader_word_ns: u64,
    last_canvass_ns: Option<u64>,
    last_append_sent_ns: Option<u64>,
    last_append_sent: i64,
    /// The replication this member is in, in follower log replication.
    replication: Option<Replication>,
    /// Whether this member holds its catch-up subscription.
    catchup: bool,
    /// The catch-up this member is in, in follower catch-up await and follower
    /// catch-up.
    catching: Option<Catchup>,
    /// Why this member's last election began: set where an election begins at init.
    cause: ElectionCause,
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
            read_back: false,
            led_from_start: false,
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
            serving: [Serving::Idle; MAX_MEMBERS],
            leader_log_position: 0,
            log_session_id: 0,
            joined: false,
            last_leader_word_ns: 0,
            last_canvass_ns: None,
            last_append_sent_ns: None,
            last_append_sent: -1,
            replication: None,
            catchup: false,
            catching: None,
            cause: ElectionCause::Started,
        })
    }

    /// A member with id `member_id` among the members with `member_ids` that elects its
    /// leader (`spec/cluster.md`, "Roles and the election"; ADR 0077), in init. Its
    /// first duty cycle reads what its log directory holds, through
    /// [`Io::candidate_term`] and [`Io::log_leadership_term_id`]: none on a new
    /// directory, and on one the member kept the term it canvasses with and the term
    /// it will not vote in a second time (ADR 0089). `seed` is where its nomination
    /// delays come from: two members with different seeds draw different delays, and
    /// one seed draws the same delays at every run. Refused as [`Consensus::new`]
    /// refuses.
    ///
    /// This path has nominate, the two ballots, the elections a leader holds, on the
    /// loss of its quorum and on a higher term (ADR 0082), the truncation (ADR 0083),
    /// the replication of a term a member missed (ADR 0085, ADR 0086) and the catch-up
    /// within a leader's term (ADR 0087, ADR 0088), the leader's side of each and the
    /// member's, and a member's start on the log it kept (ADR 0089). The consensus
    /// module does not use it yet: see the crate's README for
    /// what a member on it does where the election would do more, and for what the
    /// module's `Io` refuses.
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

    /// What the `cluster.leader_member_id` gauge holds (`spec/cluster.md`,
    /// "Counters"; ADR 0102): the id of the member this member leads as or follows,
    /// and −1 while it has none, in init, canvass, nominate and the two ballots.
    /// Where the leader is appointed its id is known from the start, and a member
    /// that has neither led nor taken its new leadership term still has no leader.
    pub fn leader_gauge(&self) -> i64 {
        match self.state {
            ElectionState::Init
            | ElectionState::Canvass
            | ElectionState::Nominate
            | ElectionState::CandidateBallot
            | ElectionState::FollowerBallot => -1,
            _ => i64::from(self.leader_id),
        }
    }

    /// Why this member's last election began, the one it is in or the one that
    /// closed last: the value of the `cluster.election_cause` gauge.
    pub fn election_cause(&self) -> ElectionCause {
        self.cause
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
    /// it did, 0 for nothing, so that the caller's idle strategy can back off. A
    /// leader that elects looks after the replays it serves after its state's own
    /// work, so a fault of the state's work is returned before one of a replay, and
    /// a replay's fault is returned once the state's work has run.
    pub fn do_work<I: Io>(&mut self, clock: Clock, io: &mut I) -> Result<usize, I::Fault> {
        self.append_position = self.append_position.max(io.append_position());
        let stepped = match self.state {
            ElectionState::Init => self.init(clock, io),
            ElectionState::Canvass => self.canvass(clock, io),
            ElectionState::Nominate => self.nominate(clock, io),
            ElectionState::CandidateBallot => self.candidate_ballot(clock, io),
            ElectionState::FollowerBallot => Ok(self.follower_ballot(clock)),
            ElectionState::LeaderLogReplication => self.leader_log_replication(clock, io),
            ElectionState::LeaderReplay => Ok(self.step(ElectionState::LeaderInit, clock)),
            ElectionState::LeaderInit => Ok(self.step(ElectionState::LeaderReady, clock)),
            ElectionState::LeaderReady => self.leader_ready(clock, io),
            ElectionState::FollowerLogReplication => self.follower_log_replication(clock, io),
            ElectionState::FollowerReplay => self.follower_replay(clock),
            ElectionState::FollowerCatchupInit => self.follower_catchup_init(clock, io),
            ElectionState::FollowerCatchupAwait => self.follower_catchup_await(clock, io),
            ElectionState::FollowerCatchup => self.follower_catchup(clock, io),
            ElectionState::FollowerLogInit => self.follower_log_init(clock, io),
            ElectionState::FollowerLogAwait => self.follower_log_await(clock, io),
            ElectionState::FollowerReady => Ok(self.follower_ready(clock, io)),
            ElectionState::Closed => match self.role {
                Role::Leader => Ok(self.commit(clock, io)),
                Role::Follower | Role::Candidate => self.follow(clock, io),
            },
        };
        // A leader that elects looks after the replays it serves, in whichever of its
        // states it is: one that ended, one it stopped, one that is due. It does so
        // after its state's own work, so that a fault of a replay costs the cycle no
        // heartbeat, no commit position and no watch of the quorum: the fault is
        // returned once both have run, the state's own first.
        let served = if self.role == Role::Leader && self.election.is_some() {
            self.serve_replays(io)
        } else {
            Ok(0)
        };
        let work = stepped?;
        Ok(work.max(served?))
    }

    /// One message of the consensus stream, read at `clock`. A member in init reads
    /// nothing. A member whose leader is appointed ignores request vote and vote, which
    /// no such member sends, catch-up position and stop catch-up (`spec/cluster.md`,
    /// "The appointed leader"). A leader that elects answers a catch-up position at or
    /// below its log position with a replay of its own recordings, stops the replay it
    /// runs to a member that canvasses again, and stops a catch-up, with stop
    /// catch-up, when the member it serves reports with the catch-up flag, on the live
    /// log ("Messages outside an election"; ADR 0085, ADR 0087, ADR 0088). A member
    /// that elects, in follower catch-up, takes its leader's stop catch-up once it has
    /// reported with that flag, and removes its catch-up subscription (ADR 0088). A
    /// member that elects and reads a new leadership term
    /// which leaves it below its leader's term base goes to follower log replication
    /// (ADR 0086). A member that elects notes the sender of whatever it reads
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
                // A member that canvasses is in no replication: first the replay this
                // leader runs to it is stopped and the next it owed it dropped, so
                // that the member joins no stale replay when it asks again.
                if self.election.is_some() {
                    self.end_serving(index, io)?;
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
                flags,
            } => {
                if self.role == Role::Leader
                    && let Some(index) = self.other(follower_member_id)
                {
                    self.report(index, log_position, leadership_term_id, true, clock);
                    // A report with the catch-up flag, for this leader's term: the
                    // member is on the live log. It holds every position below where
                    // its live image joined, so the replay brings it nothing more,
                    // and the catch-up is over at whatever position it reports (ADR
                    // 0088).
                    if self.election.is_some()
                        && flags & APPEND_POSITION_FLAG_CATCHUP != 0
                        && leadership_term_id == self.term
                    {
                        self.stop_catchup(index, io)?;
                    }
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
                // The base of the lowest term in this member's own record above the
                // one the message names, read where the message names a term below
                // this member's log term: the leader's record holds none of them.
                let own_base_above = if log_leadership_term_id < self.log_term {
                    io.terms_after(log_leadership_term_id)[0].map_or(-1, |(_, base)| base)
                } else {
                    -1
                };
                let action = new_leadership_term_action(
                    self.log_term,
                    self.append_position,
                    self.candidate_term,
                    in_canvass,
                    own_base_above,
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
                    // The message is for a log term that is not this member's, and
                    // says nothing of the terms this member holds: the member takes
                    // neither the leader nor the term, and canvasses, a ballot it
                    // was in being lost.
                    TermAction::Canvass => {
                        if !in_canvass {
                            self.lose_ballot(clock);
                        }
                        Ok(())
                    }
                    // The leader's record holds none of this member's terms above
                    // the one named, and the message answers the log this member
                    // holds. What it held above the base of the lowest of them is cut
                    // by now, under the guard of a truncation; the terms leave its
                    // record, and it reads the message again with the log it then
                    // holds: once, since its log term is then at or below the term
                    // named (ADR 0096). Below it, when the member never recorded the
                    // term named: the second read canvasses. A drop that faults
                    // leaves the log cut and the record holding some of those terms
                    // or all of them, with `log_term` as it was: the member reports
                    // it again, the leader answers again, and the drop, which changes
                    // nothing already dropped, runs again.
                    TermAction::Drop if self.election.is_some() => {
                        self.log_term = io.drop_terms_above(log_leadership_term_id)?;
                        count(&mut self.counts.terms_dropped);
                        // A record that still holds a term above the one named was
                        // not cut: the member fails its election and reads nothing
                        // twice.
                        if self.log_term > log_leadership_term_id {
                            self.fail(clock);
                            return Ok(());
                        }
                        self.on_message(clock, message, io)
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
                    // Below the leader's term base, with a stop position: the member
                    // replicates one term from the leader's recordings (state 9). With
                    // its append position below the next term's base that term is its
                    // own log term, which its record holds. With its append position at
                    // that base it begins the next term, of which it holds nothing, and
                    // state 9 records that term at that base before it asks for
                    // anything (ADR 0086). The sender is its leader from here, as in
                    // the step to follower replay.
                    TermAction::Replicate if self.election.is_some() => {
                        let begins = (self.append_position == next_term_base_log_position)
                            .then_some((next_leadership_term_id, next_term_base_log_position));
                        self.candidate_term = self.candidate_term.max(leadership_term_id);
                        self.term = leadership_term_id;
                        self.term_base = term_base_log_position;
                        self.leader_id = leader_member_id;
                        self.role = Role::Follower;
                        self.log_session_id = log_session_id;
                        self.leader_log_position = log_position;
                        self.leader_word(clock, log_position, commit_position);
                        self.replication = Some(Replication {
                            stop: action.stop_position,
                            begins,
                            asked: false,
                            asked_from: None,
                            image: None,
                            position: self.append_position,
                            moved_ns: clock.now_ns,
                            reached_ns: None,
                        });
                        // Its first cycle in the state reports its position.
                        self.last_append_sent = -1;
                        self.last_append_sent_ns = None;
                        self.step(ElectionState::FollowerLogReplication, clock);
                        Ok(())
                    }
                    // At the next term's base with no end carried: the member waits
                    // for a later new leadership term, where it is. No leader of this
                    // version sends it: a next term that is not the leader's own ends
                    // at a base the leader's record holds.
                    TermAction::Wait if self.election.is_some() => Ok(()),
                    // A log the leader's does not reach, and where the leader is
                    // appointed a truncation, a replication or a wait: nothing the
                    // member can join with.
                    TermAction::Drop
                    | TermAction::Replay
                    | TermAction::Replicate
                    | TermAction::Wait
                    | TermAction::Fail => {
                        self.fail(clock);
                        Ok(())
                    }
                }
            }
            Message::CatchupPosition {
                leadership_term_id,
                log_position,
                follower_member_id,
                ..
            } => {
                // A leader that elects reads it, for its own term, from another
                // member. The endpoint the message carries is the one the members
                // string gives that member, which is where the replay goes
                // (ADR 0085).
                if self.role == Role::Leader
                    && self.election.is_some()
                    && leadership_term_id == self.term
                    && let Some(index) = self.other(follower_member_id)
                {
                    self.on_catchup_position(index, log_position, io)?;
                }
                Ok(())
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
            Message::StopCatchup {
                leadership_term_id,
                follower_member_id,
            } => {
                // The leader's word that the catch-up is over: the member reported
                // with the catch-up flag, which says it is on the live log. The
                // catch-up subscription goes, and follower catch-up leaves once the
                // member is on the live log. One read before the member has reported
                // with the flag answers an earlier catch-up, and is ignored: taking
                // it would end the replay before the member holds the log to where
                // its live image joins.
                if self.state == ElectionState::FollowerCatchup
                    && follower_member_id == self.member_id()
                    && leadership_term_id == self.term
                    && let Some(mut catching) = self.catching
                    && catching.flagged
                    && !catching.stopped
                {
                    if self.catchup {
                        io.remove_catchup()?;
                        self.catchup = false;
                    }
                    catching.stopped = true;
                    catching.commit_target = self.commit_position;
                    self.catching = Some(catching);
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

    /// Whether this member has taken a leader: in follower log replication, and in a
    /// follower's states after canvass.
    fn follows(&self) -> bool {
        matches!(
            self.state,
            ElectionState::FollowerLogReplication
                | ElectionState::FollowerReplay
                | ElectionState::FollowerCatchupInit
                | ElectionState::FollowerCatchupAwait
                | ElectionState::FollowerCatchup
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
    /// from a leader whose log does not reach its position. A member's fails in
    /// follower log replication, as that state says. A leader's,
    /// where it elects, fails in leader log replication or leader ready, on a canvass
    /// position with a term above its own or when its active members fall below a
    /// quorum; init gives its log up. Either is a follower from here, the moves
    /// `candidate -> follower` and `leader -> follower` that `cluster.elections_failed`
    /// counts, so that the role never reads candidate or leader while the election
    /// reads init. The appointed leader fails no election.
    fn fail(&mut self, clock: Clock) {
        count(&mut self.counts.elections_failed);
        self.role = Role::Follower;
        self.cause = ElectionCause::ElectionFailed;
        self.enter(ElectionState::Init, clock);
    }

    /// Truncates this member's log to `position`, the base of the term its leader's
    /// record holds next after this member's own, which a new leadership term named
    /// (`spec/cluster.md`, "Messages during an election"; ADR 0083), or the base of
    /// the lowest of this member's terms that such a message says the leader's
    /// record lacks, before they are dropped (ADR 0096). Returns whether
    /// the member goes on with that message. It does not while the truncation is under
    /// way, and it does not when the position is below the commit position it knows:
    /// a committed entry is never truncated, so no leader that holds what was
    /// committed names such a base, and the member refuses, counted in
    /// `cluster.truncations_refused`, and fails its election.
    /// Once truncated, its append position is the position and its log term the
    /// highest its record still holds, before anything is recorded or reported.
    fn truncate<I: Io>(
        &mut self,
        position: i64,
        clock: Clock,
        io: &mut I,
    ) -> Result<bool, I::Fault> {
        if position < self.commit_position {
            count(&mut self.counts.truncations_refused);
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
        self.cause = ElectionCause::HigherTerm;
        self.enter(ElectionState::Init, clock);
    }

    /// A ballot that ended with no leader, this member's own or its candidate's: its
    /// own on a vote against its candidate term or once it holds no vote of its own
    /// for that term (ADR 0105), either at the election timeout, or on a new leadership
    /// term for a log term that is not its own, once any terms the message says its
    /// leader's record lacks are dropped (ADR 0096). Counted, and the member canvasses
    /// again as a follower, with the reports and the members heard that it has.
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

    /// What this leader serves the member at `index`.
    fn serving(&self, index: usize) -> Serving {
        self.serving.get(index).copied().unwrap_or(Serving::Idle)
    }

    fn serve(&mut self, index: usize, serving: Serving) {
        if let Some(slot) = self.serving.get_mut(index) {
            *slot = serving;
        }
    }

    /// A catch-up position from the member at `index`, read by a leader that elects
    /// (`spec/cluster.md`, "Messages outside an election"). The position tells the
    /// two requests apart. One below this leader's term base log position is a
    /// replication (state 9): the leader replays its recordings from the position to
    /// the first term base above it. One from the base to the leader's own append
    /// position is a catch-up within its term (state 11): the replay follows the
    /// leader's live recording, with no end, until the leader stops it. One above
    /// the leader's append position asks for what the leader does not hold: nothing
    /// is started, the member's own timer counts it, and what the leader held for
    /// the member is ended, since the member has left whatever it was in.
    ///
    /// With no replay running to the member the leader starts one at once. With one
    /// running, the member failed and asks again: the leader stops that one and keeps
    /// the request, the newest, until the replay has stopped. A start that faults
    /// leaves the request as a replay owed, which the duty cycle starts once the
    /// member has reported the position.
    fn on_catchup_position<I: Io>(
        &mut self,
        index: usize,
        position: i64,
        io: &mut I,
    ) -> Result<(), I::Fault> {
        if position < 0 || position > self.append_position {
            // Nothing this leader can replay. The member asked all the same, so what
            // the leader held for it, of an older request, is ended as a canvass
            // position ends it: the replay stopped, the one owed dropped.
            return self.end_serving(index, io);
        }
        let end = if position < self.term_base {
            // This leader's own term is in its record at its base, above the position.
            let Some(end) = io.term_base_above(position) else {
                return Ok(());
            };
            Some(end)
        } else {
            None
        };
        let member = self.peer(index).id;
        let serving = match self.serving(index) {
            Serving::Idle | Serving::Next { .. } => {
                self.serve(index, Serving::Next { position, end });
                self.start_replay(member, position, end, io)?
            }
            Serving::Running => {
                io.stop_replay(member)?;
                Serving::Stopping {
                    next: Some((position, end)),
                }
            }
            Serving::Stopping { .. } => Serving::Stopping {
                next: Some((position, end)),
            },
        };
        self.serve(index, serving);
        Ok(())
    }

    /// The member at `index` is on the live log: it reported with the catch-up flag,
    /// which a member sets once its append position has reached where its live image
    /// joined. The leader ends what it held for it, as for a member that canvasses:
    /// the replay it runs is asked to stop, once, and a replay it still owed the
    /// member is dropped, since the member waits for none. It tells the member so with
    /// stop catch-up, on which the member removes its catch-up subscription. The
    /// member reports with the flag until it has read one, so a stop catch-up that
    /// did not leave is sent again on its next report.
    fn stop_catchup<I: Io>(&mut self, index: usize, io: &mut I) -> Result<(), I::Fault> {
        self.end_serving(index, io)?;
        let member = self.peer(index).id;
        io.send(
            member,
            &Message::StopCatchup {
                leadership_term_id: self.term,
                follower_member_id: member,
            },
        );
        Ok(())
    }

    /// Starts a replay to `member` from `position`: a replication's, whose replays end
    /// at `end`, counted in `cluster.log_replications`, or a catch-up's, with no end,
    /// counted in `cluster.catchups`. A position this leader's recordings do not hold
    /// is answered by nothing and counts nothing.
    fn start_replay<I: Io>(
        &mut self,
        member: i32,
        position: i64,
        end: Option<i64>,
        io: &mut I,
    ) -> Result<Serving, I::Fault> {
        if !io.start_replay(member, position, end)? {
            return Ok(Serving::Idle);
        }
        count(if end.is_some() {
            &mut self.counts.log_replications
        } else {
            &mut self.counts.catchups
        });
        Ok(Serving::Running)
    }

    /// Ends what this leader serves the member at `index`, which canvasses again,
    /// asks for what the leader cannot replay, or reports that it is on the live log:
    /// the replay it runs is stopped, and the next it owed is dropped.
    fn end_serving<I: Io>(&mut self, index: usize, io: &mut I) -> Result<(), I::Fault> {
        let serving = match self.serving(index) {
            Serving::Idle | Serving::Next { .. } => Serving::Idle,
            Serving::Running => {
                io.stop_replay(self.peer(index).id)?;
                Serving::Stopping { next: None }
            }
            Serving::Stopping { .. } => Serving::Stopping { next: None },
        };
        self.serve(index, serving);
        Ok(())
    }

    /// The replays a leader serves, once a duty cycle. A replay that has stopped,
    /// at its replication's end, at its recording's end below it or for any other
    /// reason, is followed by nothing of the leader's own: the member goes on by a
    /// new request, which it sends when its log has moved since it last asked, or
    /// its own timer fails its election (ADR 0092). A replay the leader stopped is
    /// followed, once it has stopped, by the request the member sent since. How a
    /// replay ended is handed over once, so the request is first kept as a replay
    /// owed: a start that faults is tried again on a later cycle, when the member
    /// has reported its position.
    fn serve_replays<I: Io>(&mut self, io: &mut I) -> Result<usize, I::Fault> {
        let mut work = 0;
        // Every member is served in every cycle: one member's fault, which may last,
        // is kept and returned after the others have had their turn.
        let mut fault = None;
        for index in 0..self.count {
            match self.serve_member(index, io) {
                Ok(served) => work = work.max(served),
                Err(error) => {
                    fault.get_or_insert(error);
                }
            }
        }
        match fault {
            Some(error) => Err(error),
            None => Ok(work),
        }
    }

    /// One member's turn in [`Consensus::serve_replays`]: returns 1 when what the
    /// leader holds for it changed.
    fn serve_member<I: Io>(&mut self, index: usize, io: &mut I) -> Result<usize, I::Fault> {
        let peer = self.peer(index);
        let serving = match self.serving(index) {
            Serving::Idle => return Ok(0),
            // A replay that has stopped is followed by nothing of the leader's own,
            // however it ended: one that ended at its recording's end below the
            // replication's is carried on by the member's next catch-up position,
            // which it sends once a recording is armed for the image (ADR 0092).
            Serving::Running => match io.replay_stopped(peer.id)? {
                None => return Ok(0),
                Some(_) => Serving::Idle,
            },
            Serving::Stopping { next } => match (io.replay_stopped(peer.id)?, next) {
                (None, _) => return Ok(0),
                (Some(_), None) => Serving::Idle,
                (Some(_), Some((position, end))) => {
                    self.serve(index, Serving::Next { position, end });
                    self.start_replay(peer.id, position, end, io)?
                }
            },
            Serving::Next { position, end } => {
                if !(peer.reported && peer.appended && peer.position >= position) {
                    return Ok(0);
                }
                self.start_replay(peer.id, position, end, io)?
            }
        };
        self.serve(index, serving);
        Ok(1)
    }

    fn due(last: Option<u64>, interval_ns: u64, clock: Clock) -> bool {
        last.is_none_or(|last| clock.now_ns.saturating_sub(last) >= interval_ns)
    }

    /// State 0. The member takes its candidate term, leaves the live log when it had
    /// joined one, gives up the log of a term it led, stops the replays it served, and
    /// canvasses with no report held from the election before; a cluster of one member
    /// leads at once. A fault at any step leaves it in init, and the next cycle tries
    /// the step again.
    fn init<I: Io>(&mut self, clock: Clock, io: &mut I) -> Result<usize, I::Fault> {
        // A member that elects starts on what its log directory holds (ADR 0089): the
        // candidate term it stored, so that it votes once in a term across a restart,
        // and the highest term of its record, the term it canvasses with. It reads
        // them here, once, and no message before: a member in init reads none. Its
        // append position it reads every cycle, as it always has.
        if self.election.is_some() && !self.read_back {
            self.candidate_term = self.candidate_term.max(io.candidate_term());
            self.log_term = self.log_term.max(io.log_leadership_term_id());
            self.read_back = true;
        }
        // Out of the live log, so that the image joined next is a new one whose join
        // position follower log await checks.
        if self.joined {
            io.leave_log()?;
            self.joined = false;
        }
        // And out of a replication that failed: its subscription goes, and with it
        // the image of any replay still on it. So too out of a catch-up that failed.
        if self.catchup {
            io.remove_catchup()?;
            self.catchup = false;
        }
        self.replication = None;
        self.catching = None;
        // A leader that began again holds the log publication of the term it led. It
        // gives it up, so that no follower's image of it goes on and the term it leads
        // next has a publication of its own, at that term's base (ADR 0082).
        if self.log.is_some() {
            io.remove_log()?;
            self.log = None;
        }
        // And it serves no member from here: a replay it runs is stopped, and it
        // asks no more about one it had stopped already. A fault leaves the rest for
        // the next cycle.
        for index in 0..self.count {
            if matches!(self.serving(index), Serving::Running) {
                io.stop_replay(self.peer(index).id)?;
            }
            self.serve(index, Serving::Idle);
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
        // The appointed leader leads once, in its first election since it started, so
        // its canvass deadline is the startup canvass timeout after the state began.
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
    /// quorum for and none against (ADR 0077). It canvasses again at once when it
    /// cannot win: a vote against its candidate term has been cast, or it no longer
    /// holds its own vote for that term (ADR 0105). At the election timeout with no
    /// leader it canvasses again, short of a quorum.
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
        // A ballot with a vote against makes no leader, at once or at the election
        // timeout, and a candidate without its own vote for its term leads nothing:
        // waiting out the timeout would change neither (ADR 0105).
        if timeout_passed || outcome.lost || !own_vote {
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
        self.led_from_start = self.startup;
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
        self.led_from_start = self.startup;
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

    /// New leadership term to the member at `index`, for the highest term this
    /// leader's record holds at or below the one it reported.
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
        // The term this message answers for is one this leader's own record holds:
        // the highest at or below the one the member reported. A member that
        // recorded a term this record lacks, one that fewer than a quorum recorded,
        // reads a term below its own and drops what lies above it (ADR 0096).
        let answered = io.term_at_or_below(peer.term);
        io.send(
            peer.id,
            &Message::NewLeadershipTerm {
                log_leadership_term_id: answered,
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
                // The appointed leader leads once, from its start. A leader that
                // elects says whether this term is of its first election since it
                // started, as Aeron's leader sends its own node startup (ADR 0089).
                is_startup: self.election.is_none() || self.led_from_start,
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

    /// State 8. The heartbeat until enough members count for the term at the leader's
    /// log position; then the term's first entry, the new leadership term event. A
    /// member counts by an append position alone, which it sends once it has joined the
    /// log. A leader that elects closes on a quorum at once: a member that joins after
    /// the term's first entry catches up with it, states 11 to 13. It fails its
    /// election when its active members fall below a quorum, as in state 5. The
    /// appointed leader's members have no catch-up, so it waits for every member it has
    /// heard, or for a quorum once the leader heartbeat timeout has passed in this
    /// state, and never while a member's last report holds a log ahead of its own. A
    /// member is heard once it has reported since the canvass began, in a canvass
    /// position or an append position.
    fn leader_ready<I: Io>(&mut self, clock: Clock, io: &mut I) -> Result<usize, I::Fault> {
        if self.report_own(clock) {
            self.fail(clock);
            return Ok(1);
        }
        let work = self.announce(clock, io);
        let members = || self.peers.iter().take(self.count);
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
        let ready = if self.election.is_some() {
            // Row 8 of the page. No member's log is ahead of an elected leader's: a
            // report of a term above its own has already failed this election.
            counted >= quorum(self.count)
        } else {
            let heard = members().filter(|peer| peer.reported).count();
            let deadline_passed =
                clock.now_ns.saturating_sub(self.state_since_ns) >= LEADER_HEARTBEAT_TIMEOUT_NS;
            let ahead = self.count_ahead(LogReport {
                term: self.term,
                position: self.append_position,
            });
            leader_ready(self.count, heard, counted, ahead, deadline_passed)
        };
        if !ready {
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
            self.cause = ElectionCause::QuorumLost;
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

    /// State 9. The member replicates one term from its leader's recordings, from its
    /// append position to the stop position its new leadership term gave, the end of
    /// that term (`spec/cluster.md`, follower log replication; ADR 0086).
    ///
    /// A term it holds none of is recorded at its base first, and only while its
    /// append position is still that base. Then it subscribes on
    /// its `catchup` endpoint and sends catch-up position, for one replay: the
    /// replay's image must join where the member asked from. When that image has
    /// gone with the member still below its stop, the member removes its
    /// subscription and, when its log ends further on than where it asked from,
    /// subscribes and asks again from there: a recording is armed for each image,
    /// and a member asks from a position once (ADR 0092). It reports its append
    /// position as it moves and every leader heartbeat interval. Below the stop, an
    /// append position that has not moved for the leader heartbeat timeout fails the
    /// election.
    ///
    /// At the stop it records the leader's term there when the stop is the leader's
    /// term base log position, reports, and then waits for the commit position it was
    /// told to reach its append position, for the leader heartbeat timeout after the
    /// later of the stop and the leader's last word. A stop below the base has no such
    /// wait. Then the subscription goes and the member canvasses again: the next new
    /// leadership term carries it on to the next term, or it joins.
    fn follower_log_replication<I: Io>(
        &mut self,
        clock: Clock,
        io: &mut I,
    ) -> Result<usize, I::Fault> {
        let Some(mut replication) = self.replication else {
            // The state is entered with a replication and left without one.
            self.fail(clock);
            return Ok(1);
        };
        if let Some((term, base)) = replication.begins {
            // The term is begun at its base and nowhere else. The message that began
            // this was read with the append position of the cycle before; one that
            // has risen since is of bytes above the base that are of no term this
            // member may record there, so it records nothing and begins again.
            if self.append_position != base {
                self.fail(clock);
                return Ok(1);
            }
            io.record_term(term, base)?;
            self.log_term = term;
            replication.begins = None;
            self.replication = Some(replication);
        }
        if self.append_position != replication.position {
            replication.position = self.append_position;
            replication.moved_ns = clock.now_ns;
        }
        // Past the stop is past the end of the term: a replay the member did not ask
        // for. Nothing it holds above the stop is of a term in its record.
        if self.append_position > replication.stop {
            self.fail(clock);
            return Ok(1);
        }
        let mut work = 0;
        if self.append_position < replication.stop {
            // The member asks for one replay at a time, and from a position once
            // (ADR 0092): at first, and again when a replay's image has gone and
            // left its log further on. It subscribes for each, so that a recording
            // is armed for the image the leader's replay brings: a driver arms one
            // only on a stream with no image.
            if !replication.asked && replication.asked_from != Some(self.append_position) {
                if !self.catchup {
                    io.add_catchup()?;
                    self.catchup = true;
                }
                if io.send_catchup_position(self.leader_id, self.term, self.append_position) {
                    replication.asked = true;
                    replication.asked_from = Some(self.append_position);
                    replication.image = None;
                    count(&mut self.counts.log_replications);
                    work = 1;
                    // Kept at once: a fault below must not ask a second time.
                    self.replication = Some(replication);
                }
            }
            if self.catchup {
                match io.catchup_image()? {
                    // A new image, checked before this cycle reports what it
                    // brought: it must join where the member asked from.
                    Some(join) if replication.image != Some(join) => {
                        if replication.asked_from != Some(join) {
                            count(&mut self.counts.join_position_mismatches);
                            self.fail(clock);
                            return Ok(1);
                        }
                        replication.image = Some(join);
                    }
                    Some(_) => {}
                    // The image has gone: its replay ended, at its recording's end
                    // or at a fault, or was stopped. The subscription goes, and the
                    // stop of the image's recording is where the log ends, which the
                    // next cycle reads; it asks again there if that is further on.
                    None if replication.image.is_some() => {
                        io.remove_catchup()?;
                        self.catchup = false;
                        replication.image = None;
                        replication.asked = false;
                        work = 1;
                        self.replication = Some(replication);
                    }
                    None => {}
                }
            }
            if clock.now_ns.saturating_sub(replication.moved_ns) >= LEADER_HEARTBEAT_TIMEOUT_NS {
                count(&mut self.counts.log_replications_unavailable);
                self.fail(clock);
                return Ok(1);
            }
            if self.report_replicated(clock, io) {
                work = 1;
            }
            self.replication = Some(replication);
            return Ok(work);
        }
        // At the stop. The leader's term is recorded at its base before the stop is
        // reported, so that the member counts toward the leader's commits with the
        // log it holds to the base, as one that joined at the base does.
        let at_leaders_base = replication.stop == self.term_base;
        if at_leaders_base && self.term > self.log_term {
            io.record_term(self.term, self.term_base)?;
            self.log_term = self.term;
        }
        if self.report_replicated(clock, io) {
            work = 1;
        }
        if at_leaders_base && self.commit_position < self.append_position {
            let reached = *replication.reached_ns.get_or_insert(clock.now_ns);
            self.replication = Some(replication);
            let since = reached.max(self.last_leader_word_ns);
            if clock.now_ns.saturating_sub(since) >= LEADER_HEARTBEAT_TIMEOUT_NS {
                self.fail(clock);
                return Ok(1);
            }
            return Ok(work);
        }
        if self.catchup {
            io.remove_catchup()?;
            self.catchup = false;
        }
        self.replication = None;
        // The reports this member holds were read against the log it had before it
        // replicated. Against the log it has now, a report that was ahead of it may
        // no longer be, and canvass would make it a candidate at once, to ask for
        // votes in a term above its leader's. It canvasses with no report, as a
        // member does from init, and its leader answers its canvass position (ADR
        // 0106).
        for peer in self.peers.iter_mut().take(self.count) {
            peer.reported = false;
        }
        Ok(self.step(ElectionState::Canvass, clock))
    }

    /// The member's append position to its leader while it replicates: when it has
    /// moved since the last sent, and every leader heartbeat interval. Returns
    /// whether one left.
    fn report_replicated<I: Io>(&mut self, clock: Clock, io: &mut I) -> bool {
        (self.append_position != self.last_append_sent
            || Self::due(
                self.last_append_sent_ns,
                LEADER_HEARTBEAT_INTERVAL_NS,
                clock,
            ))
            && self.send_append_position(clock, io)
    }

    /// State 10. Nothing to replay in a log with no term before the leader's. A member
    /// behind the leader's log position catches up, when it elects; one whose leader
    /// is appointed has no catch-up and fails.
    fn follower_replay<F>(&mut self, clock: Clock) -> Result<usize, F> {
        if self.append_position < self.leader_log_position {
            if self.election.is_none() {
                self.fail(clock);
                return Ok(1);
            }
            return Ok(self.step(ElectionState::FollowerCatchupInit, clock));
        }
        Ok(self.step(ElectionState::FollowerLogInit, clock))
    }

    /// State 11. The member subscribes on its `catchup` endpoint and asks its leader
    /// for the log from its append position, counted in `cluster.catchups` when the
    /// catch-up position has left. One that does not leave within the leader
    /// heartbeat timeout fails the election.
    fn follower_catchup_init<I: Io>(
        &mut self,
        clock: Clock,
        io: &mut I,
    ) -> Result<usize, I::Fault> {
        if !self.catchup {
            io.add_catchup()?;
            self.catchup = true;
        }
        if io.send_catchup_position(self.leader_id, self.term, self.append_position) {
            count(&mut self.counts.catchups);
            self.catching = Some(Catchup {
                asked: self.append_position,
                flagged: false,
                stopped: false,
                commit_target: 0,
                position: self.append_position,
                moved_ns: clock.now_ns,
            });
            // Its first cycle in follower catch-up reports its position.
            self.last_append_sent = -1;
            self.last_append_sent_ns = None;
            return Ok(self.step(ElectionState::FollowerCatchupAwait, clock));
        }
        if clock.now_ns.saturating_sub(self.state_since_ns) >= LEADER_HEARTBEAT_TIMEOUT_NS {
            self.fail(clock);
            return Ok(1);
        }
        Ok(0)
    }

    /// State 12. The image of the leader's replay, which must join at the position
    /// the member asked from. One that joins elsewhere, a stale replay's among them,
    /// fails the election, counted; so does none within the leader heartbeat timeout.
    fn follower_catchup_await<I: Io>(
        &mut self,
        clock: Clock,
        io: &mut I,
    ) -> Result<usize, I::Fault> {
        let asked = self
            .catching
            .map_or(self.append_position, |catching| catching.asked);
        match io.catchup_image()? {
            Some(join) if join == asked => {
                // Follower catch-up's time begins here, with its replay: what the
                // image took to come is this state's, and is not counted against an
                // append position that has had no replay to move it.
                if let Some(mut catching) = self.catching {
                    catching.position = self.append_position;
                    catching.moved_ns = clock.now_ns;
                    self.catching = Some(catching);
                }
                Ok(self.step(ElectionState::FollowerCatchup, clock))
            }
            Some(_) => {
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

    /// State 13. The member records the replay and reports its append position, when
    /// it moves and every leader heartbeat interval. Once it is within the live-add
    /// window of the leader's position, the greater of the log position and the
    /// commit position its leader told it, it adds the live log: the live image joins
    /// at or past its append position, and the replay brings it there. Its reports
    /// carry the catch-up flag only from then on, once its append position has
    /// reached where the live image joined: the flag tells the leader that it may
    /// stop the replay, and a replay stopped sooner would leave a hole below the live
    /// image that nothing fills (ADR 0088). When the leader has sent stop catch-up,
    /// and its append position has reached the commit position it had been told when
    /// it read that message, it goes on to follower log init, already subscribed: the
    /// commit positions a leader sends after do not hold it, or a steady load would
    /// for as long as it lasted. A catch-up whose append position
    /// has not moved for the leader heartbeat timeout, counted from the replay's image
    /// joining, fails the election.
    fn follower_catchup<I: Io>(&mut self, clock: Clock, io: &mut I) -> Result<usize, I::Fault> {
        let Some(mut catching) = self.catching else {
            // The state is entered with a catch-up and left without one.
            self.fail(clock);
            return Ok(1);
        };
        if self.append_position != catching.position {
            catching.position = self.append_position;
            catching.moved_ns = clock.now_ns;
            self.catching = Some(catching);
        }
        let mut work = 0;
        let near = self.leader_log_position.max(self.commit_position);
        if !self.joined && self.append_position >= near.saturating_sub(LIVE_ADD_WINDOW) {
            io.join_log()?;
            self.joined = true;
            work = 1;
        }
        // On the live log: the member holds every position below where its live
        // image joined, so the live log brings it all that follows.
        let on_live = self.joined
            && io
                .log_image(self.log_session_id)?
                .is_some_and(|live_start| self.append_position >= live_start);
        let flags = if on_live {
            APPEND_POSITION_FLAG_CATCHUP
        } else {
            0
        };
        // Sent when the position moved, every leader heartbeat interval, and at once
        // when the member is first on the live log, so that the leader's stop does
        // not wait an interval for it.
        if (self.append_position != self.last_append_sent
            || (on_live && !catching.flagged)
            || Self::due(
                self.last_append_sent_ns,
                LEADER_HEARTBEAT_INTERVAL_NS,
                clock,
            ))
            && self.send_append_position_flagged(clock, flags, io)
        {
            catching.flagged |= on_live;
            work = 1;
        }
        self.catching = Some(catching);
        if catching.stopped && on_live && self.append_position >= catching.commit_target {
            self.catching = None;
            return Ok(self.step(ElectionState::FollowerLogInit, clock));
        }
        if clock.now_ns.saturating_sub(catching.moved_ns) >= LEADER_HEARTBEAT_TIMEOUT_NS {
            self.fail(clock);
            return Ok(1);
        }
        Ok(work)
    }

    /// State 14. The log subscription and its armed recording. A member that caught
    /// up is already subscribed, by the live add of state 13, and on the live image:
    /// it goes to follower ready.
    fn follower_log_init<I: Io>(&mut self, clock: Clock, io: &mut I) -> Result<usize, I::Fault> {
        if self.joined {
            return Ok(self.step(ElectionState::FollowerReady, clock));
        }
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
        self.send_append_position_flagged(clock, 0, io)
    }

    /// The member's append position to its leader, with `flags`: the catch-up flag
    /// for a member in follower catch-up that is on the live log, and none otherwise.
    fn send_append_position_flagged<I: Io>(&mut self, clock: Clock, flags: u8, io: &mut I) -> bool {
        let sent = io.send(
            self.leader_id,
            &Message::AppendPosition {
                leadership_term_id: self.log_term,
                log_position: self.append_position,
                follower_member_id: self.member_id(),
                flags,
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
            self.cause = ElectionCause::LeaderHeartbeatTimeout;
            return Ok(self.enter(ElectionState::Init, clock));
        }
        if io.log_image(self.log_session_id)?.is_none() {
            count(&mut self.counts.log_images_lost);
            self.cause = ElectionCause::LogImageLost;
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
