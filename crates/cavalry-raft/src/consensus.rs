//! The core of a cluster member: the state machine of `spec/cluster.md`, "Roles and the
//! election", on the appointed leader's path ("The appointed leader"; ADR 0063). It does
//! no I/O of its own. It is given the time and the consensus messages, and reaches
//! everything else through [`Io`]: the module gives it those operations over the client
//! and the `terms` file, and [`crate::memory`] gives it the same operations in memory.
//!
//! One member is one [`Consensus`], driven by one thread: [`Consensus::do_work`] once
//! per duty cycle and [`Consensus::on_message`] for each message of the consensus
//! stream. Neither allocates, locks or blocks, and every table is an array of
//! [`MAX_MEMBERS`].

use cavalry_proto::cluster::{
    CanvassAction, CanvassTerms, ELECTION_STATUS_INTERVAL_NS, LEADER_HEARTBEAT_INTERVAL_NS,
    LEADER_HEARTBEAT_TIMEOUT_NS, LogReport, MAX_MEMBERS, MemberPosition, Message, PROTOCOL_VERSION,
    STARTUP_CANVASS_TIMEOUT_NS, TermAction, TermFields, TimeUnit, appointed_canvass,
    appointed_leader, canvass_position_action, commit_position, leader_ready, log_at_least,
    new_leadership_term_action, next_fields, quorum,
};

/// The cluster session id of a session message of no session: one the leader's own
/// process appended (`spec/cluster.md`, "The appointed leader").
pub const NO_SESSION: i64 = -1;

/// A member's role, with the value of the `cluster.role` gauge (`spec/cluster.md`,
/// "Counters").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Role {
    /// Records the leader's log and reports its append position.
    Follower = 0,
    /// A follower asking for votes. No member of this version is one: the leader is
    /// appointed.
    Candidate = 1,
    /// Writes the log and commits it.
    Leader = 2,
}

// not a state machine: the election's states as the core holds them. Its machine is
// registered in `counters.toml`, with the counters of [`Counts`] on its transitions, by
// the consensus module, which stores those counters in the counters file and is this
// crate's next change (ADR 0063); the core has no counters file to move one in.
/// The states of the election a member of this version enters, each with the code
/// `spec/cluster.md` gives it, the value of the `cluster.election_state` gauge.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ElectionState {
    /// Takes its candidate term, leaves the live log when it had joined one, and
    /// canvasses.
    Init = 0,
    /// Reports its log to every other member. The appointed leader leads once canvass
    /// makes it a candidate, every member having reported with no log ahead of its own,
    /// or a quorum at the startup canvass timeout; every other member waits for the
    /// leader's new leadership term.
    Canvass = 1,
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
    /// No election runs: the leader leads its term, a follower follows it.
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

    /// The member's append position: the end of the contiguous span its recordings of
    /// the log hold from position 0, as a sync has made it durable (ADR 0060). 0 for a
    /// member with no recording.
    fn append_position(&mut self) -> i64;

    /// Appends `entry` to the leader's log. `Ok(false)` when the publication is
    /// back-pressured and nothing was written.
    fn append(&mut self, entry: &Message<'_>) -> Result<bool, Self::Fault>;
}

/// Why [`Consensus::new`] refused its members.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MembersRefused {
    /// No member, or more than [`MAX_MEMBERS`].
    Count {
        /// How many were given.
        found: usize,
    },
    /// Two members with one id, or an id below 0.
    Id {
        /// The id.
        id: i32,
    },
    /// The member's own id is not among the members.
    NotAMember {
        /// The id.
        id: i32,
    },
}

impl core::fmt::Display for MembersRefused {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            MembersRefused::Count { found } => write!(
                f,
                "members refused (members-count): {found} members, and a cluster has 1 to \
                 {MAX_MEMBERS}; give the members string of the cluster; see \
                 docs/reference/cluster.md#members-and-streams"
            ),
            MembersRefused::Id { id } => write!(
                f,
                "members refused (members-duplicate): member id {id} is below 0 or given \
                 twice; give each member one id from 0 upward; see \
                 docs/reference/cluster.md#members-and-streams"
            ),
            MembersRefused::NotAMember { id } => write!(
                f,
                "members refused (members-self): member id {id} is not in the members \
                 string; set the member id to one the string holds; see \
                 docs/reference/cluster.md#members-and-streams"
            ),
        }
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
                "this member follows; offer to the leader, the member with the lowest id",
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
/// with its name. The module stores them in the counters file; they only rise.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Counts {
    /// `cluster.elections`: an election begun, from init.
    pub elections: u64,
    /// `cluster.elections_failed`: an election that failed in a state and began again
    /// at init.
    pub elections_failed: u64,
    /// `cluster.terms_led`: a term the member began to lead.
    pub terms_led: u64,
    /// `cluster.leader_quorum_losses`: a leader whose active members fell below a
    /// quorum; the appointed leader stays the leader, and its commit position stops.
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
    /// The appointed leader's id.
    leader_id: i32,
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
    /// How many members the leader heard in its canvass, itself among them: the ones
    /// leader ready waits for.
    heard: usize,
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
        let count = member_ids.len();
        if count == 0 || count > MAX_MEMBERS {
            return Err(MembersRefused::Count { found: count });
        }
        let mut peers = [Peer {
            id: -1,
            reported: false,
            position: 0,
            term: -1,
            at_ns: 0,
            ahead_counted: false,
        }; MAX_MEMBERS];
        let mut own = None;
        for (index, (&id, peer)) in member_ids.iter().zip(peers.iter_mut()).enumerate() {
            if id < 0 || member_ids.iter().take(index).any(|&other| other == id) {
                return Err(MembersRefused::Id { id });
            }
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
            heard: 0,
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

    /// This member's id.
    pub fn member_id(&self) -> i32 {
        self.peer(self.own).id
    }

    /// The id of the appointed leader: the lowest among the members.
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
            ElectionState::LeaderLogReplication => self.leader_log_replication(clock, io),
            ElectionState::LeaderReplay => Ok(self.enter(ElectionState::LeaderInit, clock)),
            ElectionState::LeaderInit => Ok(self.enter(ElectionState::LeaderReady, clock)),
            ElectionState::LeaderReady => self.leader_ready(clock, io),
            ElectionState::FollowerReplay => self.follower_replay(clock),
            ElectionState::FollowerLogInit => self.follower_log_init(clock, io),
            ElectionState::FollowerLogAwait => self.follower_log_await(clock, io),
            ElectionState::FollowerReady => Ok(self.follower_ready(clock, io)),
            ElectionState::Closed => match self.role {
                Role::Leader => Ok(self.lead(clock, io)),
                Role::Follower | Role::Candidate => self.follow(clock, io),
            },
        }
    }

    /// One message of the consensus stream, read at `clock`. A member in init reads
    /// nothing, and a message this version's members do not send, request vote, vote,
    /// catch-up position and stop catch-up, is ignored (`spec/cluster.md`, "The
    /// appointed leader").
    pub fn on_message<I: Io>(
        &mut self,
        clock: Clock,
        message: &Message<'_>,
        io: &mut I,
    ) -> Result<(), I::Fault> {
        if self.state == ElectionState::Init {
            return Ok(());
        }
        match *message {
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
                self.report(index, log_position, log_leadership_term_id, clock);
                if self.role != Role::Leader {
                    return Ok(());
                }
                // An election on a term above its own is not the appointed leader's to
                // hold: no election can replace it, and no member of this version holds
                // such a term.
                let action = canvass_position_action(
                    self.term,
                    self.state.code(),
                    CanvassTerms {
                        leadership_term_id,
                        log_leadership_term_id,
                    },
                );
                if action == CanvassAction::Answer {
                    self.announce_to(index, clock, io);
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
                    self.report(index, log_position, leadership_term_id, clock);
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
                if self.role == Role::Leader || leader_member_id != self.leader_id {
                    return Ok(());
                }
                if self.state != ElectionState::Canvass {
                    // Its leader's word while it joins or follows; any other term's is
                    // ignored and counted.
                    if leadership_term_id == self.term {
                        self.leader_word(clock, log_position, commit_position);
                    } else {
                        count(&mut self.counts.leadership_terms_ignored);
                    }
                    return Ok(());
                }
                let action = new_leadership_term_action(
                    self.log_term,
                    self.append_position,
                    self.candidate_term,
                    true,
                    TermFields {
                        leadership_term_id,
                        log_leadership_term_id,
                        next_leadership_term_id,
                        next_term_base_log_position,
                        next_log_position,
                        term_base_log_position,
                    },
                );
                match action.action {
                    TermAction::Ignore => {
                        count(&mut self.counts.leadership_terms_ignored);
                        Ok(())
                    }
                    TermAction::Canvass => Ok(()),
                    TermAction::Replay if action.truncate_to < 0 => {
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
                        self.log_session_id = log_session_id;
                        self.leader_log_position = log_position;
                        self.leader_word(clock, log_position, commit_position);
                        self.enter(ElectionState::FollowerReplay, clock);
                        Ok(())
                    }
                    // A truncation, a replication, a wait or a log the leader's does
                    // not reach: nothing this version can join with.
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

    fn report(&mut self, index: usize, position: i64, term: i64, clock: Clock) {
        if let Some(peer) = self.peers.get_mut(index) {
            peer.reported = true;
            peer.position = position;
            peer.term = term;
            peer.at_ns = clock.now_ns;
        }
    }

    fn enter(&mut self, state: ElectionState, clock: Clock) -> usize {
        self.state = state;
        self.state_since_ns = clock.now_ns;
        1
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

    /// An election that failed in its state: counted, and begun again at init.
    fn fail(&mut self, clock: Clock) {
        count(&mut self.counts.elections_failed);
        self.enter(ElectionState::Init, clock);
    }

    fn due(last: Option<u64>, interval_ns: u64, clock: Clock) -> bool {
        last.is_none_or(|last| clock.now_ns.saturating_sub(last) >= interval_ns)
    }

    /// State 0. The member takes its candidate term, leaves the live log when it had
    /// joined one, and canvasses with no report held from the election before.
    fn init<I: Io>(&mut self, clock: Clock, io: &mut I) -> Result<usize, I::Fault> {
        // Out of the live log, so that the image joined next is a new one whose join
        // position follower log await checks.
        if self.joined {
            io.leave_log()?;
            self.joined = false;
        }
        self.candidate_term = self.candidate_term.max(self.log_term);
        for peer in self.peers.iter_mut().take(self.count) {
            peer.reported = false;
            peer.ahead_counted = false;
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
        let mut work = 0;
        if Self::due(self.last_canvass_ns, ELECTION_STATUS_INTERVAL_NS, clock) {
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
            work = 1;
        }
        if self.member_id() != self.leader_id {
            return Ok(work);
        }
        let own = LogReport {
            term: self.log_term,
            position: self.append_position,
        };
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
        // A member of this version starts with no term, so its canvass deadline is the
        // startup canvass timeout after the state began.
        let deadline_passed =
            clock.now_ns.saturating_sub(self.state_since_ns) >= STARTUP_CANVASS_TIMEOUT_NS;
        self.count_ahead(own);
        if !appointed_canvass(self.count, &reports, own, deadline_passed).leads {
            return Ok(work);
        }
        // Where an elected member would nominate itself: the term it proposes is
        // stored, and recorded at its append position, before anything in memory
        // moves. A fault at either write leaves the member in canvass, and the next
        // cycle proposes the same term again.
        let term = self.candidate_term.saturating_add(1);
        io.store_candidate_term(term)?;
        io.record_term(term, self.append_position)?;
        self.candidate_term = term;
        self.log_term = term;
        self.term = term;
        self.term_base = self.append_position;
        count(&mut self.counts.terms_led);
        self.role = Role::Leader;
        self.quorate = true;
        self.heard = reports.iter().flatten().count();
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
    /// them, fall below one. The appointed leader stays the leader.
    fn watch_quorum(&mut self, clock: Clock) {
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
        if self.quorate && !quorate {
            count(&mut self.counts.leader_quorum_losses);
        }
        self.quorate = quorate;
    }

    /// The leader's own place in its table, set on every cycle, and its quorum
    /// watched.
    fn report_own(&mut self, clock: Clock) {
        self.report(self.own, self.append_position, self.term, clock);
        self.watch_quorum(clock);
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
        // This leader's record holds its own term alone, so the lowest term above any
        // other the member reports is that term, at its base, with no end.
        let next = next_fields(&[], peer.term, self.term, self.term_base);
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
    /// quorum position has reached the append position.
    fn leader_log_replication<I: Io>(
        &mut self,
        clock: Clock,
        io: &mut I,
    ) -> Result<usize, I::Fault> {
        if self.log.is_none() {
            self.log = io.add_log(self.term, self.term_base)?;
            return Ok(1);
        }
        self.report_own(clock);
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
            return Ok(self.enter(ElectionState::LeaderReplay, clock));
        }
        Ok(work)
    }

    /// State 8. The heartbeat until every member the leader heard in its canvass counts
    /// for the term at the leader's log position, or a quorum once the leader heartbeat
    /// timeout has passed in this state, and no member's last report holds a log ahead
    /// of the leader's; then the term's first entry, the new leadership term event.
    fn leader_ready<I: Io>(&mut self, clock: Clock, io: &mut I) -> Result<usize, I::Fault> {
        self.report_own(clock);
        let work = self.announce(clock, io);
        let counted = self
            .peers
            .iter()
            .take(self.count)
            .filter(|peer| {
                // At the leader's log position and no other: a position past it is of
                // entries this leader's log does not hold.
                peer.reported
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
        if !leader_ready(self.count, self.heard, counted, ahead, deadline_passed) {
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
        Ok(self.enter(ElectionState::Closed, clock))
    }

    /// The leader, its election closed: the commit position from the members' append
    /// positions, sent when it rises and every leader heartbeat interval. A leader
    /// whose active members fall below a quorum stays the leader: its commit position
    /// stays where it was.
    fn lead<I: Io>(&mut self, clock: Clock, io: &mut I) -> usize {
        self.report_own(clock);
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
        Ok(self.enter(ElectionState::FollowerLogInit, clock))
    }

    /// State 14. The log subscription and its armed recording. No member of this
    /// version is already subscribed here: that is the catch-up's live add, state 13.
    fn follower_log_init<I: Io>(&mut self, clock: Clock, io: &mut I) -> Result<usize, I::Fault> {
        io.join_log()?;
        self.joined = true;
        Ok(self.enter(ElectionState::FollowerLogAwait, clock))
    }

    /// State 15. The leader's image, which must join at this member's append position.
    fn follower_log_await<I: Io>(&mut self, clock: Clock, io: &mut I) -> Result<usize, I::Fault> {
        match io.log_image(self.log_session_id)? {
            Some(join_position) if join_position == self.append_position => {
                Ok(self.enter(ElectionState::FollowerReady, clock))
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
            return self.enter(ElectionState::Closed, clock);
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
