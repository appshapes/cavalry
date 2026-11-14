//! A cluster's service, as a program writes one (`spec/cluster.md`, "The service"; ADR
//! 0118): the committed log of the member on this client's driver, each entry handed
//! over as what it is, and a reply to a session. In Aeron, this is the
//! `ClusteredService` and its `ClientSession` (`docs/glossary.md`).
//!
//! [`ClusterService::poll`] reads the member's committed log through a
//! [`CommittedLog`], across the member's recordings, and no further than the commit
//! position. It follows the sessions the log opens and closes and the member that
//! leads each term, both from the log's own entries. For the entries of a term its
//! member leads it holds an egress publication to each open session's response
//! channel, and [`Replies::reply`] answers a session through one. A service beside a
//! follower runs the same code on the same entries, reaches the same state and sends
//! nothing.

use std::sync::Arc;

use cavalry_proto::cluster::{
    CloseReason, EGRESS_STREAM_ID, KEEPALIVE_INTERVAL_NS, MAX_SESSIONS, Message, Stream,
};
use cavalry_proto::frame::{self, DataFrameHeader, Flags};
use cavalry_proto::udp::MTU_LENGTH_DEFAULT;
use cavalry_shm::counters::generated::{
    CLUSTER_APPEND_POSITION, CLUSTER_LEADER_MEMBER_ID, CLUSTER_LEADERSHIP_TERM_ID,
    CLUSTER_REPLIES_DROPPED,
};
use cavalry_shm::counters::{CounterId, CountersReader};
use cavalry_shm::log::Action;

use crate::client::Cavalry;
use crate::cluster::{ClusterError, SESSION_MESSAGE_HEADER_LENGTH, send};
use crate::committed_log::{CommittedLog, MemberKey, find, is_members};
use crate::conductor::nano_time;
use crate::counter::Counter;
use crate::error::ClientError;
use crate::publication::{Offer, Publication};

/// The longest response channel a session has: `cavalry:udp?endpoint=`, a host name
/// of 253 bytes, `:` and a port of five digits (`spec/cluster.md`, "Sessions").
const MAX_RESPONSE_CHANNEL_LENGTH: usize = 21 + 253 + 1 + 5;

/// The longest payload a reply carries: what one frame of a session's egress stream
/// carries after the frame header and the session message header, 1,344 bytes
/// (`spec/cluster.md`, "Replies"). A member accepts a response channel that names an
/// endpoint and nothing else, so the stream's MTU length is always the UDP
/// transport's, 1,408 bytes.
const MAX_REPLY_LENGTH: usize =
    MTU_LENGTH_DEFAULT as usize - frame::HEADER_LENGTH - SESSION_MESSAGE_HEADER_LENGTH;

/// How long a service goes on offering a reply to a session whose response channel
/// does not take it: the keepalive interval, 500 ms, which is how often a client
/// reads its session at the least (`spec/cluster.md`, "The service" and "Values").
const REPLY_TIMEOUT_NS: i64 = KEEPALIVE_INTERVAL_NS as i64;

/// How many times one reply is offered again at once when the term of the session's
/// publication rotated under it: a rotation is over by the next offer.
const ROTATIONS: usize = 2;

/// One entry of the committed log, as what it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Entry<'a> {
    /// A session's message: what a client offered, or, with a session id of −1, what
    /// the leader's own process appended for no session.
    SessionMessage {
        /// The session that sent it, or −1.
        session_id: i64,
        /// The client's message.
        payload: &'a [u8],
    },
    /// A session opened.
    SessionOpen {
        /// The id the leader gave it.
        session_id: i64,
    },
    /// A session closed.
    SessionClose {
        /// The session.
        session_id: i64,
        /// Whether the leader closed it for the session timeout, and not the client.
        timed_out: bool,
    },
    /// A leadership term began: the first entry of every term.
    NewLeadershipTerm {
        /// The member that leads it.
        leader_member_id: i32,
    },
    /// An entry of the page that no leader of this version appends, and that a
    /// service of this version does nothing with: a timer event, a cluster action or
    /// a membership change event.
    Other,
}

/// A committed entry with where and when it is in the log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Committed<'a> {
    /// The entry.
    pub entry: Entry<'a>,
    /// The position the entry begins at.
    pub position: i64,
    /// The leadership term it was appended in.
    pub leadership_term_id: i64,
    /// The cluster's time when its leader appended it, milliseconds since
    /// 1970-01-01T00:00:00Z: the one clock a service reads, so that every member's
    /// service reaches the same state.
    pub timestamp: i64,
}

/// What became of a reply ([`Replies::reply`]).
///
/// A service must not let its state depend on which it was: the service beside every
/// other member, which reads the same entry, is answered [`Reply::NotAnswering`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use = "a reply that was not taken is worth Action::Abort: see Reply::Retry"]
pub enum Reply {
    /// The reply is on its way to the session. It is the answer too for a reply that
    /// an earlier handling of the same entry sent: an entry that is handed over again
    /// makes its replies again, and the service does not offer a session one it has
    /// taken already.
    Sent,
    /// This service does not answer the entry. Its member did not lead the entry's
    /// term, so the service beside the member that did answers. Or the entry is one
    /// the member held before this service first opened its log, which a service
    /// before this one answered. Or its member led the term and does not lead it
    /// now: it holds an election, follows another member or leads a later term.
    /// Then no service answers the entry, so that no reply of a term comes after the
    /// next leader's service has answered, and the client learns by its own wait.
    /// There is nothing to do.
    NotAnswering,
    /// The session's response channel did not take the reply now: the client's
    /// stream is not connected yet, or it is back-pressured. Return
    /// [`Action::Abort`] and the entry is handed over again at the next poll, with
    /// everything the handler does for it, so reply before changing any state. The
    /// handler then makes the same replies in the same order, as it does on every
    /// member: those the earlier handling settled are not offered again, so no
    /// session is sent a reply twice, and a reply to this session that comes after
    /// the refused one in the same handling is `Retry` too and is not offered, so
    /// that this service's replies to a session keep their order. A service never waits here for
    /// longer than the keepalive interval, 500 ms, for one session: after that the
    /// answer is [`Reply::Dropped`]. A handler that goes on past a `Retry` gives the
    /// reply up, and the wait ends with the entry.
    Retry,
    /// The reply is lost, and counted in `cluster.replies_dropped`: the session's
    /// response channel has taken nothing for the keepalive interval, 500 ms, since
    /// it first refused a reply, or the driver refused the channel, or the stream to
    /// the client has ended. Go on with the log: the client learns of it by its own
    /// wait for a reply. Each reply to that session is dropped at once until its
    /// channel takes one again. A reply dropped by one handling of an entry is
    /// `Dropped` again, and counted no more, when the entry is handed over again, if
    /// it is among the first 64 replies the entry makes its session; one after
    /// those is `Sent` then, and is not offered either.
    Dropped,
    /// No session with that id is open in the log at this entry.
    NoSession,
    /// The reply is longer than one frame carries after the session message header,
    /// 1,344 bytes. It is refused before the service counts or offers it, so it is
    /// `TooLong` each time its entry is handed over, and it is not one of the
    /// session's replies that the service counts: the reply after it is the next
    /// one, in every handling alike, and is offered once.
    TooLong {
        /// The payload's length.
        found: usize,
        /// The longest payload a reply carries: 1,344 bytes.
        limit: usize,
    },
}

/// A session the log holds open, and the publication the service answers it on for
/// the entries of a term its member leads.
struct Session {
    open: bool,
    id: i64,
    channel: [u8; MAX_RESPONSE_CHANNEL_LENGTH],
    channel_length: usize,
    publication: Option<Publication>,
    /// The driver refused the channel: it is not asked again.
    refused: bool,
    /// The service's nano clock when the publication first refused a reply since it
    /// last took one, or 0.
    refusing_since_ns: i64,
    /// The publication refused for the reply timeout: a reply to it is offered once.
    unresponsive: bool,
    /// The position of the entry the four fields below are about: the entry being
    /// handed over, or the one handed over last.
    entry_position: i64,
    /// How many replies to this session the handlings of that entry have settled,
    /// sent or dropped. They are the first replies its handler makes the session,
    /// and a handling that comes after does not offer them again.
    settled: u32,
    /// Which of the first 64 of those were dropped, the first reply as bit 0.
    dropped: u64,
    /// How many replies the present handling has made to this session.
    made: u32,
    /// The present handling was refused a reply to this session. The replies it
    /// makes the session after that one wait behind it.
    held: bool,
}

impl Session {
    fn closed() -> Self {
        Self {
            open: false,
            id: -1,
            channel: [0; MAX_RESPONSE_CHANNEL_LENGTH],
            channel_length: 0,
            publication: None,
            refused: false,
            refusing_since_ns: 0,
            unresponsive: false,
            entry_position: -1,
            settled: 0,
            dropped: 0,
            made: 0,
            held: false,
        }
    }

    /// Begins a handling of the entry at `position`: what the entry's earlier
    /// handlings settled is kept, and what an earlier entry's did is forgotten.
    fn begin(&mut self, position: i64) {
        if self.entry_position != position {
            self.entry_position = position;
            self.settled = 0;
            self.dropped = 0;
        }
        self.made = 0;
        self.held = false;
    }

    /// Ends a handling whose entry was taken. A reply that handling was refused and
    /// went on past is given up, and the session's wait with it, unless the wait
    /// had already run out.
    fn taken(&mut self) {
        if self.held && !self.unresponsive {
            self.refusing_since_ns = 0;
        }
        self.held = false;
    }

    /// Settles the reply the present handling has just made: sent, or dropped.
    fn settle(&mut self, dropped: bool) {
        if dropped && let Some(bit) = 1u64.checked_shl(self.made.saturating_sub(1)) {
            self.dropped |= bit;
        }
        self.settled = self.made;
    }

    /// Adds the session's egress publication, when it has none: a command to the
    /// driver. A channel the driver refuses leaves the session with none, and its
    /// replies are dropped.
    fn connect(&mut self, client: &mut Cavalry) -> Result<(), ClientError> {
        if self.publication.is_some() || self.refused {
            return Ok(());
        }
        let Some(Ok(channel)) = self
            .channel
            .get(..self.channel_length)
            .map(core::str::from_utf8)
        else {
            self.refused = true;
            return Ok(());
        };
        match client.add_exclusive_publication(channel, EGRESS_STREAM_ID) {
            Ok(publication) => {
                self.publication = Some(publication);
                Ok(())
            }
            Err(ClientError::Refused { .. }) => {
                self.refused = true;
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    /// Gives the session's publication back, when it has one.
    fn disconnect(&mut self) {
        self.publication = None;
        self.refused = false;
        self.refusing_since_ns = 0;
        self.unresponsive = false;
    }
}

/// What the service found of its member when it first opened the member's log, and
/// where it reads what the member says of itself now.
struct Member {
    id: i32,
    key: MemberKey,
    counters: Arc<CountersReader>,
    /// The member's leadership term then. An entry of a term below it is history.
    leadership_term_id: i64,
    /// The member's append position then. An entry of that term below it is history.
    append_position: i64,
    /// The records `cluster.leader_member_id` and `cluster.leadership_term_id` were
    /// last found in: `None` until the first load, and after a load that found the
    /// record no longer the member's.
    leader_record: Option<u32>,
    term_record: Option<u32>,
    /// `cluster.replies_dropped`, which the service adds.
    dropped: Counter,
}

impl Member {
    /// Reads the member's term and its append position, in that order, and adds the
    /// service's counter. `None` when the member's counters are no longer there to
    /// read: the member closed.
    fn read(client: &mut Cavalry, key: MemberKey) -> Result<Option<Self>, ClientError> {
        let counters = client.shared_counters();
        let mut term_record = None;
        let Some(leadership_term_id) =
            load(&counters, &mut term_record, CLUSTER_LEADERSHIP_TERM_ID, key)
        else {
            return Ok(None);
        };
        let Some(append_position) = load(&counters, &mut None, CLUSTER_APPEND_POSITION, key) else {
            return Ok(None);
        };
        let id = i32::from_le_bytes(key);
        let dropped = client.add_counter(
            CLUSTER_REPLIES_DROPPED.0,
            &key,
            &format!("cluster.replies_dropped {id}"),
        )?;
        Ok(Some(Self {
            id,
            key,
            counters,
            leadership_term_id,
            append_position,
            leader_record: None,
            term_record,
            dropped,
        }))
    }

    /// Whether the member says, now, that it leads `leadership_term_id`: its
    /// `cluster.leader_member_id` holds its own id, and then its
    /// `cluster.leadership_term_id` holds that term (`spec/cluster.md`, "Replies").
    ///
    /// The order of the two loads is the reverse of the member's stores. A member
    /// stores its term before the id of the member it leads as or follows, and both
    /// before a commit position, in every duty cycle. So a service that has read an
    /// entry below a commit position it loaded, and then loads the id and then the
    /// term, never takes a member that leads a later term, or follows in one, for the
    /// leader of this one: the later term was stored before the id that says so.
    ///
    /// Two acquire loads and two reads of a record, with no allocation. A member that
    /// started again has added its counters anew, so a record that is no longer the
    /// member's is looked for again, once.
    fn leads(&mut self, leadership_term_id: i64) -> bool {
        let leader = load(
            &self.counters,
            &mut self.leader_record,
            CLUSTER_LEADER_MEMBER_ID,
            self.key,
        );
        leader == Some(i64::from(self.id))
            && load(
                &self.counters,
                &mut self.term_record,
                CLUSTER_LEADERSHIP_TERM_ID,
                self.key,
            ) == Some(leadership_term_id)
    }
}

/// The value of the counter of type `type_id` of the member whose key is `key`, from
/// a record that is the member's after the load, as the commit position is read
/// (`spec/cluster.md`, "The service"). `record` is where the counter was last found.
/// `None` when the member has no such counter: it closed.
fn load(
    counters: &CountersReader,
    record: &mut Option<u32>,
    type_id: CounterId,
    key: MemberKey,
) -> Option<i64> {
    let read = |id: u32| {
        let value = counters.value(id).ok()?.load_acquire();
        is_members(counters, id, type_id, key).then_some(value)
    };
    // At the record it was last found in, when there is one.
    if let Some(value) = record.and_then(read) {
        return Some(value);
    }
    // And where it is now: a member that started again added its counters anew.
    *record = None;
    let (id, _) = find(counters, type_id, Some(key))?;
    let value = read(id)?;
    *record = Some(id);
    Some(value)
}

/// What a service's handler answers a session with: handed to the handler beside each
/// entry.
pub struct Replies<'a> {
    sessions: &'a mut [Session; MAX_SESSIONS],
    dropped: &'a Counter,
    /// Whether this service answers the entry being handled.
    answering: bool,
    leadership_term_id: i64,
    timestamp: i64,
}

impl Replies<'_> {
    /// Sends `payload` to the session with `session_id`, as a session message on the
    /// egress stream with the term and the cluster's time of the entry being handled
    /// (`spec/cluster.md`, "The service"). The session need not be the one whose
    /// message is being handled: any session open at this entry can be sent to, and
    /// more than one. A session is open from its own open event to its own close
    /// event, both included, so a handler may send it a welcome at
    /// [`Entry::SessionOpen`].
    ///
    /// [`Reply`] says what became of it, and [`Reply::Retry`] is the one answer that
    /// asks something of the handler. Nothing here allocates, and nothing waits. A
    /// reply that is sent reads no clock; one that is refused reads it once, to
    /// measure the keepalive interval.
    ///
    /// A reply is offered to its session once, however often its entry is handed
    /// over. The service counts, for each session, the replies that the handlings of
    /// the entry have settled, sent or dropped, and answers them as they were
    /// without offering them again. A reply that is [`Reply::TooLong`] is refused
    /// before it is counted, so it moves no other reply's place in that count. It
    /// relies on what the replay check asks of every handler: the same replies, in
    /// the same order, each time an entry is handed over.
    pub fn reply(&mut self, session_id: i64, payload: &[u8]) -> Reply {
        if !self.answering {
            return Reply::NotAnswering;
        }
        let Some(session) = self
            .sessions
            .iter_mut()
            .find(|session| session.open && session.id == session_id)
        else {
            return Reply::NoSession;
        };
        if payload.len() > MAX_REPLY_LENGTH {
            // Refused before it is counted, in every handling alike: it is not one
            // of the session's replies, so it takes no place among those that the
            // handlings of the entry settle.
            return Reply::TooLong {
                found: payload.len(),
                limit: MAX_REPLY_LENGTH,
            };
        }
        session.made = session.made.saturating_add(1);
        if session.made <= session.settled {
            // An earlier handling of this entry settled this reply: it is not
            // offered again, and it is answered as it was then.
            let was_dropped = 1u64
                .checked_shl(session.made.saturating_sub(1))
                .is_some_and(|bit| session.dropped & bit != 0);
            return if was_dropped {
                Reply::Dropped
            } else {
                Reply::Sent
            };
        }
        if session.held {
            // Behind a reply to this session that this handling was refused.
            return Reply::Retry;
        }
        let Some(publication) = session.publication.as_mut() else {
            // The driver refused the session's channel.
            session.settle(true);
            let _ = self.dropped.increment();
            return Reply::Dropped;
        };
        let message = Message::SessionMessage {
            leadership_term_id: self.leadership_term_id,
            cluster_session_id: session_id,
            timestamp: self.timestamp,
            payload,
        };
        let mut sent = send(publication, &message);
        for _ in 0..ROTATIONS {
            if sent != Err(Offer::AdminAction) {
                break;
            }
            sent = send(publication, &message);
        }
        match sent {
            Ok(_) => {
                session.refusing_since_ns = 0;
                session.unresponsive = false;
                session.settle(false);
                Reply::Sent
            }
            Err(Offer::NotConnected | Offer::BackPressured | Offer::AdminAction) => {
                // The one clock read, and only for a reply that was refused: a reply
                // that is sent reads none.
                let now_ns = nano_time();
                if session.refusing_since_ns == 0 {
                    session.refusing_since_ns = now_ns.max(1);
                }
                if !session.unresponsive
                    && now_ns.saturating_sub(session.refusing_since_ns) < REPLY_TIMEOUT_NS
                {
                    session.held = true;
                    return Reply::Retry;
                }
                session.unresponsive = true;
                session.settle(true);
                let _ = self.dropped.increment();
                Reply::Dropped
            }
            // `TooLong` here is a publication whose frame carries less than
            // `MAX_REPLY_LENGTH` bytes, which no session has: a member refuses a
            // response channel that sets its own MTU length. Were one to, the reply
            // would be lost for good, so it is dropped and counted, and keeps its
            // place among the session's settled replies.
            Err(Offer::MaxPositionExceeded | Offer::Closed | Offer::TooLong { .. }) => {
                session.settle(true);
                let _ = self.dropped.increment();
                Reply::Dropped
            }
        }
    }
}

/// A cluster's service beside one member: the handle a program reads the committed
/// log through and answers sessions with.
///
/// One thread uses a service, with the client it was made from.
pub struct ClusterService {
    log: Option<CommittedLog>,
    /// The position after the last entry the handler took.
    position: i64,
    member: Option<Member>,
    /// The term of the last new leadership term event read, and the member that
    /// leads it: (−1, −1) before the first.
    term_leader: (i64, i32),
    /// Whether the service answered the last entry it read: it holds the sessions'
    /// publications while it does.
    answering: bool,
    sessions: Box<[Session; MAX_SESSIONS]>,
}

impl ClusterService {
    /// A service that reads its member's log from position 0.
    pub(crate) fn new() -> Self {
        Self {
            log: None,
            position: 0,
            member: None,
            term_leader: (-1, -1),
            answering: false,
            sessions: Box::new(core::array::from_fn(|_| Session::closed())),
        }
    }

    /// Reads up to `fragment_limit` committed entries, each handed to `handler` with
    /// the [`Replies`] it may answer a session through. The handler's [`Action`]
    /// says whether the entry is taken: [`Action::Abort`] leaves it for the next
    /// poll, which is what a [`Reply::Retry`] asks for. Returns how many entries
    /// were taken; 0 while the driver hosts no member that has a log yet.
    ///
    /// Which entries a service answers. An entry is answered by one service: the one
    /// beside the member that led the entry's term, which the term's new leadership
    /// term event names. That service answers it while its member's counters say the
    /// member leads the term still, loaded as the entry is read: a member that holds
    /// an election, follows another or leads a later term has a service that sends
    /// nothing for the earlier term's entries. And it answers it unless the entry is history: of a
    /// term below its member's leadership term when the service first opened the
    /// member's log, or of that term and below the member's append position then. So
    /// a service that starts again reads its member's log from the start, applies
    /// every entry, and answers no session twice for what a service before it
    /// answered. An entry appended while no service ran beside its leader is
    /// answered by none, and a client is ready for that.
    ///
    /// The poll is the service's duty cycle. It opens the member's log when the
    /// member has one, and again at the next recording when one ends. From the first
    /// entry it answers it holds an egress publication to each open session's
    /// response channel, and from the first it does not, none. Those are commands to
    /// the driver: at a session's opening, where a term's leader changes, and at a
    /// recording's end. Nothing is allocated, and no command is sent, for a session
    /// message. An entry the service would answer costs two loads of the member's
    /// counters.
    pub fn poll<F>(
        &mut self,
        client: &mut Cavalry,
        mut handler: F,
        fragment_limit: usize,
    ) -> Result<usize, ClusterError>
    where
        F: FnMut(&Committed<'_>, &mut Replies<'_>) -> Action,
    {
        if self.log.is_none() {
            let Some(log) = client.committed_log(self.position)? else {
                return Ok(0);
            };
            if self.member.is_none() {
                // After the log's open, so that the append position is one the member
                // stored no earlier than it named the recording.
                match Member::read(client, log.member_key()) {
                    Ok(Some(member)) => self.member = Some(member),
                    Ok(None) => {
                        log.close(client)?;
                        return Ok(0);
                    }
                    Err(error) => {
                        log.close(client)?;
                        return Err(error.into());
                    }
                }
            }
            self.log = Some(log);
        }
        let Self {
            log,
            member,
            term_leader,
            answering,
            sessions,
            ..
        } = self;
        let (Some(log), Some(member)) = (log.as_mut(), member.as_mut()) else {
            return Ok(0);
        };
        let mut failure: Option<ClusterError> = None;
        let polled = log.controlled_poll(
            |bytes: &[u8], header: &DataFrameHeader, position: i64| {
                // An entry is one frame: no leader appends a longer one.
                let parsed = header
                    .flags()
                    .contains(Flags::UNFRAGMENTED)
                    .then(|| Message::parse(Stream::Log, bytes).ok())
                    .flatten();
                let mut opened: Option<(i64, &[u8])> = None;
                let (entry, leadership_term_id, timestamp) = match parsed {
                    Some(Message::SessionMessage {
                        leadership_term_id,
                        cluster_session_id,
                        timestamp,
                        payload,
                    }) => (
                        Entry::SessionMessage {
                            session_id: cluster_session_id,
                            payload,
                        },
                        leadership_term_id,
                        timestamp,
                    ),
                    Some(Message::SessionOpenEvent {
                        leadership_term_id,
                        cluster_session_id,
                        timestamp,
                        response_channel,
                        ..
                    }) => {
                        opened = Some((cluster_session_id, response_channel));
                        (
                            Entry::SessionOpen {
                                session_id: cluster_session_id,
                            },
                            leadership_term_id,
                            timestamp,
                        )
                    }
                    Some(Message::SessionCloseEvent {
                        leadership_term_id,
                        cluster_session_id,
                        timestamp,
                        close_reason,
                    }) => (
                        Entry::SessionClose {
                            session_id: cluster_session_id,
                            timed_out: close_reason == CloseReason::Timeout,
                        },
                        leadership_term_id,
                        timestamp,
                    ),
                    Some(Message::NewLeadershipTermEvent {
                        leadership_term_id,
                        timestamp,
                        leader_member_id,
                        ..
                    }) => {
                        *term_leader = (leadership_term_id, leader_member_id);
                        (
                            Entry::NewLeadershipTerm { leader_member_id },
                            leadership_term_id,
                            timestamp,
                        )
                    }
                    Some(
                        Message::TimerEvent {
                            leadership_term_id,
                            timestamp,
                            ..
                        }
                        | Message::ClusterAction {
                            leadership_term_id,
                            timestamp,
                            ..
                        }
                        | Message::MembershipChangeEvent {
                            leadership_term_id,
                            timestamp,
                            ..
                        },
                    ) => (Entry::Other, leadership_term_id, timestamp),
                    _ => {
                        failure = Some(ClusterError::Entry { position });
                        return Action::Abort;
                    }
                };
                // The session is in the table once, whatever the handler answers: an
                // entry it aborts is handed over again and finds its session here.
                if let Some((session_id, channel)) = opened
                    && !sessions
                        .iter()
                        .any(|session| session.open && session.id == session_id)
                    && let Some(session) = sessions.iter_mut().find(|session| !session.open)
                    && let Some(held) = session.channel.get_mut(..channel.len())
                {
                    held.copy_from_slice(channel);
                    session.channel_length = channel.len();
                    session.id = session_id;
                    session.open = true;
                }
                // Who answers: the service beside the leader of the entry's term, for
                // what is not history.
                let history = leadership_term_id < member.leadership_term_id
                    || (leadership_term_id == member.leadership_term_id
                        && position < member.append_position);
                // And only while the member says it leads the term still: a member
                // that has stopped leading it, or has joined a later term, has a
                // service that would answer late, after the next leader's service
                // answered later entries. Loaded here, after the commit position
                // that let this entry be read.
                let answers = !history
                    && *term_leader == (leadership_term_id, member.id)
                    && member.leads(leadership_term_id);
                if answers != *answering {
                    for session in sessions.iter_mut().filter(|session| session.open) {
                        if !answers {
                            session.disconnect();
                        } else if let Err(error) = session.connect(client) {
                            failure = Some(error.into());
                            return Action::Abort;
                        }
                    }
                    *answering = answers;
                }
                if answers
                    && let Some((session_id, _)) = opened
                    && let Some(session) = sessions
                        .iter_mut()
                        .find(|session| session.open && session.id == session_id)
                    && let Err(error) = session.connect(client)
                {
                    failure = Some(error.into());
                    return Action::Abort;
                }
                // What the entry's earlier handlings settled for each session is kept,
                // so that a reply is offered once however often the entry is read.
                for session in sessions.iter_mut().filter(|session| session.open) {
                    session.begin(position);
                }
                let action = handler(
                    &Committed {
                        entry,
                        position,
                        leadership_term_id,
                        timestamp,
                    },
                    &mut Replies {
                        sessions: &mut *sessions,
                        dropped: &member.dropped,
                        answering: answers,
                        leadership_term_id,
                        timestamp,
                    },
                );
                if action != Action::Abort {
                    for session in sessions.iter_mut().filter(|session| session.open) {
                        session.taken();
                    }
                }
                // A session is closed once its close event is taken, and its
                // publication goes with it.
                if action != Action::Abort
                    && let Entry::SessionClose { session_id, .. } = entry
                    && let Some(session) = sessions
                        .iter_mut()
                        .find(|session| session.open && session.id == session_id)
                {
                    *session = Session::closed();
                }
                action
            },
            fragment_limit,
        );
        self.position = log.position();
        // A handle that has ended, at its recording's stop, is closed: the next poll
        // opens the log again where this one stopped.
        if (polled.is_err() || log.is_ended())
            && let Some(log) = self.log.take()
        {
            log.close(client)?;
        }
        if let Some(error) = failure {
            return Err(error);
        }
        polled.map_err(ClusterError::Log)
    }

    /// The position after the last entry the handler took, 0 before the first.
    pub fn position(&self) -> i64 {
        self.position
    }

    /// Whether the service answered the last entry it read: its member led that
    /// entry's term, and the entry was not history.
    pub fn answers(&self) -> bool {
        self.answering
    }

    /// How many sessions the log holds open, as far as the service has read.
    pub fn sessions(&self) -> usize {
        self.sessions.iter().filter(|session| session.open).count()
    }

    /// Closes the service: its read of the log, its publications and its counter.
    pub fn close(mut self, client: &mut Cavalry) -> Result<(), ClusterError> {
        for session in self.sessions.iter_mut() {
            if let Some(publication) = session.publication.take() {
                publication.close()?;
            }
        }
        if let Some(member) = self.member.take() {
            member.dropped.close();
        }
        if let Some(log) = self.log.take() {
            log.close(client)?;
        }
        Ok(())
    }
}

impl core::fmt::Debug for ClusterService {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ClusterService")
            .field("position", &self.position)
            .field("answers", &self.answers())
            .field("sessions", &self.sessions())
            .finish()
    }
}
