//! A cluster member's sessions: the member's side of the session protocol
//! (`spec/cluster.md`, "Sessions"; ADR 0115).
//!
//! [`Sessions`] is plain state, as [`Consensus`](crate::Consensus) is: it reads no
//! clock, opens no socket and allocates nothing after [`Sessions::new`]. Its caller
//! hands it three things. The messages of the ingress stream, one at a time, through
//! [`Sessions::on_ingress`], whose answer says whether the message was taken or must
//! stay unread. The session open and close events of the member's own log, in log
//! order and no further than the commit position, through [`Sessions::on_entry`]:
//! they alone open and close a session, on the leader and on a follower alike, so
//! that every member holds the same sessions at the same position and a new leader
//! has them. And a duty cycle, [`Sessions::do_work`], which sends what is owed and
//! counts the timeouts. What it sends and appends goes through [`SessionIo`].

use cavalry_proto::channel::Channel;
use cavalry_proto::cluster::{
    CONCURRENT_SESSION_LIMIT, CloseReason, EGRESS_STREAM_ID, EventCode, INVALID_CLIENT_VERSION,
    INVALID_RESPONSE_CHANNEL, LEADER_HEARTBEAT_TIMEOUT_NS, MAX_PENDING_ANSWERS, MAX_SESSIONS,
    Message, PROTOCOL_VERSION, SESSION_CONNECT_TIMEOUT_NS, SESSION_TIMEOUT_NS,
};
use cavalry_shm::counters::CounterId;
use cavalry_shm::counters::generated::{
    CLUSTER_CONNECTS_DROPPED, CLUSTER_INGRESS_IGNORED, CLUSTER_SESSIONS_ACCEPTED,
    CLUSTER_SESSIONS_CLOSED, CLUSTER_SESSIONS_OPENED, CLUSTER_SESSIONS_REFUSED,
    CLUSTER_SESSIONS_TIMED_OUT,
};

use crate::consensus::Clock;

/// The longest response channel a session holds: `cavalry:udp?endpoint=`, a host name
/// of 253 bytes, `:` and a port of five digits. A longer one is not
/// `cavalry:udp?endpoint=<host:port>` and is refused as one that is not.
pub const MAX_RESPONSE_CHANNEL_LENGTH: usize = 21 + 253 + 1 + 5;

/// How many egress publications a member holds at most: one for each session and one
/// for each pending answer. [`SessionIo`] names each by its slot, below this.
pub const EGRESS_SLOTS: usize = MAX_SESSIONS + MAX_PENDING_ANSWERS;

/// The detail of the session event that closes a session for the session timeout.
pub const TIMEOUT_DETAIL: &[u8] = b"TIMEOUT";

/// What the member's election says, read once for a duty cycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Leadership {
    /// Whether the member is the leader and its election has closed: the one member
    /// that opens sessions and appends their messages.
    pub leads: bool,
    /// The member's leadership term id, −1 for none.
    pub leadership_term_id: i64,
    /// The member it leads as or follows, −1 while it has none.
    pub leader_member_id: i32,
}

/// What a member's sessions do to the world: the egress publications they answer on
/// and the leader's log.
pub trait SessionIo {
    /// What a call that could not be made returns.
    type Fault;

    /// Adds a publication on the egress stream to `channel`, held in `slot`, which
    /// holds none. `Ok(false)` when the driver refuses the channel, and the slot then
    /// holds nothing.
    fn add_egress(&mut self, slot: usize, channel: &[u8]) -> Result<bool, Self::Fault>;

    /// Sends `message` on the publication in `slot`. `false` when it was not sent: the
    /// slot holds no publication, the client has not connected to it yet, or it is
    /// back-pressured.
    fn send_egress(&mut self, slot: usize, message: &Message<'_>) -> bool;

    /// Removes the publication in `slot`. A slot that holds none is left as it is.
    fn remove_egress(&mut self, slot: usize) -> Result<(), Self::Fault>;

    /// Appends `entry` to the leader's log. `Ok(false)` when the log is
    /// back-pressured and nothing was written.
    fn append(&mut self, entry: &Message<'_>) -> Result<bool, Self::Fault>;
}

/// What became of a message of the ingress stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Ingress {
    /// The message was acted on, answered, ignored or dropped, each counted: the
    /// reader takes it.
    Taken,
    /// The message cannot be taken yet, because the log is back-pressured or the
    /// member's election has not closed: the reader leaves it at its image's
    /// position and reads nothing more from the ingress stream this cycle
    /// (`Action::Abort` of a controlled poll, ADR 0114).
    Unread,
}

/// What a member's sessions have counted, each the counter of `spec/cluster.md`,
/// "Counters", with its name. They only rise.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct SessionCounts {
    /// `cluster.sessions_accepted`: a session connect the leader accepted, which
    /// began a session's connection.
    pub accepted: u64,
    /// `cluster.sessions_opened`: a session opened by a committed session open event,
    /// on the leader and on every follower.
    pub opened: u64,
    /// `cluster.sessions_closed`: a session closed by a committed session close
    /// event, on the leader and on every follower.
    pub closed: u64,
    /// `cluster.sessions_timed_out`: a session the leader closed for the session
    /// timeout.
    pub timed_out: u64,
    /// `cluster.sessions_refused`: a session connect the leader refused, with the
    /// version, the response channel or the limit as the detail.
    pub refused: u64,
    /// `cluster.connects_dropped`: a session connect dropped unanswered.
    pub connects_dropped: u64,
    /// `cluster.ingress_ignored`: a session message, keepalive or close ignored, for
    /// a term not the leader's or a session not open.
    pub ingress_ignored: u64,
}

impl SessionCounts {
    /// How many counts these are: one counter of `counters.toml` each.
    pub const LEN: usize = 7;

    /// Each count with the id of its counter in `counters.toml`, as
    /// [`Counts::by_counter`](crate::Counts::by_counter) gives the election's.
    ///
    /// ```
    /// use cavalry_raft::SessionCounts;
    /// use cavalry_shm::counters::generated::{CLUSTER_SESSIONS_OPENED, COUNTERS};
    ///
    /// let counts = SessionCounts { opened: 2, ..SessionCounts::default() };
    /// let pairs = counts.by_counter();
    /// assert!(pairs.contains(&(CLUSTER_SESSIONS_OPENED, 2)));
    /// for (id, _) in pairs {
    ///     assert!(COUNTERS.iter().any(|def| def.id == id));
    /// }
    /// ```
    pub const fn by_counter(&self) -> [(CounterId, u64); Self::LEN] {
        [
            (CLUSTER_SESSIONS_ACCEPTED, self.accepted),
            (CLUSTER_SESSIONS_OPENED, self.opened),
            (CLUSTER_SESSIONS_CLOSED, self.closed),
            (CLUSTER_SESSIONS_TIMED_OUT, self.timed_out),
            (CLUSTER_SESSIONS_REFUSED, self.refused),
            (CLUSTER_CONNECTS_DROPPED, self.connects_dropped),
            (CLUSTER_INGRESS_IGNORED, self.ingress_ignored),
        ]
    }
}

/// A session's place in its life (`spec/cluster.md`, "Sessions"). A slot of the
/// table that holds no session is closed.
// state machine: cluster_session
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum SessionState {
    /// No session: the slot is free.
    Closed,
    /// The leader accepted the connect and holds an egress publication for it; the
    /// session open event is appended or waits for the log, and is not committed.
    Connecting,
    /// A committed session open event opened it.
    Open,
}

/// One session, or a free slot. Its place in the table is its egress slot.
#[derive(Clone, Copy, Debug)]
struct Session {
    state: SessionState,
    id: i64,
    correlation_id: i64,
    /// Whether the member holds an egress publication for it: the leader, from the
    /// connect it accepted or from the election it won.
    egress: bool,
    /// Connecting: whether the session open event is in the log.
    appended: bool,
    /// Open, on the leader that accepted it: the ok event is yet to be sent.
    ok_owed: bool,
    /// Open, on a leader: the new leader event of its term is yet to be sent.
    leader_owed: bool,
    /// Open, on the leader: the reason of the session close event the leader has
    /// decided on, and whether it is in the log yet.
    closing: Option<CloseReason>,
    close_appended: bool,
    /// Open, on the leader, closing for the timeout: the closed event is yet to be
    /// sent. Owed from when the log takes the session close event, never before,
    /// until the publication takes it or the committed close frees the slot.
    closed_owed: bool,
    /// When the connect was accepted, or the session opened.
    since_ns: u64,
    /// When the leader last heard from the client, or its election closed.
    activity_ns: u64,
    channel: [u8; MAX_RESPONSE_CHANNEL_LENGTH],
    channel_length: usize,
}

impl Session {
    const FREE: Self = Self {
        state: SessionState::Closed,
        id: -1,
        correlation_id: -1,
        egress: false,
        appended: false,
        ok_owed: false,
        leader_owed: false,
        closing: None,
        close_appended: false,
        closed_owed: false,
        since_ns: 0,
        activity_ns: 0,
        channel: [0; MAX_RESPONSE_CHANNEL_LENGTH],
        channel_length: 0,
    };

    fn channel(&self) -> &[u8] {
        self.channel.get(..self.channel_length).unwrap_or(&[])
    }

    /// Stores `channel`; `false`, and nothing stored, when it is longer than a
    /// session holds.
    fn set_channel(&mut self, channel: &[u8]) -> bool {
        let Some(held) = self.channel.get_mut(..channel.len()) else {
            return false;
        };
        held.copy_from_slice(channel);
        self.channel_length = channel.len();
        true
    }
}

/// The detail of a pending answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Detail {
    /// The ingress endpoints of every member: a redirect's.
    Endpoints,
    /// A refusal's reason.
    Reason(&'static [u8]),
}

/// A redirect or a refusal the member has yet to send, on the egress publication in
/// the slot `MAX_SESSIONS` + its place.
#[derive(Clone, Copy, Debug)]
struct Answer {
    correlation_id: i64,
    code: EventCode,
    detail: Detail,
    since_ns: u64,
}

/// What a session connect comes to, before anything is added or appended.
enum Outcome {
    Answer(EventCode, Detail),
    Accept,
}

/// One cluster member's sessions. See the module's page for how it is driven.
#[derive(Debug)]
pub struct Sessions {
    member_id: i32,
    /// `<id>=<host:port>` of every member's ingress endpoint, in the members string's
    /// order, separated by `,`: a redirect's detail and a new leader event's.
    ingress_endpoints: Box<[u8]>,
    sessions: [Session; MAX_SESSIONS],
    answers: [Option<Answer>; MAX_PENDING_ANSWERS],
    /// The id the next session the leader accepts is given: one more than the
    /// highest it has given or read in a session open event.
    next_session_id: i64,
    /// Whether the member led at its last cycle, and in which term.
    led: Option<i64>,
    counts: SessionCounts,
}

impl Sessions {
    /// The sessions of the member with `member_id`, none open. `ingress_endpoints` is
    /// what a redirect and a new leader event carry: `<id>=<host:port>` for every
    /// member in the members string's order, separated by `,`.
    pub fn new(member_id: i32, ingress_endpoints: &[u8]) -> Self {
        Self {
            member_id,
            ingress_endpoints: ingress_endpoints.into(),
            sessions: [Session::FREE; MAX_SESSIONS],
            answers: [None; MAX_PENDING_ANSWERS],
            next_session_id: 0,
            led: None,
            counts: SessionCounts::default(),
        }
    }

    /// What the member has counted.
    pub fn counts(&self) -> SessionCounts {
        self.counts
    }

    /// How many sessions are open: `cluster.sessions`.
    pub fn open(&self) -> usize {
        self.sessions
            .iter()
            .filter(|session| session.state == SessionState::Open)
            .count()
    }

    /// Whether the session with `id` is open.
    pub fn is_open(&self, id: i64) -> bool {
        self.sessions
            .iter()
            .any(|session| session.state == SessionState::Open && session.id == id)
    }

    /// How many redirects and refusals the member has yet to send.
    pub fn pending_answers(&self) -> usize {
        self.answers.iter().flatten().count()
    }

    /// One message of the ingress stream, which its reader parsed
    /// (`spec/cluster.md`, "Sessions"). A session connect is answered, accepted or
    /// dropped; a session message of an open session is appended to the log with the
    /// leader's term and the cluster's time; a keepalive is the session's activity; a
    /// session close begins the session's close. Whatever the leader does not act on
    /// is counted.
    ///
    /// [`Ingress::Unread`] asks the reader to leave the message where it is: a
    /// session message or a session close whose entry the back-pressured log did not
    /// take, and a connect that reached a member whose election as leader has not
    /// closed yet.
    pub fn on_ingress<I: SessionIo>(
        &mut self,
        clock: Clock,
        lead: Leadership,
        message: &Message<'_>,
        io: &mut I,
    ) -> Result<Ingress, I::Fault> {
        match *message {
            Message::SessionConnect {
                correlation_id,
                response_stream_id,
                version,
                response_channel,
                ..
            } => self.on_connect(
                clock,
                lead,
                correlation_id,
                response_stream_id,
                version,
                response_channel,
                io,
            ),
            Message::SessionMessage {
                leadership_term_id,
                cluster_session_id,
                payload,
                ..
            } => {
                let Some(index) = self.acting(lead, leadership_term_id, cluster_session_id) else {
                    self.counts.ingress_ignored = self.counts.ingress_ignored.saturating_add(1);
                    return Ok(Ingress::Taken);
                };
                let appended = io.append(&Message::SessionMessage {
                    leadership_term_id: lead.leadership_term_id,
                    cluster_session_id,
                    timestamp: clock.time_ms,
                    payload,
                })?;
                if !appended {
                    return Ok(Ingress::Unread);
                }
                if let Some(session) = self.sessions.get_mut(index) {
                    session.activity_ns = clock.now_ns;
                }
                Ok(Ingress::Taken)
            }
            Message::SessionKeepalive {
                leadership_term_id,
                cluster_session_id,
            } => {
                match self.acting(lead, leadership_term_id, cluster_session_id) {
                    Some(index) => {
                        if let Some(session) = self.sessions.get_mut(index) {
                            session.activity_ns = clock.now_ns;
                        }
                    }
                    None => {
                        self.counts.ingress_ignored = self.counts.ingress_ignored.saturating_add(1);
                    }
                }
                Ok(Ingress::Taken)
            }
            Message::SessionClose {
                leadership_term_id,
                cluster_session_id,
            } => {
                let Some(index) = self.acting(lead, leadership_term_id, cluster_session_id) else {
                    self.counts.ingress_ignored = self.counts.ingress_ignored.saturating_add(1);
                    return Ok(Ingress::Taken);
                };
                let appended = io.append(&Message::SessionCloseEvent {
                    leadership_term_id: lead.leadership_term_id,
                    cluster_session_id,
                    timestamp: clock.time_ms,
                    close_reason: CloseReason::ClientAction,
                })?;
                if !appended {
                    return Ok(Ingress::Unread);
                }
                // The client asked: it is sent nothing more, what it was owed
                // included.
                if let Some(session) = self.sessions.get_mut(index) {
                    session.closing = Some(CloseReason::ClientAction);
                    session.close_appended = true;
                    session.ok_owed = false;
                    session.leader_owed = false;
                }
                Ok(Ingress::Taken)
            }
            // The ingress stream carries no other template, and its reader rejects
            // one before this is called; counted here all the same.
            _ => {
                self.counts.ingress_ignored = self.counts.ingress_ignored.saturating_add(1);
                Ok(Ingress::Taken)
            }
        }
    }

    /// One entry of the member's own log, in log order and at or below the commit
    /// position: a session open event opens its session and a session close event
    /// closes it, on every member; any other entry is none of the sessions'.
    ///
    /// On the leader that accepted the connect, the session open event is what the
    /// ok event waits for. A session open event a leader reads that it did not write,
    /// an entry of the term before its own, opens a session it takes up at once: a
    /// publication, and the new leader event. A session open event with the table full, or for an id
    /// that is open, and a session close event for a session that is not open, are
    /// entries no leader of this version writes: nothing is done with one, and the
    /// answer is `false`, for the caller to count as a message rejected.
    pub fn on_entry<I: SessionIo>(
        &mut self,
        clock: Clock,
        entry: &Message<'_>,
        io: &mut I,
    ) -> Result<bool, I::Fault> {
        match *entry {
            Message::SessionOpenEvent {
                correlation_id,
                cluster_session_id,
                response_channel,
                ..
            } => {
                self.next_session_id = self
                    .next_session_id
                    .max(cluster_session_id.saturating_add(1));
                if self.is_open(cluster_session_id) {
                    return Ok(false);
                }
                // The leader's own connecting session, or a free slot.
                let connecting = self.sessions.iter().position(|session| {
                    session.state == SessionState::Connecting && session.id == cluster_session_id
                });
                let index = match connecting {
                    Some(index) => index,
                    None => match self.free_session() {
                        Some(index) => index,
                        None => return Ok(false),
                    },
                };
                let leads = self.led.is_some();
                let Some(session) = self.sessions.get_mut(index) else {
                    return Ok(false);
                };
                if connecting.is_none() {
                    *session = Session::FREE;
                    if !session.set_channel(response_channel) {
                        return Ok(false);
                    }
                    session.id = cluster_session_id;
                    session.correlation_id = correlation_id;
                    // An entry this leader did not write: a leader before it accepted
                    // the connect, and the entry is committed in this leader's term.
                    // The session is taken up as the ones the election found open.
                    if leads {
                        session.egress = io.add_egress(index, response_channel)?;
                        session.leader_owed = true;
                    }
                }
                session.state = SessionState::Open;
                session.ok_owed = connecting.is_some();
                session.since_ns = clock.now_ns;
                session.activity_ns = clock.now_ns;
                self.counts.opened = self.counts.opened.saturating_add(1);
                Ok(true)
            }
            Message::SessionCloseEvent {
                cluster_session_id, ..
            } => {
                let Some(index) = self.sessions.iter().position(|session| {
                    session.state == SessionState::Open && session.id == cluster_session_id
                }) else {
                    return Ok(false);
                };
                self.free(index, io)?;
                self.counts.closed = self.counts.closed.saturating_add(1);
                Ok(true)
            }
            _ => Ok(true),
        }
    }

    /// One duty cycle. On a member that leads: the session open and close events the
    /// back-pressured log did not take are appended, the ok events and new leader
    /// events owed are sent, and a session with nothing from its client for the
    /// session timeout is closed, its session close event appended and then, once
    /// the log holds it, its closed event sent, tried on each cycle until the
    /// publication takes it or the committed close frees the session. From when the
    /// leader decides a session's close, it sends the session nothing it still owed
    /// but that closed event. On every member: the redirects and refusals owed
    /// are sent, and one unsent at the session connect timeout is dropped. A member
    /// that stops leading gives up the connects it had accepted and every egress
    /// publication of a session; a member that begins to lead a term takes up the
    /// open sessions, each with a publication, a new leader event and the session
    /// timeout counted from this cycle. Returns how much it did.
    pub fn do_work<I: SessionIo>(
        &mut self,
        clock: Clock,
        lead: Leadership,
        io: &mut I,
    ) -> Result<usize, I::Fault> {
        let mut work = 0;
        let leading = lead.leads.then_some(lead.leadership_term_id);
        if leading != self.led {
            if self.led.is_some() {
                work += self.step_down(io)?;
            }
            if leading.is_some() {
                work += self.take_up(clock, io)?;
            }
            self.led = leading;
        }
        if lead.leads {
            for index in 0..MAX_SESSIONS {
                work += self.lead_session(index, clock, lead, io)?;
            }
        }
        for index in 0..MAX_PENDING_ANSWERS {
            work += self.answer(index, clock, lead, io)?;
        }
        Ok(work)
    }

    /// The place of the open session with `id` that a leader acts for: `None` on a
    /// member that does not lead, for a term that is not the leader's, and for a
    /// session that is not open or is closing.
    fn acting(&self, lead: Leadership, term: i64, id: i64) -> Option<usize> {
        if !lead.leads || term != lead.leadership_term_id {
            return None;
        }
        self.sessions.iter().position(|session| {
            session.state == SessionState::Open && session.id == id && session.closing.is_none()
        })
    }

    fn free_session(&self) -> Option<usize> {
        self.sessions
            .iter()
            .position(|session| session.state == SessionState::Closed)
    }

    /// Frees the slot at `index` and removes its egress publication.
    fn free<I: SessionIo>(&mut self, index: usize, io: &mut I) -> Result<(), I::Fault> {
        let Some(session) = self.sessions.get_mut(index) else {
            return Ok(());
        };
        let held = session.egress;
        *session = Session::FREE;
        if held {
            io.remove_egress(index)?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn on_connect<I: SessionIo>(
        &mut self,
        clock: Clock,
        lead: Leadership,
        correlation_id: i64,
        response_stream_id: i32,
        version: i32,
        response_channel: &[u8],
        io: &mut I,
    ) -> Result<Ingress, I::Fault> {
        // A channel no publication can be added on is one the driver refuses: the
        // connect is dropped, whoever it reached.
        let Ok(Channel::Udp(channel)) = Channel::parse(response_channel) else {
            self.counts.connects_dropped = self.counts.connects_dropped.saturating_add(1);
            return Ok(Ingress::Taken);
        };
        let outcome = if !lead.leads {
            if lead.leader_member_id == self.member_id {
                // This member's election as leader has not closed: the connect waits
                // for it, and a redirect to this member would only come back.
                return Ok(Ingress::Unread);
            }
            Outcome::Answer(EventCode::Redirect, Detail::Endpoints)
        } else if version >> 16 != PROTOCOL_VERSION >> 16 {
            Outcome::Answer(EventCode::Error, Detail::Reason(INVALID_CLIENT_VERSION))
        } else if response_stream_id != EGRESS_STREAM_ID
            || response_channel.len() > MAX_RESPONSE_CHANNEL_LENGTH
            || !is_endpoint_alone(&channel)
        {
            Outcome::Answer(EventCode::Error, Detail::Reason(INVALID_RESPONSE_CHANNEL))
        } else if self.free_session().is_none() {
            Outcome::Answer(EventCode::Error, Detail::Reason(CONCURRENT_SESSION_LIMIT))
        } else {
            Outcome::Accept
        };
        match outcome {
            Outcome::Answer(code, detail) => {
                let Some(index) = self.answers.iter().position(Option::is_none) else {
                    self.counts.connects_dropped = self.counts.connects_dropped.saturating_add(1);
                    return Ok(Ingress::Taken);
                };
                if !io.add_egress(MAX_SESSIONS + index, response_channel)? {
                    self.counts.connects_dropped = self.counts.connects_dropped.saturating_add(1);
                    return Ok(Ingress::Taken);
                }
                if code == EventCode::Error {
                    self.counts.refused = self.counts.refused.saturating_add(1);
                }
                if let Some(answer) = self.answers.get_mut(index) {
                    *answer = Some(Answer {
                        correlation_id,
                        code,
                        detail,
                        since_ns: clock.now_ns,
                    });
                }
                Ok(Ingress::Taken)
            }
            Outcome::Accept => {
                let Some(index) = self.free_session() else {
                    return Ok(Ingress::Taken);
                };
                if !io.add_egress(index, response_channel)? {
                    self.counts.connects_dropped = self.counts.connects_dropped.saturating_add(1);
                    return Ok(Ingress::Taken);
                }
                let id = self.next_session_id;
                self.next_session_id = id.saturating_add(1);
                let Some(session) = self.sessions.get_mut(index) else {
                    return Ok(Ingress::Taken);
                };
                *session = Session::FREE;
                // The length was checked against what a session holds above.
                session.set_channel(response_channel);
                session.state = SessionState::Connecting;
                session.id = id;
                session.correlation_id = correlation_id;
                session.egress = true;
                session.since_ns = clock.now_ns;
                session.activity_ns = clock.now_ns;
                self.counts.accepted = self.counts.accepted.saturating_add(1);
                self.append_open(index, clock, lead, io)?;
                Ok(Ingress::Taken)
            }
        }
    }

    /// Appends the session open event of the connecting session at `index`, when the
    /// log takes it.
    fn append_open<I: SessionIo>(
        &mut self,
        index: usize,
        clock: Clock,
        lead: Leadership,
        io: &mut I,
    ) -> Result<bool, I::Fault> {
        let Some(session) = self.sessions.get(index) else {
            return Ok(false);
        };
        let appended = io.append(&Message::SessionOpenEvent {
            leadership_term_id: lead.leadership_term_id,
            correlation_id: session.correlation_id,
            cluster_session_id: session.id,
            timestamp: clock.time_ms,
            response_stream_id: EGRESS_STREAM_ID,
            response_channel: session.channel(),
            encoded_principal: &[],
        })?;
        if appended && let Some(session) = self.sessions.get_mut(index) {
            session.appended = true;
        }
        Ok(appended)
    }

    /// A member that led and no longer does: the connects it accepted and has not
    /// seen committed are given up, each counted as dropped, and the open sessions
    /// stay, the log's, without their publications and without what this leader owed
    /// them.
    fn step_down<I: SessionIo>(&mut self, io: &mut I) -> Result<usize, I::Fault> {
        let mut work = 0;
        for index in 0..MAX_SESSIONS {
            let Some(session) = self.sessions.get_mut(index) else {
                continue;
            };
            match session.state {
                SessionState::Closed => {}
                SessionState::Connecting => {
                    self.free(index, io)?;
                    self.counts.connects_dropped = self.counts.connects_dropped.saturating_add(1);
                    work += 1;
                }
                SessionState::Open => {
                    let held = session.egress;
                    session.egress = false;
                    session.ok_owed = false;
                    session.leader_owed = false;
                    session.closing = None;
                    session.close_appended = false;
                    session.closed_owed = false;
                    if held {
                        io.remove_egress(index)?;
                        work += 1;
                    }
                }
            }
        }
        Ok(work)
    }

    /// A member that begins to lead a term: every open session is given an egress
    /// publication when it has none, is owed the term's new leader event, and has
    /// its session timeout counted from now. A session whose response channel the
    /// driver refuses now has no publication, hears nothing, and is closed by its
    /// timeout.
    fn take_up<I: SessionIo>(&mut self, clock: Clock, io: &mut I) -> Result<usize, I::Fault> {
        let mut work = 0;
        for index in 0..MAX_SESSIONS {
            let Some(session) = self.sessions.get(index) else {
                continue;
            };
            if session.state != SessionState::Open {
                continue;
            }
            let egress = session.egress || io.add_egress(index, session.channel())?;
            if let Some(session) = self.sessions.get_mut(index) {
                session.egress = egress;
                session.leader_owed = true;
                session.activity_ns = clock.now_ns;
                session.closing = None;
                session.close_appended = false;
                session.closed_owed = false;
            }
            work += 1;
        }
        Ok(work)
    }

    /// The leader's cycle for the session at `index`.
    fn lead_session<I: SessionIo>(
        &mut self,
        index: usize,
        clock: Clock,
        lead: Leadership,
        io: &mut I,
    ) -> Result<usize, I::Fault> {
        let Some(session) = self.sessions.get(index).copied() else {
            return Ok(0);
        };
        match session.state {
            SessionState::Closed => Ok(0),
            SessionState::Connecting => {
                if session.appended {
                    return Ok(0);
                }
                if self.append_open(index, clock, lead, io)? {
                    return Ok(1);
                }
                // The log took no session open event for the session connect
                // timeout: the client has reported the connect lost by now.
                if clock.now_ns.saturating_sub(session.since_ns) >= SESSION_CONNECT_TIMEOUT_NS {
                    self.free(index, io)?;
                    self.counts.connects_dropped = self.counts.connects_dropped.saturating_add(1);
                    return Ok(1);
                }
                Ok(0)
            }
            SessionState::Open => {
                let mut work = 0;
                let mut owed = (session.ok_owed, session.leader_owed);
                if owed.0
                    && io.send_egress(
                        index,
                        &Message::SessionEvent {
                            cluster_session_id: session.id,
                            correlation_id: session.correlation_id,
                            leadership_term_id: lead.leadership_term_id,
                            leader_member_id: lead.leader_member_id,
                            code: EventCode::Ok,
                            version: PROTOCOL_VERSION,
                            leader_heartbeat_timeout_ns: LEADER_HEARTBEAT_TIMEOUT_NS as i64,
                            detail: &[],
                        },
                    )
                {
                    owed.0 = false;
                    work += 1;
                }
                if owed.1
                    && io.send_egress(
                        index,
                        &Message::NewLeaderEvent {
                            leadership_term_id: lead.leadership_term_id,
                            cluster_session_id: session.id,
                            leader_member_id: lead.leader_member_id,
                            ingress_endpoints: &self.ingress_endpoints,
                        },
                    )
                {
                    owed.1 = false;
                    work += 1;
                }
                let mut closing = session.closing;
                let mut close_appended = session.close_appended;
                let mut closed_owed = session.closed_owed;
                // Nothing from the client for the session timeout, an ok event it
                // could not be sent among that nothing: the session is closed. What
                // it was still owed is not sent after this decision.
                if closing.is_none()
                    && clock.now_ns.saturating_sub(session.activity_ns) >= SESSION_TIMEOUT_NS
                {
                    closing = Some(CloseReason::Timeout);
                    owed = (false, false);
                    self.counts.timed_out = self.counts.timed_out.saturating_add(1);
                    work += 1;
                }
                // The close event first, as Aeron's `SessionManager.checkSessions`
                // does: the client is told of a timeout only once the log holds it.
                if let Some(close_reason) = closing
                    && !close_appended
                    && io.append(&Message::SessionCloseEvent {
                        leadership_term_id: lead.leadership_term_id,
                        cluster_session_id: session.id,
                        timestamp: clock.time_ms,
                        close_reason,
                    })?
                {
                    close_appended = true;
                    closed_owed = close_reason == CloseReason::Timeout;
                    work += 1;
                }
                // Owed until the publication takes it, as the ok event is; the
                // committed close frees the slot and ends the debt.
                if closed_owed
                    && io.send_egress(
                        index,
                        &Message::SessionEvent {
                            cluster_session_id: session.id,
                            correlation_id: -1,
                            leadership_term_id: lead.leadership_term_id,
                            leader_member_id: lead.leader_member_id,
                            code: EventCode::Closed,
                            version: PROTOCOL_VERSION,
                            leader_heartbeat_timeout_ns: LEADER_HEARTBEAT_TIMEOUT_NS as i64,
                            detail: TIMEOUT_DETAIL,
                        },
                    )
                {
                    closed_owed = false;
                    work += 1;
                }
                if let Some(session) = self.sessions.get_mut(index) {
                    session.ok_owed = owed.0;
                    session.leader_owed = owed.1;
                    session.closing = closing;
                    session.close_appended = close_appended;
                    session.closed_owed = closed_owed;
                }
                Ok(work)
            }
        }
    }

    /// The pending answer at `index`: sent, or dropped at the session connect timeout.
    fn answer<I: SessionIo>(
        &mut self,
        index: usize,
        clock: Clock,
        lead: Leadership,
        io: &mut I,
    ) -> Result<usize, I::Fault> {
        let Some(Some(answer)) = self.answers.get(index).copied() else {
            return Ok(0);
        };
        let slot = MAX_SESSIONS + index;
        let sent = io.send_egress(
            slot,
            &Message::SessionEvent {
                cluster_session_id: -1,
                correlation_id: answer.correlation_id,
                leadership_term_id: lead.leadership_term_id,
                leader_member_id: lead.leader_member_id,
                code: answer.code,
                version: PROTOCOL_VERSION,
                leader_heartbeat_timeout_ns: LEADER_HEARTBEAT_TIMEOUT_NS as i64,
                detail: match answer.detail {
                    Detail::Endpoints => &self.ingress_endpoints,
                    Detail::Reason(reason) => reason,
                },
            },
        );
        let expired = clock.now_ns.saturating_sub(answer.since_ns) >= SESSION_CONNECT_TIMEOUT_NS;
        if !sent && !expired {
            return Ok(0);
        }
        if !sent {
            self.counts.connects_dropped = self.counts.connects_dropped.saturating_add(1);
        }
        if let Some(answer) = self.answers.get_mut(index) {
            *answer = None;
        }
        io.remove_egress(slot)?;
        Ok(1)
    }
}

/// Whether `channel` is `cavalry:udp?endpoint=<host:port>` and names nothing else:
/// what a session's response channel is (`spec/cluster.md`, "Session connect,
/// template 3").
fn is_endpoint_alone(channel: &cavalry_proto::channel::UdpChannel<'_>) -> bool {
    channel
        .endpoint()
        .is_some_and(|endpoint| endpoint.port().is_some())
        && channel.interface().is_none()
        && channel.control().is_none()
        && channel.control_mode().is_none()
        && !channel.names_flow_control()
        && !channel.names_ssc()
        && !channel.names_term_length()
        && !channel.names_position()
}
