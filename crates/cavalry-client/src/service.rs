//! A cluster's service, as a program writes one (`spec/cluster.md`, "The service"; ADR
//! 0118): the committed log of the member on this client's driver, each entry handed
//! over as what it is, and a reply to a session. In Aeron, this is the
//! `ClusteredService` and its `ClientSession` (`docs/glossary.md`).
//!
//! [`ClusterService::poll`] reads the member's committed log through a
//! [`CommittedLog`], across the member's recordings, and no further than the commit
//! position. It follows the sessions the log opens and closes and the member that
//! leads each term, both from the log's own entries. For the entries of a term its
//! member led, while the member's counters say it leads the term still, it holds an
//! egress publication to each open session's response channel, and
//! [`Replies::reply`] answers a session through one. A service beside a follower
//! runs the same code on the same entries, reaches the same state and sends nothing.
//!
//! A poll ends at an [`Entry::Snapshot`], the leader's request for a snapshot, and
//! [`ClusterService::snapshot`] then writes the program's state as it is at that
//! position, with the sessions that are open there, to a recording of its own, and
//! tells the member, which lists it (`spec/cluster.md`, "Snapshots"; ADR 0131).
//! [`ClusterService::restore`], before the first poll, starts the service from the
//! newest snapshot its member lists: the program is given its state back, and the
//! polls read the log from that snapshot's position (ADR 0132).
//!
//! A program has timers (`spec/cluster.md`, "Timers"; ADR 0151). Inside a handler
//! [`Replies::schedule_timer`] and [`Replies::cancel_timer`] change the service's
//! table of pending timers, and the handler is handed [`Entry::Timer`] for a timer
//! whose event the log holds at or after its deadline, once. The table is a
//! function of the entries the handler took, so every member's service decides the
//! same from the same log. After a poll's entries the service tells the member on
//! its driver what they changed, and every pending timer again each second; the
//! member's leader appends the events. The pending timers are in a snapshot, and
//! a start from one holds them.

use std::sync::Arc;
use std::time::Duration;

use cavalry_proto::client_protocol::{ErrorCode, ReplayStopReason};
use cavalry_proto::cluster::{
    CONSENSUS_MODULE_STREAM_ID, CloseReason, ClusterAction, EGRESS_STREAM_ID,
    KEEPALIVE_INTERVAL_NS, MAX_SERVICE_SNAPSHOT_STATE_LENGTH, MAX_SERVICE_TIMERS, MAX_SESSIONS,
    MAX_UNSENT_TIMER_CANCELS, Message, SERVICE_ID, SERVICE_SNAPSHOT_REPLAY_STREAM_ID,
    SERVICE_SNAPSHOT_STATE_INDEX, SERVICE_SNAPSHOT_STATE_MESSAGE_LENGTH,
    SERVICE_SNAPSHOT_STREAM_ID, SERVICE_SNAPSHOT_TYPE_ID, ServiceTimers, SnapshotMark, Stream,
    TIMER_REFRESH_INTERVAL_NS, TimeUnit, TimerEventIs, TimersFull,
};
use cavalry_proto::frame::{self, DataFrameHeader, Flags};
use cavalry_proto::udp::MTU_LENGTH_DEFAULT;
use cavalry_shm::counters::generated::{
    CLUSTER_APPEND_POSITION, CLUSTER_LEADER_MEMBER_ID, CLUSTER_LEADERSHIP_TERM_ID,
    CLUSTER_REPLIES_DROPPED, CLUSTER_SERVICE_SNAPSHOT_POSITION,
    CLUSTER_SERVICE_SNAPSHOT_RECORDING_ID, CLUSTER_SERVICE_SNAPSHOT_START_POSITION,
    CLUSTER_SERVICE_SNAPSHOTS_FAILED, CLUSTER_SERVICE_SNAPSHOTS_PASSED_OVER,
    CLUSTER_SERVICE_SNAPSHOTS_TAKEN, CLUSTER_SERVICE_TIMER_CANCELS_DROPPED,
    CLUSTER_SERVICE_TIMER_EVENTS_PASSED_OVER, CLUSTER_SERVICE_TIMERS,
};
use cavalry_shm::counters::{CounterId, CountersReader};
use cavalry_shm::log::Action;

use crate::client::Cavalry;
use crate::cluster::{ClusterError, SESSION_MESSAGE_HEADER_LENGTH, SnapshotCause, send};
use crate::committed_log::{CommittedLog, MemberKey, find, is_members, member_with_a_log};
use crate::conductor::nano_time;
use crate::counter::Counter;
use crate::error::ClientError;
use crate::publication::{Offer, Publication};
use crate::recording::Recording;
use crate::{NULL_LENGTH, NULL_POSITION};

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

/// The channel a service's snapshot and its service ack are written on: streams of
/// the member's driver alone.
const IPC_CHANNEL: &str = "cavalry:ipc";

/// How long a snapshot may go without its next step: a message a publication does
/// not take, a recording whose stop the driver does not announce, or a service ack
/// the consensus module stream does not take (`spec/cluster.md`, "Values", snapshot
/// timeout). Past it the snapshot is given up. It is the one a member's own snapshot
/// is under.
const SNAPSHOT_TIMEOUT_NS: i64 = cavalry_proto::cluster::SNAPSHOT_TIMEOUT_NS as i64;

/// How long the service's thread sleeps between two tries of a step of a snapshot
/// that the driver has not caught up with.
const SNAPSHOT_PAUSE: Duration = Duration::from_micros(50);

/// The longest message of a service's snapshot that the service encodes: a client
/// session with the longest response channel, 8 + 12 + 4 + 280 + 4 bytes. The
/// program's bytes are offered as they are.
const SNAPSHOT_SCRATCH_LENGTH: usize = 512;

/// How long a service's start waits for the first message of its snapshot's
/// recording, and for each message after one, before it passes the snapshot over
/// (`spec/cluster.md`, "Values", service snapshot load timeout).
const SNAPSHOT_LOAD_TIMEOUT_NS: i64 = 5_000_000_000;

/// How many messages of its snapshot's recording a service's start reads at once.
const SNAPSHOT_LOAD_LIMIT: usize = 64;

/// The leader of the term a service started in, from a snapshot: it did not read
/// that term's new leadership term event. For an entry of that term the service
/// asks its member's counters alone, which say that the member leads the term
/// only on the member that the event names (`spec/cluster.md`, "A service's
/// start"; ADR 0132).
const ASK_MEMBER: i32 = -2;

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
    /// The leader asks every member for a snapshot here: a cluster action whose
    /// action is snapshot. The poll that hands it over ends after it, so the
    /// program's state is then its state at the snapshot's position, the position
    /// after this entry, and [`ClusterService::snapshot`] takes that state before
    /// the next poll. A program that takes no snapshots reads on.
    Snapshot,
    /// A timer that the program scheduled is due: its timer event, handed over once,
    /// at this position beside every member, with a [`Committed::timestamp`] at or
    /// after the deadline. The timer is pending no more once the handler has taken
    /// the entry, unless the handler schedules the same id again while it handles
    /// it ([`Replies::schedule_timer`]). A timer event whose timer is not pending,
    /// or is pending with a later deadline, is passed over and never handed over
    /// (`spec/cluster.md`, "The service", "Timers"; ADR 0151).
    Timer {
        /// The id the program gave the timer when it scheduled it.
        correlation_id: i64,
    },
    /// An entry of the page that no leader of this version appends, and that a
    /// service of this version does nothing with: a cluster action that suspends or
    /// resumes, or a membership change event.
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

/// What [`ClusterService::restore`] found for a service to start from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Restored {
    /// The driver hosts no cluster member that has a log yet, so there is nothing to
    /// start from. Ask again, as a poll that reads nothing is made again.
    NoMember,
    /// The member lists no snapshot of a service, or the one it lists could not be
    /// taken, which `cluster.service_snapshots_passed_over` counts. The service
    /// reads its member's log from position 0.
    FromStart,
    /// The program's state, as the program gave it to the snapshot. The service
    /// holds the sessions that were open at the snapshot's position, and its polls
    /// read the log from that position.
    State(Vec<u8>),
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
    /// The record `cluster.service_snapshot_position` was last found in.
    service_snapshot_record: Option<u32>,
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
            service_snapshot_record: None,
            dropped,
        }))
    }

    /// The log position of the newest snapshot of a service that the member lists,
    /// −1 while it lists none: its `cluster.service_snapshot_position`. `None` when
    /// the member has no such counter: it closed, or it is of a version that lists
    /// no service's snapshot.
    fn service_snapshot_position(&mut self) -> Option<i64> {
        load(
            &self.counters,
            &mut self.service_snapshot_record,
            CLUSTER_SERVICE_SNAPSHOT_POSITION,
            self.key,
        )
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

/// Writes one snapshot of a service: a recording of a span of the service's
/// publication on the service snapshot stream, stopped at the end of what was
/// written, and then the service ack to the member (`spec/cluster.md`, "A service's
/// snapshot"). A recording that has begun and is given up is asked to stop: it is in
/// no list, and is no snapshot.
fn write_snapshot(
    client: &mut Cavalry,
    snapshots: &mut SnapshotPublications,
    sessions: &[Session; MAX_SESSIONS],
    timers: &ServiceTimers,
    due: Due,
    state: &[u8],
) -> Result<(), ClusterError> {
    // The driver records the stream's first publication that is not yet recorded
    // (`spec/client-protocol.md`, "Commands", start recording). One service beside a
    // member writes on this stream, so that is this service's; a recording of another
    // session is not this snapshot's.
    let recording = client.start_recording(IPC_CHANNEL, SERVICE_SNAPSHOT_STREAM_ID)?;
    if recording.session_id() != snapshots.publication.session_id() {
        let _ = recording.stop(client);
        return Err(ClusterError::SnapshotFailed {
            cause: SnapshotCause::Recording,
        });
    }
    let written = write_messages(&mut snapshots.publication, sessions, timers, due, state)
        .and_then(|end| stop_at(client, &recording, end));
    if let Err(cause) = written {
        if recording.stop_position().is_none() {
            // Asked once and not awaited.
            let _ = recording.stop(client);
        }
        return Err(ClusterError::SnapshotFailed { cause });
    }
    let mut scratch = [0u8; SNAPSHOT_SCRATCH_LENGTH];
    let ack = Message::ServiceAck {
        log_position: due.log_position,
        timestamp: due.timestamp,
        ack_id: 0,
        relevant_id: recording.recording_id(),
        service_id: SERVICE_ID,
    };
    offer_within(&mut snapshots.acks, encoded(&ack, &mut scratch)?)
        .map(|_| ())
        .map_err(|()| ClusterError::SnapshotFailed {
            cause: SnapshotCause::Ack,
        })
}

/// The messages of a service's snapshot, in order, each one message of the
/// publication: a marker, the open sessions by their ids, a marker, the program's
/// bytes and a marker. Returns the position after the last.
fn write_messages(
    publication: &mut Publication,
    sessions: &[Session; MAX_SESSIONS],
    timers: &ServiceTimers,
    due: Due,
    state: &[u8],
) -> Result<i64, SnapshotCause> {
    let mut scratch = [0u8; SNAPSHOT_SCRATCH_LENGTH];
    let marker = |mark, index| Message::SnapshotMarker {
        type_id: SERVICE_SNAPSHOT_TYPE_ID,
        log_position: due.log_position,
        leadership_term_id: due.leadership_term_id,
        index,
        mark,
        time_unit: TimeUnit::Millis,
        app_version: 0,
    };
    let mut offer = |message: &Message<'_>| -> Result<i64, SnapshotCause> {
        let bytes = encoded(message, &mut scratch).map_err(|_| SnapshotCause::Write)?;
        offer_within(publication, bytes).map_err(|()| SnapshotCause::Write)
    };
    offer(&marker(SnapshotMark::Begin, 0))?;
    // The open sessions in the rising order of their ids: the order is the same
    // beside every member, whatever slot of the table a session is in.
    let mut after = i64::MIN;
    while let Some(session) = sessions
        .iter()
        .filter(|session| session.open && session.id >= after)
        .min_by_key(|session| session.id)
    {
        offer(&Message::ClientSession {
            cluster_session_id: session.id,
            response_stream_id: EGRESS_STREAM_ID,
            response_channel: session.channel.get(..session.channel_length).unwrap_or(&[]),
            encoded_principal: &[],
        })?;
        let Some(next) = session.id.checked_add(1) else {
            break;
        };
        after = next;
    }
    // The pending timers, in the rising order of their ids, which is the table's.
    // With none pending the snapshot is byte for byte what version 1 wrote.
    for (correlation_id, deadline) in timers.iter() {
        offer(&Message::Timer {
            correlation_id,
            deadline,
        })?;
    }
    offer(&marker(SnapshotMark::Section, SERVICE_SNAPSHOT_STATE_INDEX))?;
    // The program's bytes as they are: no message of the page, and read by none.
    for piece in state.chunks(SERVICE_SNAPSHOT_STATE_MESSAGE_LENGTH) {
        offer_within(publication, piece).map_err(|()| SnapshotCause::Write)?;
    }
    let bytes =
        encoded(&marker(SnapshotMark::End, 0), &mut scratch).map_err(|_| SnapshotCause::Write)?;
    offer_within(publication, bytes).map_err(|()| SnapshotCause::Write)
}

/// `message` as its bytes in `scratch`.
fn encoded<'a>(message: &Message<'_>, scratch: &'a mut [u8]) -> Result<&'a [u8], ClusterError> {
    let length = message
        .write(scratch)
        .map_err(|_| ClusterError::SnapshotFailed {
            cause: SnapshotCause::Write,
        })?;
    scratch.get(..length).ok_or(ClusterError::SnapshotFailed {
        cause: SnapshotCause::Write,
    })
}

/// Offers `bytes` until the publication takes them, and gives the position after
/// them. `Err` when the publication takes nothing for the snapshot timeout, or
/// refuses the message for good. The clock is read only while it refuses.
fn offer_within(publication: &mut Publication, bytes: &[u8]) -> Result<i64, ()> {
    let mut refusing_since_ns = 0i64;
    loop {
        match publication.offer(bytes) {
            Ok(position) => return Ok(position),
            // A recording connects the publication and gives it its limit a moment
            // after it starts, and the member's subscription connects the acks'.
            Err(Offer::BackPressured | Offer::NotConnected | Offer::AdminAction) => {
                let now = nano_time();
                if refusing_since_ns == 0 {
                    refusing_since_ns = now;
                } else if now.saturating_sub(refusing_since_ns) >= SNAPSHOT_TIMEOUT_NS {
                    return Err(());
                }
                std::thread::sleep(SNAPSHOT_PAUSE);
            }
            Err(Offer::MaxPositionExceeded | Offer::Closed | Offer::TooLong { .. }) => {
                return Err(());
            }
        }
    }
}

/// Stops the recording once the recorder has written to `end`, and checks that it
/// stopped there: the driver answers a stop once the catalog's stop is written and
/// synced, so a snapshot whose ack is sent is one a power loss leaves whole.
fn stop_at(client: &mut Cavalry, recording: &Recording, end: i64) -> Result<(), SnapshotCause> {
    let mut since_ns = nano_time();
    let mut reached = recording.position();
    while reached < end {
        if recording.stop_position().is_some() {
            return Err(SnapshotCause::Short);
        }
        let now = nano_time();
        let position = recording.position();
        if position > reached {
            reached = position;
            since_ns = now;
        } else if now.saturating_sub(since_ns) >= SNAPSHOT_TIMEOUT_NS {
            return Err(SnapshotCause::Write);
        }
        std::thread::sleep(SNAPSHOT_PAUSE);
    }
    recording.stop(client).map_err(|_| SnapshotCause::Stop)?;
    let since_ns = nano_time();
    loop {
        if let Some(stop) = recording.stop_position() {
            return if stop == end {
                Ok(())
            } else {
                Err(SnapshotCause::Short)
            };
        }
        if nano_time().saturating_sub(since_ns) >= SNAPSHOT_TIMEOUT_NS {
            return Err(SnapshotCause::Stop);
        }
        std::thread::sleep(SNAPSHOT_PAUSE);
    }
}

/// A service's snapshot as its recording holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Snapshot {
    log_position: i64,
    leadership_term_id: i64,
    /// The sessions that were open at that position: each one's id and its response
    /// channel, in the rising order of the ids.
    sessions: Vec<(i64, Vec<u8>)>,
    /// The timers that were pending, each an id and a deadline, in the rising order
    /// of the ids.
    timers: Vec<(i64, i64)>,
    /// The program's bytes.
    state: Vec<u8>,
}

/// Where the reading of a service's snapshot is, message by message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reading {
    /// Nothing is read: the begin marker comes first.
    Begin,
    /// The begin marker is read: client sessions, then timers, then the section
    /// marker.
    Sessions,
    /// A timer is read: timers, then the section marker.
    Timers,
    /// The section marker is read: the program's bytes, then the end marker, which
    /// is the last message.
    Bytes,
    /// A message was not what its place asks for: the recording is no snapshot.
    Failed,
}

/// Reads a service's snapshot from the messages of its recording, in their order
/// (`spec/cluster.md`, "A service's start"). After the section marker a message is
/// the program's bytes unless it is the last, which is the end marker: each is kept
/// until the next one arrives, and the one kept at the end must be that marker.
struct SnapshotReader {
    reading: Reading,
    snapshot: Snapshot,
    /// The message read last after the section marker.
    last: Vec<u8>,
    /// Whether `last` holds a message.
    held: bool,
    /// The longest state the reader takes: the service snapshot state of the page's
    /// "Values", the bound a service writes with.
    state_limit: usize,
}

impl SnapshotReader {
    fn new() -> Self {
        Self::with_state_limit(MAX_SERVICE_SNAPSHOT_STATE_LENGTH)
    }

    /// A reader that takes a state of `state_limit` bytes at the most: [`new`]'s
    /// with the page's bound, and a test's with one it can reach.
    ///
    /// [`new`]: SnapshotReader::new
    fn with_state_limit(state_limit: usize) -> Self {
        Self {
            reading: Reading::Begin,
            snapshot: Snapshot {
                log_position: 0,
                leadership_term_id: 0,
                sessions: Vec::with_capacity(MAX_SESSIONS),
                timers: Vec::with_capacity(MAX_SERVICE_TIMERS),
                state: Vec::new(),
            },
            last: Vec::with_capacity(SERVICE_SNAPSHOT_STATE_MESSAGE_LENGTH),
            held: false,
            state_limit,
        }
    }

    /// Whether `bytes` are a marker of this snapshot with `mark` and `index`.
    fn is_marker(&self, bytes: &[u8], expected: SnapshotMark, expected_index: i32) -> bool {
        matches!(
            Message::parse(Stream::Snapshot, bytes),
            Ok(Message::SnapshotMarker {
                type_id: SERVICE_SNAPSHOT_TYPE_ID,
                log_position,
                leadership_term_id,
                index,
                mark,
                ..
            }) if mark == expected
                && index == expected_index
                && log_position == self.snapshot.log_position
                && leadership_term_id == self.snapshot.leadership_term_id
        )
    }

    /// Takes the next message of the recording. `whole` says that it arrived as one
    /// frame, as every message of a snapshot is written.
    fn message(&mut self, bytes: &[u8], whole: bool) {
        if !whole {
            self.reading = Reading::Failed;
        }
        self.reading = match self.reading {
            Reading::Begin => match Message::parse(Stream::Snapshot, bytes) {
                Ok(Message::SnapshotMarker {
                    type_id: SERVICE_SNAPSHOT_TYPE_ID,
                    log_position,
                    leadership_term_id,
                    index: 0,
                    mark: SnapshotMark::Begin,
                    ..
                }) if log_position > 0 => {
                    self.snapshot.log_position = log_position;
                    self.snapshot.leadership_term_id = leadership_term_id;
                    Reading::Sessions
                }
                _ => Reading::Failed,
            },
            Reading::Sessions => match Message::parse(Stream::Snapshot, bytes) {
                Ok(Message::ClientSession {
                    cluster_session_id,
                    response_channel,
                    ..
                }) => {
                    let above = self
                        .snapshot
                        .sessions
                        .last()
                        .is_none_or(|(before, _)| *before < cluster_session_id);
                    if above && self.snapshot.sessions.len() < MAX_SESSIONS {
                        self.snapshot
                            .sessions
                            .push((cluster_session_id, response_channel.to_vec()));
                        Reading::Sessions
                    } else {
                        Reading::Failed
                    }
                }
                Ok(Message::Timer {
                    correlation_id,
                    deadline,
                }) => self.timer(correlation_id, deadline),
                Ok(Message::SnapshotMarker { .. })
                    if self.is_marker(
                        bytes,
                        SnapshotMark::Section,
                        SERVICE_SNAPSHOT_STATE_INDEX,
                    ) =>
                {
                    Reading::Bytes
                }
                _ => Reading::Failed,
            },
            // The timers follow the sessions: no session after the first timer.
            Reading::Timers => match Message::parse(Stream::Snapshot, bytes) {
                Ok(Message::Timer {
                    correlation_id,
                    deadline,
                }) => self.timer(correlation_id, deadline),
                Ok(Message::SnapshotMarker { .. })
                    if self.is_marker(
                        bytes,
                        SnapshotMark::Section,
                        SERVICE_SNAPSHOT_STATE_INDEX,
                    ) =>
                {
                    Reading::Bytes
                }
                _ => Reading::Failed,
            },
            Reading::Bytes => {
                // The message kept before this one was not the last: the program's.
                if self.held {
                    self.snapshot.state.extend_from_slice(&self.last);
                }
                self.last.clear();
                self.last.extend_from_slice(bytes);
                self.held = true;
                if self.snapshot.state.len() > self.state_limit {
                    Reading::Failed
                } else {
                    Reading::Bytes
                }
            }
            Reading::Failed => Reading::Failed,
        };
    }

    /// One timer of the snapshot: above the one before it, and no more than a
    /// service holds (`spec/cluster.md`, "A service's start").
    fn timer(&mut self, correlation_id: i64, deadline: i64) -> Reading {
        let above = self
            .snapshot
            .timers
            .last()
            .is_none_or(|(before, _)| *before < correlation_id);
        if above && self.snapshot.timers.len() < MAX_SERVICE_TIMERS {
            self.snapshot.timers.push((correlation_id, deadline));
            Reading::Timers
        } else {
            Reading::Failed
        }
    }

    /// The snapshot, when the recording held one whole: the last message is its end
    /// marker.
    fn finish(self) -> Option<Snapshot> {
        let whole = self.reading == Reading::Bytes
            && self.held
            && self.is_marker(&self.last, SnapshotMark::End, 0);
        whole.then_some(self.snapshot)
    }

    /// The snapshot of a replay that has ended, for `reason`: the one the reader
    /// holds when the driver played the recording to its end, and none when the
    /// replay ended another way, at a frame that failed, at another client's stop
    /// or with no subscriber (`spec/cluster.md`, "A service's start": the driver
    /// plays the recording to its end). A replay that ended short can end at bytes
    /// of the program's that read as the end marker, and what it delivered before
    /// them is no whole state.
    fn finish_ended(self, reason: Option<ReplayStopReason>) -> Option<Snapshot> {
        if reason == Some(ReplayStopReason::Ended) {
            self.finish()
        } else {
            None
        }
    }
}

/// Plays a service's snapshot back whole, through its member's driver, and reads it:
/// `None` when the driver refuses the replay for its recording, plays nothing for the
/// service snapshot load timeout, ends the replay before the recording's end, at a
/// frame that failed, at another client's stop or with no subscriber, or the
/// recording holds no service's snapshot. Any other refusal of the driver's is the
/// error.
fn read_snapshot(client: &mut Cavalry, recording_id: i64) -> Result<Option<Snapshot>, ClientError> {
    // The subscription first: a replay with no subscriber does not move. And on a
    // stream of its own: on the service log stream a service that is not polling
    // holds every replay there at its own position, and a snapshot longer than the
    // publication window would wait for it.
    let mut subscription =
        client.add_subscription(IPC_CHANNEL, SERVICE_SNAPSHOT_REPLAY_STREAM_ID)?;
    let replay = match client.start_replay(
        recording_id,
        NULL_POSITION,
        NULL_LENGTH,
        SERVICE_SNAPSHOT_REPLAY_STREAM_ID,
        IPC_CHANNEL,
    ) {
        Ok(replay) => replay,
        // A refusal for the recording itself, which a later call meets again: the
        // driver does not hold it as a valid recording, code 21, or holds no bytes of
        // it where the replay starts, code 23.
        Err(ClientError::Refused {
            code: Some(ErrorCode::UnknownRecording | ErrorCode::PositionOutOfRange),
            ..
        }) => return Ok(None),
        // Every other refusal can pass, and is the caller's to retry: a registry, the
        // counters file or a queue at its bound, code 10; a retention command at work
        // on the recording, code 24, after which the recording plays or is unknown;
        // and code 12, one code for a segment file that cannot be read and for a
        // replay's log that cannot be created under shm pressure. A start that passed
        // its snapshot over for back-pressure could not take it again (invariant 3).
        Err(error) => return Err(error),
    };
    let session = replay.session_id();
    let mut reader = SnapshotReader::new();
    let mut since_ns = nano_time();
    // How far the replay's image has been read, kept from each poll that still had
    // the image: the driver withdraws it once it is read to the end of stream,
    // which can be before `replay stopped` reaches this client.
    let mut reached = 0i64;
    loop {
        let mut read = 0usize;
        let polled = subscription.poll(
            |bytes, header| {
                // Another service's replay on the stream is read past.
                if header.session_id() == session {
                    reader.message(bytes, header.flags().contains(Flags::UNFRAGMENTED));
                    read += 1;
                }
            },
            SNAPSHOT_LOAD_LIMIT,
        );
        if polled.is_err() || reader.reading == Reading::Failed {
            let _ = replay.stop(client);
            return Ok(None);
        }
        if let Some(image) = subscription
            .images()
            .iter()
            .find(|image| image.session_id() == session)
        {
            reached = reached.max(image.position());
        }
        // The replay has ended, and its image was read to where it ended. Only a
        // replay that played the recording to its end gives a snapshot.
        if let Some((end, reason)) = replay.ended()
            && reached >= end
        {
            return Ok(reader.finish_ended(reason));
        }
        let now = nano_time();
        if read > 0 {
            since_ns = now;
        } else if now.saturating_sub(since_ns) >= SNAPSHOT_LOAD_TIMEOUT_NS {
            let _ = replay.stop(client);
            return Ok(None);
        } else {
            std::thread::sleep(SNAPSHOT_PAUSE);
        }
    }
}

/// What a service's handler answers a session with: handed to the handler beside each
/// entry.
pub struct Replies<'a> {
    sessions: &'a mut [Session; MAX_SESSIONS],
    timers: &'a mut ServiceTimers,
    dropped: &'a Counter,
    /// Whether this service answers the entry being handled.
    answering: bool,
    leadership_term_id: i64,
    timestamp: i64,
}

impl Replies<'_> {
    /// Schedules the timer `correlation_id`, any id the program chooses, for
    /// `deadline` in the cluster's time, the clock of [`Committed::timestamp`]
    /// (`spec/cluster.md`, "The service", "Timers"; ADR 0151). When that time has
    /// come, the leader appends a timer event, and the handler is handed
    /// [`Entry::Timer`] with this id, once, at one position beside every member. A
    /// deadline at or before the entry's time is due at once. Scheduling an id that
    /// is pending sets its deadline and makes no second timer.
    ///
    /// A service holds at most 1,024 pending timers: a schedule of another id
    /// beyond them is refused with [`TimersFull`], beside every member alike, and
    /// changes nothing. A program with more deadlines than that keeps them in its
    /// own state and schedules one timer for the earliest.
    ///
    /// A program schedules its first timer only once every member of its cluster,
    /// with the service beside it, runs version 2 of the cluster protocol or a later
    /// one, and from that entry on no member is stepped back to version 1: a service of version 1
    /// hands its program no timer, so the two would compute two states from one
    /// log. Nothing in a member checks it (`spec/cluster.md`, "Versions and the upgrade").
    ///
    /// The call sends nothing and waits for nothing: the service tells its member
    /// after the handler has taken the entry. A handler that answers
    /// [`Action::Abort`] is handed the entry again with every timer as it was
    /// before, so it makes the same calls and is given the same answers.
    ///
    /// # Errors
    ///
    /// [`TimersFull`] when 1,024 timers are pending and `correlation_id` is not
    /// one of them.
    pub fn schedule_timer(&mut self, correlation_id: i64, deadline: i64) -> Result<(), TimersFull> {
        self.timers.schedule(correlation_id, deadline)
    }

    /// Cancels the timer `correlation_id`: it is pending no more, and no timer
    /// event is handed over for it, whatever events the log holds already. It
    /// answers nothing, whether the timer was pending or not.
    pub fn cancel_timer(&mut self, correlation_id: i64) {
        self.timers.cancel(correlation_id);
    }

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
    /// The snapshot the last poll's last entry asked for, until the next poll.
    due: Option<Due>,
    /// The two counters of the service's snapshots, added with the three of its
    /// timers at the first of its first snapshot that writes one, the first thing
    /// it has to tell its member of a timer and the first timer event it passes
    /// over ([`ClusterService::add_counters`]).
    snapshot_counters: Option<SnapshotCounters>,
    /// The service's publication on the service snapshot stream, from its first
    /// snapshot whose add the driver took: each snapshot is a recording of a span
    /// of it. Held apart from the counters, so that a refused add is counted, and a
    /// call after it adds no counter twice.
    snapshot_stream: Option<Publication>,
    /// The service's publication on the consensus module stream, from its first
    /// open of its member's log (ADR 0168): its service acks, schedule timers and
    /// cancel timers go to the member on it.
    member_stream: Option<Publication>,
    /// Whether the service has begun: a poll has opened its member's log, or
    /// [`ClusterService::restore`] has answered where it starts.
    started: bool,
    /// The service's two counters of its start, added when its member named a
    /// snapshot for it: where it started, and the snapshot it passed over.
    start: Option<(Counter, Counter)>,
    /// The program's pending timers, and what the member is still to be told.
    timers: Box<ServiceTimers>,
    /// What the service has counted of its timers, and its counters of them once it
    /// has added them.
    timer_counts: TimerCounts,
    timer_counters: Option<TimerCounters>,
    /// Whether the service has read the history it found when it opened its
    /// member's log, and so tells its member of its timers.
    history_read: bool,
    /// When the service last sent its member every pending timer, on its own
    /// monotonic clock.
    refreshed_at_ns: i64,
    /// When the driver last refused one of the six adds for the member, by the
    /// monotonic clock: the service asks again at its first poll a timer refresh
    /// interval after it, and at none between (ADR 0168). `None` while none was
    /// refused.
    add_refused_at_ns: Option<i64>,
}

/// What a service has counted of its timers: the events it passed over and the
/// cancels it dropped, with what its counters last held of them and of how many
/// timers are pending.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct TimerCounts {
    passed_over: i64,
    cancels_dropped: i64,
    stored: (i64, i64, i64),
}

/// The three counters of a service's timers, which it adds with its member's id as
/// the key (`spec/cluster.md`, "Counters").
struct TimerCounters {
    pending: Counter,
    passed_over: Counter,
    cancels_dropped: Counter,
}

/// The snapshot a cluster action asked for: what its markers and its ack carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Due {
    /// The position after the action's entry.
    log_position: i64,
    /// The leadership term of the action's entry.
    leadership_term_id: i64,
    /// The cluster's time of the action's entry.
    timestamp: i64,
}

/// The two counters of a service's snapshots, kept from the first snapshot it writes
/// to its close: `cluster.service_snapshots_taken` and
/// `cluster.service_snapshots_failed`.
struct SnapshotCounters {
    taken: Counter,
    failed: Counter,
}

impl SnapshotCounters {
    /// Adds the two, with the member's id as the key and in the label.
    fn add(client: &mut Cavalry, member: &Member) -> Result<Self, ClientError> {
        let (key, id) = (member.key, member.id);
        let taken = client.add_counter(
            CLUSTER_SERVICE_SNAPSHOTS_TAKEN.0,
            &key,
            &format!("cluster.service_snapshots_taken {id}"),
        )?;
        let failed = client.add_counter(
            CLUSTER_SERVICE_SNAPSHOTS_FAILED.0,
            &key,
            &format!("cluster.service_snapshots_failed {id}"),
        )?;
        Ok(Self { taken, failed })
    }
}

/// The two publications a snapshot is written through, the service's own, taken for
/// the writing and given back after it: the publication each snapshot is a recording
/// of a span of, and the publication its service acks go to the member on.
struct SnapshotPublications {
    publication: Publication,
    acks: Publication,
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
            due: None,
            snapshot_counters: None,
            snapshot_stream: None,
            member_stream: None,
            started: false,
            start: None,
            timers: {
                // A service reads history first, and tells its member nothing of it.
                let mut timers = Box::new(ServiceTimers::new());
                timers.hold();
                timers
            },
            timer_counts: TimerCounts::default(),
            timer_counters: None,
            history_read: false,
            refreshed_at_ns: 0,
            add_refused_at_ns: None,
        }
    }

    /// Starts the service from the newest snapshot of a service that its member
    /// lists (`spec/cluster.md`, "A service's start"; ADR 0132). It is called before
    /// the first poll, by a program that gives its state to
    /// [`snapshot`](Self::snapshot): a program that never calls it reads its
    /// member's log from position 0, and needs the whole of it. The member's
    /// `cluster.log_purge_position`, from which an operator purges, assumes a
    /// program that calls it: beside a service that takes snapshots and never calls
    /// it, nothing may be purged. In Aeron, this is the snapshot image of
    /// `ClusteredService.onStart`.
    ///
    /// [`Restored::State`] is the program's state as the program gave it. The
    /// service then holds the sessions that were open at the snapshot's position,
    /// [`position`](Self::position) is that position, and the next poll hands over
    /// the entry after it. For the entries of the term the snapshot was taken in,
    /// the service answers when its member's counters say that the member leads
    /// that term.
    ///
    /// [`Restored::FromStart`] says that the polls read from position 0: the member
    /// lists no snapshot of a service, or the one it lists could not be taken, its
    /// recording gone, not played back within 5 s of its last message, or its
    /// replay ended before the recording's end, at a frame that failed, at another
    /// client's stop or with no subscriber. That one is counted in
    /// `cluster.service_snapshots_passed_over`. A member whose log no
    /// longer begins at 0 then refuses the service its replay, which the poll
    /// reports. [`Restored::NoMember`] says that the driver hosts no member with a
    /// log yet, and the call is made again.
    ///
    /// The call waits for the driver to play the snapshot's recording back. It is
    /// refused with [`ClusterError::RestoreTooLate`] once a poll has opened the
    /// member's log, or once it has answered [`Restored::State`] or
    /// [`Restored::FromStart`].
    ///
    /// A replay that the driver refuses for its recording, which it does not hold as
    /// a valid recording, error code 21, or holds no bytes of where the replay starts,
    /// code 23, passes the snapshot over, counted. Every other refusal of the call's
    /// commands is [`ClusterError::Client`], with the driver's code: the add of the two
    /// counters of the service's start or of its subscription, and a replay refused for
    /// a registry, the counters file or a queue at its bound, code 10, a retention
    /// command at work on the recording, code 24, or a file that cannot be read or
    /// created, code 12. The service has not started, has counted nothing and holds
    /// neither counter, and the call is made again: a refusal that passes is never a
    /// snapshot passed over. The driver counts a registry at its bound in
    /// `driver.registry_full`, and the error's message says what the operator does
    /// about the others.
    ///
    /// ```
    /// use cavalry_client::{Cavalry, Restored};
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let dir = std::env::temp_dir().join(format!("cavalry-doc-restore-{}", std::process::id()));
    /// # let _ = std::fs::remove_dir_all(&dir);
    /// # let driver = cavalry_driver::Driver::start(&dir, dir.join("log"))?;
    /// let mut client = Cavalry::connect_to(&dir)?;
    /// let mut service = client.cluster_service();
    /// // The program's state: a count it keeps as eight bytes.
    /// let mut total = 0u64;
    /// match service.restore(&mut client)? {
    ///     // The count as the service's last snapshot held it.
    ///     Restored::State(state) => total = u64::from_le_bytes(state.as_slice().try_into()?),
    ///     // No snapshot: the polls read the log from its start.
    ///     Restored::FromStart => {}
    ///     // A driver that hosts no cluster member: ask again before the first poll.
    ///     Restored::NoMember => {}
    /// }
    /// assert_eq!((total, service.position()), (0, 0));
    /// # service.close(&mut client)?;
    /// # client.close()?;
    /// # driver.close()?;
    /// # let _ = std::fs::remove_dir_all(&dir);
    /// # Ok(())
    /// # }
    /// ```
    pub fn restore(&mut self, client: &mut Cavalry) -> Result<Restored, ClusterError> {
        if self.started {
            return Err(ClusterError::RestoreTooLate);
        }
        let counters = client.shared_counters();
        let Some(key) = member_with_a_log(&counters) else {
            return Ok(Restored::NoMember);
        };
        // The member names the recording of its newest snapshot of a service. It
        // stored that before it named a recording of its log, so the value is no
        // counter's first 0.
        let named = load(
            &counters,
            &mut None,
            CLUSTER_SERVICE_SNAPSHOT_RECORDING_ID,
            key,
        );
        let Some(recording_id) = named.filter(|recording_id| *recording_id >= 0) else {
            self.started = true;
            return Ok(Restored::FromStart);
        };
        let id = i32::from_le_bytes(key);
        let started_at = client.add_counter(
            CLUSTER_SERVICE_SNAPSHOT_START_POSITION.0,
            &key,
            &format!("cluster.service_snapshot_start_position {id}"),
        )?;
        let passed_over = client.add_counter(
            CLUSTER_SERVICE_SNAPSHOTS_PASSED_OVER.0,
            &key,
            &format!("cluster.service_snapshots_passed_over {id}"),
        )?;
        let taken = read_snapshot(client, recording_id)?.filter(|snapshot| self.take(snapshot));
        let restored = match taken {
            Some(snapshot) => {
                let _ = started_at.store_release(snapshot.log_position);
                Restored::State(snapshot.state)
            }
            None => {
                let _ = started_at.store_release(-1);
                let _ = passed_over.increment();
                Restored::FromStart
            }
        };
        // From here the service has begun: a call that failed above is made again.
        self.started = true;
        self.start = Some((started_at, passed_over));
        Ok(restored)
    }

    /// Takes a snapshot's sessions, position and term, when the table holds every
    /// session: `false`, and nothing taken, when it does not.
    fn take(&mut self, snapshot: &Snapshot) -> bool {
        let fits = snapshot.sessions.len() <= MAX_SESSIONS
            && snapshot
                .sessions
                .iter()
                .all(|(_, channel)| channel.len() <= MAX_RESPONSE_CHANNEL_LENGTH);
        if !fits {
            return false;
        }
        // The timers first: a snapshot whose timers the table does not take is
        // passed over whole, with no timer and no session of it kept.
        self.timers.clear();
        for &(correlation_id, deadline) in &snapshot.timers {
            if self.timers.load(correlation_id, deadline).is_err() {
                self.timers.clear();
                return false;
            }
        }
        for (session, (id, channel)) in self.sessions.iter_mut().zip(&snapshot.sessions) {
            *session = Session::closed();
            if let Some(held) = session.channel.get_mut(..channel.len()) {
                held.copy_from_slice(channel);
                session.channel_length = channel.len();
                session.id = *id;
                session.open = true;
            }
        }
        self.position = snapshot.log_position;
        self.term_leader = (snapshot.leadership_term_id, ASK_MEMBER);
        true
    }

    /// Reads up to `fragment_limit` committed entries, each handed to `handler` with
    /// the [`Replies`] it may answer a session through. The handler's [`Action`]
    /// says whether the entry is taken: [`Action::Abort`] leaves it for the next
    /// poll, which is what a [`Reply::Retry`] asks for. Returns how many entries
    /// were taken; 0 while the driver, one that runs, hosts no member that has a
    /// log yet.
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
    /// A poll ends after an [`Entry::Snapshot`] that the handler took, whatever
    /// `fragment_limit` is, so that [`snapshot`](Self::snapshot) can take the
    /// program's state as it is at that position. The next poll reads on, with or
    /// without a snapshot taken.
    ///
    /// The poll is the service's duty cycle. It opens the member's log when the
    /// member has one, and again at the next recording when one ends. From the first
    /// entry it answers it holds an egress publication to each open session's
    /// response channel, and from the first it does not, none. Those are commands to
    /// the driver: at a session's opening, where a term's leader changes, and at a
    /// recording's end. Nothing is allocated, and no command is sent, for a session
    /// message. An entry the service would answer costs two loads of the member's
    /// counters.
    ///
    /// The poll that first opens the member's log adds the service's five counters
    /// and its publication on the consensus module stream, before it reads an
    /// entry: six commands to the driver, once in a service's life, the last of
    /// which has the driver make a log of 192 MiB resident, tens of milliseconds
    /// and more. So no later poll sends a command for a timer, and a program's
    /// first timer costs what a later one costs (ADR 0168). A refusal of one of the
    /// six is this poll's error, returned after the entries the handler took are
    /// taken, and each poll after it adds what is missing before it reads.
    ///
    /// After its entries the poll tells the member of the program's timers
    /// (`spec/cluster.md`, "What a service tells its member"). It offers the stream
    /// at most 2,048 messages, and none for a program that schedules no timer.
    ///
    /// A poll of a closed client is one error, [`ClusterError::Client`] holding
    /// [`ClientError::Closed`] with the reason [`Cavalry::closed`] gives, whatever
    /// the service held and whatever the poll was doing when it met the close: the
    /// read of its log, a command to the driver, or nothing, for a service that had
    /// no log open (ADR 0155). So a service ends when its member's driver stops, and
    /// a program's loop needs no other look at its client: it ends on that error, as
    /// `crates/cavalry/examples/adder-service.rs` does. The entries the handler took
    /// before the close are taken: [`position`](Self::position) is after the last.
    /// An error of another kind that a poll meets in the call in which its client
    /// closes is reported as the close.
    pub fn poll<F>(
        &mut self,
        client: &mut Cavalry,
        handler: F,
        fragment_limit: usize,
    ) -> Result<usize, ClusterError>
    where
        F: FnMut(&Committed<'_>, &mut Replies<'_>) -> Action,
    {
        // A program that reads on has let the snapshot go, whatever the poll finds.
        self.due = None;
        let closed = |reason| ClusterError::Client(ClientError::Closed(reason));
        if let Some(reason) = client.closed() {
            return Err(closed(reason));
        }
        match self.poll_entries(client, handler, fragment_limit) {
            // What a poll met at a client that is closed now is the close: a fault of
            // its log's read and a command's refusal say the same thing there.
            Err(error) => Err(client.closed().map_or(error, closed)),
            taken => taken,
        }
    }

    /// The poll, for a client that was open when it began.
    fn poll_entries<F>(
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
            // The service reads from where it is: a start from a snapshot is asked
            // for before this.
            self.started = true;
        }
        // The service's counters and its publication to its member, before the
        // entries of the poll that first opened the log; where the driver refused
        // one, again before the entries of the first poll a timer refresh interval
        // after the refusal (`spec/cluster.md`, "What a service tells its member";
        // ADR 0168). A refusal is this poll's error once its entries are taken.
        let added = self.add_for_member(client);
        let Self {
            log,
            member,
            term_leader,
            answering,
            sessions,
            timers,
            timer_counts,
            history_read,
            refreshed_at_ns,
            ..
        } = self;
        let (Some(log), Some(member)) = (log.as_mut(), member.as_mut()) else {
            return Ok(0);
        };
        // A service that opens its member's log at or past where that log ended then
        // has no history to read: it tells its member of its timers from here, the
        // ones of the snapshot it started from among them (`spec/cluster.md`, "The
        // service", "Timers").
        if !*history_read && log.position() >= member.append_position {
            *history_read = true;
            timers.release();
            *refreshed_at_ns = nano_time();
        }
        let mut failure: Option<ClusterError> = None;
        // The term and the time of a snapshot action the handler took.
        let mut asked: Option<(i64, i64)> = None;
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
                    Some(Message::ClusterAction {
                        leadership_term_id,
                        timestamp,
                        action: ClusterAction::Snapshot,
                        ..
                    }) => (Entry::Snapshot, leadership_term_id, timestamp),
                    Some(Message::TimerEvent {
                        leadership_term_id,
                        correlation_id,
                        timestamp,
                    }) => (
                        Entry::Timer { correlation_id },
                        leadership_term_id,
                        timestamp,
                    ),
                    Some(
                        Message::ClusterAction {
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
                // A service that started from a snapshot in this term did not read
                // the term's event, and takes the member's word alone.
                let named = term_leader.0 == leadership_term_id
                    && (term_leader.1 == member.id || term_leader.1 == ASK_MEMBER);
                let answers = !history && named && member.leads(leadership_term_id);
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
                // The first entry that is no history ends the history: the service
                // tells its member of its timers from here.
                if !history && !*history_read {
                    *history_read = true;
                    timers.release();
                    *refreshed_at_ns = nano_time();
                }
                // A timer event is handed over only when its timer is pending with a
                // deadline at or before the event's time. Every other one is passed
                // over: taken, counted, and no handler's.
                let entry = match entry {
                    Entry::Timer { correlation_id }
                        if timers.begin_event(correlation_id, timestamp)
                            != TimerEventIs::Handed =>
                    {
                        timers.end_entry(true);
                        timer_counts.passed_over = timer_counts.passed_over.saturating_add(1);
                        return Action::Continue;
                    }
                    Entry::Timer { .. } => entry,
                    _ => {
                        timers.begin_entry();
                        entry
                    }
                };
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
                        timers,
                        dropped: &member.dropped,
                        answering: answers,
                        leadership_term_id,
                        timestamp,
                    },
                );
                // What the handler scheduled and cancelled stands when it took the
                // entry, and is undone when it did not.
                let dropped = timers.end_entry(action != Action::Abort);
                timer_counts.cancels_dropped = timer_counts
                    .cancels_dropped
                    .saturating_add(i64::try_from(dropped).unwrap_or(i64::MAX));
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
                // The read ends at a snapshot action that was taken: the program's
                // state is then its state at the position after this entry.
                if action != Action::Abort && entry == Entry::Snapshot {
                    asked = Some((leadership_term_id, timestamp));
                    return Action::Break;
                }
                action
            },
            fragment_limit,
        );
        self.position = log.position();
        self.due = asked.map(|(leadership_term_id, timestamp)| Due {
            log_position: self.position,
            leadership_term_id,
            timestamp,
        });
        // A handle that has ended, at its recording's stop, is closed: the next poll
        // opens the log again where this one stopped.
        if (polled.is_err() || log.is_ended())
            && let Some(log) = self.log.take()
        {
            log.close(client)?;
        }
        // What the entries taken left to tell the member, and what is due again.
        self.tell_member();
        if let Some(error) = failure {
            return Err(error);
        }
        let polled = polled.map_err(ClusterError::Log)?;
        added?;
        Ok(polled)
    }

    /// Adds what a service adds for its member where it holds it not yet: its five
    /// counters, and then its publication on the consensus module stream. A service
    /// that has found no member adds nothing. The publication is a `cavalry:ipc`
    /// log of three terms of 64 MiB, every page of which the driver touches as it
    /// creates it and this client touches again as it maps it (ADR 0027): that is
    /// what a program's first timer waited for when this was done at the first
    /// thing to tell (work item 462; ADR 0168).
    ///
    /// A refusal is returned, and nothing is asked of the driver again until a
    /// timer refresh interval, 1 s, has passed: a refused command costs the driver
    /// an error event to every one of its clients and a counter, and a refused
    /// publication the making of its log, so a service that polls a thousand times
    /// a second asks once a second and not a thousand times. A poll between two
    /// asks reports nothing of it. With all six held this is three compares, and
    /// reads no clock.
    fn add_for_member(&mut self, client: &mut Cavalry) -> Result<(), ClusterError> {
        let Some(member) = self.member.as_ref() else {
            return Ok(());
        };
        if self.member_stream.is_some()
            && self.snapshot_counters.is_some()
            && self.timer_counters.is_some()
        {
            return Ok(());
        }
        if let Some(refused_at) = self.add_refused_at_ns
            && nano_time().saturating_sub(refused_at) < TIMER_REFRESH_INTERVAL_NS as i64
        {
            return Ok(());
        }
        // The counters before the publication (`spec/cluster.md`, "A service's
        // snapshot").
        let added = Self::add_counters(
            client,
            member,
            &mut self.snapshot_counters,
            &mut self.timer_counters,
        )
        .and_then(|()| {
            if self.member_stream.is_none() {
                self.member_stream = Some(
                    client.add_exclusive_publication(IPC_CHANNEL, CONSENSUS_MODULE_STREAM_ID)?,
                );
            }
            Ok(())
        });
        self.add_refused_at_ns = added.is_err().then(nano_time);
        Ok(added?)
    }

    /// Adds the counters a service adds, the two of its snapshots and the three of
    /// its timers, each with its member's id as the key, where it holds them not
    /// yet: before its publications, so that a publication the driver refuses at a
    /// snapshot is counted (`spec/cluster.md`, "A service's snapshot"). A refusal
    /// of a counter is the caller's error, and the next call adds what is missing.
    fn add_counters(
        client: &mut Cavalry,
        member: &Member,
        snapshots: &mut Option<SnapshotCounters>,
        timers: &mut Option<TimerCounters>,
    ) -> Result<(), ClientError> {
        if snapshots.is_none() {
            *snapshots = Some(SnapshotCounters::add(client, member)?);
        }
        if timers.is_none() {
            let (key, id) = (member.key, member.id);
            let mut add = |counter: CounterId, name: &str| {
                client.add_counter(counter.0, &key, &format!("{name} {id}"))
            };
            *timers = Some(TimerCounters {
                pending: add(CLUSTER_SERVICE_TIMERS, "cluster.service_timers")?,
                passed_over: add(
                    CLUSTER_SERVICE_TIMER_EVENTS_PASSED_OVER,
                    "cluster.service_timer_events_passed_over",
                )?,
                cancels_dropped: add(
                    CLUSTER_SERVICE_TIMER_CANCELS_DROPPED,
                    "cluster.service_timer_cancels_dropped",
                )?,
            });
        }
        Ok(())
    }

    /// Stores what the service has counted of its timers into its counters of them,
    /// where it has added them and they hold something else: a few compares when
    /// nothing changed. A store that fails is made again at the next call.
    fn store_counts(&mut self, counts: (i64, i64, i64)) {
        if counts != self.timer_counts.stored
            && let Some(counters) = self.timer_counters.as_ref()
            && counters.pending.store_release(counts.0).is_ok()
            && counters.passed_over.store_release(counts.1).is_ok()
            && counters.cancels_dropped.store_release(counts.2).is_ok()
        {
            self.timer_counts.stored = counts;
        }
    }

    /// Tells the member on this service's driver of its timers, on the consensus
    /// module stream, and stores the service's counters of them (`spec/cluster.md`,
    /// "The service", "Timers", "What a service tells its member"; ADR 0151): a
    /// cancel timer for each timer that an entry left pending no more, a schedule
    /// timer for each that an entry scheduled, and a schedule timer for every
    /// pending timer again each timer refresh interval, 1 s. Each message is
    /// offered once in a poll, for as long as the stream takes them: one that the
    /// stream does not take, and those after it, are offered in a later poll. So a
    /// poll offers 2,048 messages at the most, the 1,024 cancels a service holds
    /// and its 1,024 timers, which is what the poll after a refresh of a full table
    /// costs.
    ///
    /// It adds nothing: the service's counters and its publication on the consensus
    /// module stream are added before a poll's entries, from the first open of the
    /// member's log ([`add_for_member`](Self::add_for_member); ADR 0168). While the
    /// driver refuses the publication, nothing is told and every word is kept for
    /// the poll that has it; while it refuses a counter, the counts are kept in the
    /// service and stored when the counter is there. Neither fails a call of the
    /// program's inside a handler or changes a timer. A service that reads history
    /// tells its member nothing, and its gauge holds its pending timers meanwhile.
    ///
    /// A poll of a service with no timer pending, nothing to tell and nothing new
    /// to count costs a few compares here, and reads no clock.
    fn tell_member(&mut self) {
        if self.history_read && !self.timers.is_empty() {
            let now = nano_time();
            if now.saturating_sub(self.refreshed_at_ns) >= TIMER_REFRESH_INTERVAL_NS as i64 {
                self.timers.tell_all_again();
                self.refreshed_at_ns = now;
            }
        }
        let counts = (
            i64::try_from(self.timers.len()).unwrap_or(i64::MAX),
            self.timer_counts.passed_over,
            self.timer_counts.cancels_dropped,
        );
        self.store_counts(counts);
        let Some(stream) = self.member_stream.as_mut() else {
            return;
        };
        let mut scratch = [0u8; 64];
        // Each word is told once, so the cancels a service holds and its timers
        // bound the loop.
        for _ in 0..MAX_UNSENT_TIMER_CANCELS + MAX_SERVICE_TIMERS {
            let Some(word) = self.timers.next_word() else {
                break;
            };
            let Ok(length) = word.message().write(&mut scratch) else {
                break;
            };
            let Some(bytes) = scratch.get(..length) else {
                break;
            };
            // A stream that takes nothing now, as while no member reads it, keeps
            // the word for a later poll.
            if stream.offer(bytes).is_err() {
                break;
            }
            self.timers.told(word);
        }
    }

    /// The position after the last entry the handler took, 0 before the first.
    pub fn position(&self) -> i64 {
        self.position
    }

    /// The log position of the snapshot the leader has asked for, when the last
    /// entry of the last poll was an [`Entry::Snapshot`] that the handler took:
    /// [`position`](Self::position) then. `None` otherwise, and from the next poll on.
    pub fn snapshot_due(&self) -> Option<i64> {
        self.due.map(|due| due.log_position)
    }

    /// Takes a snapshot of the program's `state`, as it is at
    /// [`snapshot_due`](Self::snapshot_due): the bytes are the program's own, and
    /// the service reads nothing in them. It is called after a poll whose last entry
    /// was an [`Entry::Snapshot`], and before the next poll (`spec/cluster.md`,
    /// "Snapshots"; ADR 0131). In Aeron, this is `ClusteredService.onTakeSnapshot`.
    ///
    /// The service writes a recording through its member's driver: a marker, the
    /// sessions that are open at that position, a marker, `state`, 1,024 bytes to a
    /// message, and a marker. It stops the recording, checks that it stopped at the
    /// end of what was written, and tells the member with a service ack. The member
    /// lists the snapshot in its snapshots file. `Ok(true)` says that the ack is on
    /// its way: a member that stops before it reads it lists nothing, and the next
    /// snapshot is asked for by itself.
    ///
    /// `Ok(false)` says that nothing was written, because the member already lists a
    /// snapshot of a service at or past this position: a service that reads its
    /// member's log again from the start meets the actions it met before.
    ///
    /// The call waits for the driver. A step of the snapshot's own, a message its
    /// publication takes, its recording's write and its stop, that makes no progress
    /// for a second ends the snapshot with [`ClusterError::SnapshotFailed`], which
    /// names the step. An add or a command that the driver does not answer waits the
    /// response timeout, 10 s: an add, or the recording's start, then ends the call
    /// with the client's error, and the recording's stop ends it as a snapshot
    /// failed at its stop. The service is as it
    /// was, and the program polls on, or calls again while no poll has been made. A
    /// state longer than 1 GiB is refused, [`ClusterError::SnapshotTooLong`], and a
    /// call where no snapshot is due is [`ClusterError::NoSnapshotDue`].
    ///
    /// A snapshot written is counted in `cluster.service_snapshots_taken`, and one
    /// given up in `cluster.service_snapshots_failed`, the driver's refusal of a
    /// command among them. At the first snapshot that refusal can be of the
    /// service's two publications, which the call returns as
    /// [`ClusterError::Client`]. The service's two counters are added before
    /// anything else, at the first call that writes a snapshot, and a refusal of
    /// either is the one failure that is not counted: there is no counter yet to
    /// count it in. The call returns it as [`ClusterError::Client`], and the next
    /// call adds them again.
    ///
    /// ```
    /// use cavalry_client::{Action, Cavalry, ClusterError, Entry};
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let dir = std::env::temp_dir().join(format!("cavalry-doc-snapshot-{}", std::process::id()));
    /// # let _ = std::fs::remove_dir_all(&dir);
    /// # let driver = cavalry_driver::Driver::start(&dir, dir.join("log"))?;
    /// let mut client = Cavalry::connect_to(&dir)?;
    /// let mut service = client.cluster_service();
    /// // The program's state: how many bytes its sessions have sent.
    /// let mut total = 0u64;
    /// service.poll(
    ///     &mut client,
    ///     |committed, _replies| {
    ///         if let Entry::SessionMessage { payload, .. } = committed.entry {
    ///             total += payload.len() as u64;
    ///         }
    ///         Action::Continue
    ///     },
    ///     10,
    /// )?;
    /// // The poll ended at the leader's request for a snapshot: give the state.
    /// if service.snapshot_due().is_some() {
    ///     service.snapshot(&mut client, &total.to_le_bytes())?;
    /// }
    /// // A driver that hosts no cluster member has no log, and no snapshot is due.
    /// assert_eq!(service.snapshot_due(), None);
    /// assert!(matches!(
    ///     service.snapshot(&mut client, &total.to_le_bytes()),
    ///     Err(ClusterError::NoSnapshotDue)
    /// ));
    /// # service.close(&mut client)?;
    /// # client.close()?;
    /// # driver.close()?;
    /// # let _ = std::fs::remove_dir_all(&dir);
    /// # Ok(())
    /// # }
    /// ```
    pub fn snapshot(&mut self, client: &mut Cavalry, state: &[u8]) -> Result<bool, ClusterError> {
        let (Some(due), Some(member)) = (self.due, self.member.as_mut()) else {
            return Err(ClusterError::NoSnapshotDue);
        };
        if state.len() > MAX_SERVICE_SNAPSHOT_STATE_LENGTH {
            return Err(ClusterError::SnapshotTooLong {
                found: state.len(),
                limit: MAX_SERVICE_SNAPSHOT_STATE_LENGTH,
            });
        }
        // The member lists one at or past this position already: an action that this
        // service, or one before it, took a snapshot for.
        if member
            .service_snapshot_position()
            .is_some_and(|listed| listed >= due.log_position)
        {
            self.due = None;
            return Ok(false);
        }
        // The counters first, so that every step after them that fails is counted,
        // the driver's refusal of a publication among them.
        Self::add_counters(
            client,
            member,
            &mut self.snapshot_counters,
            &mut self.timer_counters,
        )?;
        let Some(counters) = self.snapshot_counters.as_ref() else {
            // Added above, or the call returned.
            return Err(ClusterError::NoSnapshotDue);
        };
        // The service's two publications, each added where the service holds it not
        // yet, and taken for the writing: a refusal of the second leaves the first
        // the service's.
        let publication = match self.snapshot_stream.take() {
            Some(publication) => publication,
            None => match client.add_exclusive_publication(IPC_CHANNEL, SERVICE_SNAPSHOT_STREAM_ID)
            {
                Ok(publication) => publication,
                Err(error) => {
                    let _ = counters.failed.increment();
                    return Err(error.into());
                }
            },
        };
        let acks = match self.member_stream.take() {
            Some(acks) => acks,
            None => match client.add_exclusive_publication(IPC_CHANNEL, CONSENSUS_MODULE_STREAM_ID)
            {
                Ok(acks) => acks,
                Err(error) => {
                    self.snapshot_stream = Some(publication);
                    let _ = counters.failed.increment();
                    return Err(error.into());
                }
            },
        };
        let mut publications = SnapshotPublications { publication, acks };
        let written = write_snapshot(
            client,
            &mut publications,
            &self.sessions,
            &self.timers,
            due,
            state,
        );
        self.snapshot_stream = Some(publications.publication);
        self.member_stream = Some(publications.acks);
        match written {
            Ok(()) => {
                let _ = counters.taken.increment();
                self.due = None;
                Ok(true)
            }
            Err(error) => {
                let _ = counters.failed.increment();
                Err(error)
            }
        }
    }

    /// Whether the service answered the last entry it read: its member led that
    /// entry's term and, as the entry was read, led it still, and the entry was not
    /// history.
    pub fn answers(&self) -> bool {
        self.answering
    }

    /// How many sessions the log holds open, as far as the service has read.
    pub fn sessions(&self) -> usize {
        self.sessions.iter().filter(|session| session.open).count()
    }

    /// Closes the service: its read of the log, its publications and its counter.
    /// A driver that is gone cannot be told, and that is not an error here, as it
    /// is none of a publication's close: at a client that is closed, or that the
    /// close itself finds closed, the service drops what it held and the close is
    /// `Ok` (ADR 0155).
    pub fn close(mut self, client: &mut Cavalry) -> Result<(), ClusterError> {
        if client.closed().is_some() {
            return Ok(());
        }
        match self.close_handles(client) {
            Err(_) if client.closed().is_some() => Ok(()),
            closed => closed,
        }
    }

    /// The close, for a client that was open when it began. What it has not closed
    /// when it returns an error, the drops of the service's handles close with no
    /// answer waited for.
    fn close_handles(&mut self, client: &mut Cavalry) -> Result<(), ClusterError> {
        for session in self.sessions.iter_mut() {
            if let Some(publication) = session.publication.take() {
                publication.close()?;
            }
        }
        if let Some(member) = self.member.take() {
            member.dropped.close();
        }
        if let Some(counters) = self.snapshot_counters.take() {
            counters.taken.close();
            counters.failed.close();
        }
        if let Some(counters) = self.timer_counters.take() {
            counters.pending.close();
            counters.passed_over.close();
            counters.cancels_dropped.close();
        }
        if let Some(publication) = self.snapshot_stream.take() {
            publication.close()?;
        }
        if let Some(stream) = self.member_stream.take() {
            stream.close()?;
        }
        if let Some((started_at, passed_over)) = self.start.take() {
            started_at.close();
            passed_over.close();
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

#[cfg(test)]
mod tests {
    use super::*;
    use cavalry_proto::cluster::TimerWord;

    const POSITION: i64 = 4_096;
    const TERM: i64 = 3;
    const CHANNEL: &[u8] = b"cavalry:udp?endpoint=127.0.0.1:9040";

    fn bytes(message: &Message<'_>) -> Vec<u8> {
        let mut scratch = [0u8; SNAPSHOT_SCRATCH_LENGTH];
        let length = message
            .write(&mut scratch)
            .expect("a message of a snapshot");
        scratch[..length].to_vec()
    }

    fn marker(type_id: i64, log_position: i64, mark: SnapshotMark, index: i32) -> Vec<u8> {
        bytes(&Message::SnapshotMarker {
            type_id,
            log_position,
            leadership_term_id: TERM,
            index,
            mark,
            time_unit: TimeUnit::Millis,
            app_version: 0,
        })
    }

    fn session(id: i64) -> Vec<u8> {
        bytes(&Message::ClientSession {
            cluster_session_id: id,
            response_stream_id: EGRESS_STREAM_ID,
            response_channel: CHANNEL,
            encoded_principal: &[],
        })
    }

    /// A service's snapshot as a service writes it: a marker, the sessions, the
    /// section marker, the state in messages of 1,024 bytes, and a marker.
    fn snapshot(ids: &[i64], state: &[u8]) -> Vec<Vec<u8>> {
        let mut messages = vec![marker(
            SERVICE_SNAPSHOT_TYPE_ID,
            POSITION,
            SnapshotMark::Begin,
            0,
        )];
        messages.extend(ids.iter().map(|id| session(*id)));
        messages.push(marker(
            SERVICE_SNAPSHOT_TYPE_ID,
            POSITION,
            SnapshotMark::Section,
            SERVICE_SNAPSHOT_STATE_INDEX,
        ));
        messages.extend(
            state
                .chunks(SERVICE_SNAPSHOT_STATE_MESSAGE_LENGTH)
                .map(<[u8]>::to_vec),
        );
        messages.push(marker(
            SERVICE_SNAPSHOT_TYPE_ID,
            POSITION,
            SnapshotMark::End,
            0,
        ));
        messages
    }

    fn read(messages: &[Vec<u8>]) -> Option<Snapshot> {
        let mut reader = SnapshotReader::new();
        for message in messages {
            reader.message(message, true);
        }
        reader.finish()
    }

    #[test]
    fn a_snapshot_as_a_service_writes_it_is_read_back() {
        let state: Vec<u8> = (0..2_500u32).map(|i| i as u8).collect();
        let taken = read(&snapshot(&[0, 2, 5], &state)).expect("the snapshot");
        assert_eq!(taken.log_position, POSITION);
        assert_eq!(taken.leadership_term_id, TERM);
        assert_eq!(
            taken.sessions,
            [
                (0, CHANNEL.to_vec()),
                (2, CHANNEL.to_vec()),
                (5, CHANNEL.to_vec())
            ]
        );
        assert_eq!(taken.state, state);
        // No session and no byte: three markers.
        let empty = read(&snapshot(&[], &[])).expect("the snapshot");
        assert!(empty.sessions.is_empty() && empty.state.is_empty());
        // A state whose bytes read as messages of the page is the program's all the
        // same, by its place: a piece that is a client session, and one that is the
        // end marker itself.
        let mut tricky = session(9);
        tricky.extend_from_slice(&marker(
            SERVICE_SNAPSHOT_TYPE_ID,
            POSITION,
            SnapshotMark::End,
            0,
        ));
        let taken = read(&snapshot(&[1], &tricky)).expect("the snapshot");
        assert_eq!(taken.sessions.len(), 1);
        assert_eq!(taken.state, tricky);
        let marker_only = marker(SERVICE_SNAPSHOT_TYPE_ID, POSITION, SnapshotMark::End, 0);
        let taken = read(&snapshot(&[], &marker_only)).expect("the snapshot");
        assert_eq!(taken.state, marker_only);
    }

    /// A replay that did not play the recording to its end gives no snapshot,
    /// whatever its last message reads as. The state here ends in the bytes of the
    /// snapshot's own end marker, a message of their own, and the replay is cut
    /// after them: by its place that message is the end marker of a state that is
    /// short by those bytes, and only how the replay ended tells.
    #[test]
    fn a_replay_that_ended_short_of_the_recording_gives_no_snapshot() {
        let end = marker(SERVICE_SNAPSHOT_TYPE_ID, POSITION, SnapshotMark::End, 0);
        let mut state = vec![7u8; SERVICE_SNAPSHOT_STATE_MESSAGE_LENGTH];
        state.extend_from_slice(&end);
        let whole = snapshot(&[1], &state);
        let cut = &whole[..whole.len() - 1];
        assert_eq!(cut.last(), Some(&end), "the state's last message");
        let reader_of = |messages: &[Vec<u8>]| {
            let mut reader = SnapshotReader::new();
            for message in messages {
                reader.message(message, true);
            }
            reader
        };
        for reason in [
            Some(ReplayStopReason::Stopped),
            Some(ReplayStopReason::NotConnected),
            Some(ReplayStopReason::Failed),
            None,
        ] {
            assert!(reader_of(cut).finish_ended(reason).is_none(), "{reason:?}");
            assert!(
                reader_of(&whole).finish_ended(reason).is_none(),
                "{reason:?}"
            );
        }
        // What the cut replay would give were the reason not read: a state short by
        // the marker's bytes.
        let short = reader_of(cut).finish().expect("a snapshot, by its place");
        assert_eq!(short.state.len() + end.len(), state.len());
        // The recording played to its end gives the state whole.
        let taken = reader_of(&whole)
            .finish_ended(Some(ReplayStopReason::Ended))
            .expect("the snapshot");
        assert_eq!(taken.state, state);
    }

    /// The reader's bound on a state, at the bound: a state of exactly the bound is
    /// read, and one byte more is no snapshot. The bound a service reads with is
    /// the one it writes with, 1 GiB; the test reaches a smaller one.
    #[test]
    fn a_state_at_the_readers_bound_is_read_and_one_byte_more_is_not() {
        let state = vec![3u8; 2 * SERVICE_SNAPSHOT_STATE_MESSAGE_LENGTH + 1];
        let read_with = |limit: usize| {
            let mut reader = SnapshotReader::with_state_limit(limit);
            for message in &snapshot(&[], &state) {
                reader.message(message, true);
            }
            reader.finish()
        };
        let taken = read_with(state.len()).expect("a state at the bound");
        assert_eq!(taken.state, state);
        assert!(
            read_with(state.len() - 1).is_none(),
            "one byte past the bound"
        );
        assert_eq!(
            SnapshotReader::new().state_limit,
            MAX_SERVICE_SNAPSHOT_STATE_LENGTH
        );
    }

    #[test]
    fn a_recording_that_is_no_services_snapshot_is_not_read() {
        let state = [7u8; 1_500];
        let whole = snapshot(&[0, 1], &state);
        assert!(read(&whole).is_some());
        // Nothing, and what was written cut short at each message. Cut after a piece
        // of the state, the piece is taken for the end marker and is none.
        for kept in 0..whole.len() {
            assert!(read(&whole[..kept]).is_none(), "{kept} messages");
        }
        // A message that arrived as fragments.
        let mut reader = SnapshotReader::new();
        for (index, message) in whole.iter().enumerate() {
            reader.message(message, index != 1);
        }
        assert!(reader.finish().is_none());
        // The consensus module's type id, in the begin marker and in the end marker.
        let mut modules = whole.clone();
        modules[0] = marker(1, POSITION, SnapshotMark::Begin, 0);
        assert!(read(&modules).is_none());
        let mut modules = whole.clone();
        *modules.last_mut().expect("a marker") = marker(1, POSITION, SnapshotMark::End, 0);
        assert!(read(&modules).is_none());
        // Position 0, and an end marker of another position.
        let mut zero = whole.clone();
        zero[0] = marker(SERVICE_SNAPSHOT_TYPE_ID, 0, SnapshotMark::Begin, 0);
        assert!(read(&zero).is_none());
        let mut other = whole.clone();
        *other.last_mut().expect("a marker") = marker(
            SERVICE_SNAPSHOT_TYPE_ID,
            POSITION + 32,
            SnapshotMark::End,
            0,
        );
        assert!(read(&other).is_none());
        // No section marker: the end marker where the section marker belongs.
        let mut unsectioned = whole.clone();
        unsectioned[3] = marker(SERVICE_SNAPSHOT_TYPE_ID, POSITION, SnapshotMark::End, 0);
        assert!(read(&unsectioned).is_none());
        // A section marker with the begin marker's index.
        let mut indexed = whole.clone();
        indexed[3] = marker(SERVICE_SNAPSHOT_TYPE_ID, POSITION, SnapshotMark::Section, 0);
        assert!(read(&indexed).is_none());
        // One session twice, two out of order, and more than the table holds.
        assert!(read(&snapshot(&[1, 1], &state)).is_none());
        assert!(read(&snapshot(&[4, 1], &state)).is_none());
        let eleven: Vec<i64> = (0..11).collect();
        assert!(read(&snapshot(&eleven, &state)).is_none());
        let ten: Vec<i64> = (0..10).collect();
        assert!(read(&snapshot(&ten, &state)).is_some());
        // A message of the consensus module's snapshot among the sessions.
        let mut modules = whole;
        modules[1] = bytes(&Message::ConsensusModule {
            next_session_id: 2,
            next_service_session_id: 0,
            log_service_session_id: 0,
            pending_message_capacity: 0,
        });
        assert!(read(&modules).is_none());
    }

    /// A snapshot's messages with timers between its sessions and its section marker.
    fn snapshot_with_timers(ids: &[i64], timers: &[(i64, i64)], state: &[u8]) -> Vec<Vec<u8>> {
        let mut messages = snapshot(ids, state);
        let at = 1 + ids.len();
        for (index, (correlation_id, deadline)) in timers.iter().enumerate() {
            messages.insert(
                at + index,
                bytes(&Message::Timer {
                    correlation_id: *correlation_id,
                    deadline: *deadline,
                }),
            );
        }
        messages
    }

    /// A service's snapshot holds its pending timers between its sessions and its
    /// section marker, in the rising order of their ids, and a service that starts
    /// from it holds them, each to be sent to its member (`spec/cluster.md`, "A
    /// service's snapshot" and "A service's start"; ADR 0151). A snapshot whose
    /// timers are out of order, twice the same, after the section marker's place or
    /// more than a service holds is no snapshot, and one with no timer reads as
    /// version 1's did.
    #[test]
    fn a_snapshots_timers_are_read_in_order_and_taken_by_the_service() {
        let timers = [(-4, 900), (7, 100), (i64::MAX, -1)];
        let taken = read(&snapshot_with_timers(&[3, 8], &timers, b"state")).expect("the snapshot");
        assert_eq!(taken.timers, timers);
        assert_eq!(taken.sessions.len(), 2);
        assert_eq!(taken.state, b"state");
        let mut service = ClusterService::new();
        assert!(service.take(&taken));
        assert_eq!(service.timers.iter().collect::<Vec<_>>(), timers);
        // The service reads history still, so it tells nothing; once it has read
        // it, the snapshot's timers are what it tells first.
        assert_eq!(service.timers.next_word(), None);
        service.timers.release();
        assert_eq!(
            service.timers.next_word(),
            Some(TimerWord::Schedule {
                correlation_id: -4,
                deadline: 900
            })
        );
        // With no timer, and with timers and no session.
        assert_eq!(
            read(&snapshot_with_timers(&[3], &[], b"s"))
                .expect("the snapshot")
                .timers,
            []
        );
        assert_eq!(
            read(&snapshot_with_timers(&[], &[(1, 2)], b""))
                .expect("the snapshot")
                .timers,
            [(1, 2)]
        );
        // Out of order, and the same id twice.
        assert!(read(&snapshot_with_timers(&[3], &[(7, 1), (6, 1)], b"s")).is_none());
        assert!(read(&snapshot_with_timers(&[3], &[(7, 1), (7, 2)], b"s")).is_none());
        // A session after a timer.
        let mut messages = snapshot_with_timers(&[3], &[(7, 1)], b"s");
        messages.insert(3, session(9));
        assert!(read(&messages).is_none());
        // As many as a service holds, and one more.
        let full: Vec<(i64, i64)> = (0..MAX_SERVICE_TIMERS as i64).map(|id| (id, id)).collect();
        let taken = read(&snapshot_with_timers(&[], &full, b"")).expect("the snapshot");
        assert_eq!(taken.timers.len(), MAX_SERVICE_TIMERS);
        let mut service = ClusterService::new();
        assert!(service.take(&taken));
        assert_eq!(service.timers.len(), MAX_SERVICE_TIMERS);
        let mut more = full.clone();
        more.push((MAX_SERVICE_TIMERS as i64, 0));
        assert!(read(&snapshot_with_timers(&[], &more, b"")).is_none());
        // A snapshot that the table does not take leaves the service with no timer
        // and no session of it.
        let mut service = ClusterService::new();
        let unordered = Snapshot {
            log_position: POSITION,
            leadership_term_id: TERM,
            sessions: vec![(0, CHANNEL.to_vec())],
            timers: vec![(2, 0), (1, 0)],
            state: Vec::new(),
        };
        assert!(!service.take(&unordered));
        assert!(service.timers.is_empty());
        assert_eq!((service.position(), service.sessions()), (0, 0));
    }

    #[test]
    fn a_service_takes_a_snapshots_sessions_position_and_term_when_its_table_holds_them() {
        let mut service = ClusterService::new();
        let taken = read(&snapshot(&[3, 8], b"state")).expect("the snapshot");
        assert!(service.take(&taken));
        assert_eq!(service.position(), POSITION);
        assert_eq!(service.sessions(), 2);
        assert_eq!(service.term_leader, (TERM, ASK_MEMBER));
        let held: Vec<(i64, &[u8])> = service
            .sessions
            .iter()
            .filter(|session| session.open)
            .map(|session| (session.id, &session.channel[..session.channel_length]))
            .collect();
        assert_eq!(held, [(3, CHANNEL), (8, CHANNEL)]);
        // A channel longer than a session holds: nothing is taken.
        let mut service = ClusterService::new();
        let long = Snapshot {
            log_position: POSITION,
            leadership_term_id: TERM,
            sessions: vec![(0, vec![b'h'; MAX_RESPONSE_CHANNEL_LENGTH + 1])],
            timers: Vec::new(),
            state: Vec::new(),
        };
        assert!(!service.take(&long));
        assert_eq!((service.position(), service.sessions()), (0, 0));
        assert_eq!(service.term_leader, (-1, -1));
    }
}
