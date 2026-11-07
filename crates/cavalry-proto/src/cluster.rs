//! The cluster protocol's messages, members string and arithmetic: `spec/cluster.md`,
//! version 0 (ADR 0058; ADR 0052, feature 2).
//!
//! A [`Message`] is one message of a cluster stream, the log, consensus, ingress or
//! egress stream or a snapshot, under the 8-byte message header of schema 1, version 0.
//! [`Message::parse`] runs the spec's checks in the spec's order and borrows the
//! variable fields and the payload from the bytes, so decoding allocates nothing;
//! [`Message::write`] puts a message in place. [`parse_members`] reads a members string
//! into a caller's array, and the free functions compute what "Roles and the election"
//! and "The log" define:
//!
//! - [`quorum`], the quorum of a member count, and [`commit_position`], the quorum
//!   position and the commit position from the members' append positions;
//! - [`log_at_least`], the order of two logs, and [`candidate`], whether a canvassing
//!   member nominates itself, unanimous or by quorum;
//! - [`request_vote`], a reader's vote on a request vote and its candidate term after
//!   it, and [`ballot`], whether a candidate wins its ballot;
//! - [`canvass_position_action`], what a leader does with a canvass position;
//! - [`new_leadership_term_action`], what a reader of new leadership term does with its
//!   log, and [`next_fields`], the next fields a leader writes into it;
//! - [`replication_stop`], where a follower's log replication stops, and
//!   [`replication_record`], the terms it records there;
//! - [`replay_end`], where a leader's replay begins and ends;
//! - [`entry_position`], the position after an entry.
//!
//! A log leadership term id, wherever one is given, is the spec's: the highest term in a
//! member's record of recorded terms, never the term of its last entry. What a member
//! does with a decoded message is `cavalry-raft`'s; this module is the bytes and the
//! rules.
//!
//! ```
//! use cavalry_proto::cluster::{ClusterError, Message, Stream};
//!
//! fn main() -> Result<(), ClusterError> {
//!     // The leader: its heartbeat to a follower, written into a claim on the consensus stream.
//!     let message = Message::CommitPosition { leadership_term_id: 3, log_position: 98304, leader_member_id: 0 };
//!     let mut claim = [0u8; 64];
//!     let length = message.write(&mut claim)?;
//!     assert_eq!(length, message.encoded_length());
//!     assert_eq!(length, 28); // the 8-byte header and the template's 20-byte block
//!
//!     // The follower: the message decoded in place from the consensus stream.
//!     let decoded = Message::parse(Stream::Consensus, &claim[..length])?;
//!     assert_eq!(decoded, message);
//!
//!     // The same bytes on the log stream are refused: the template is not one it carries.
//!     let rejected = Message::parse(Stream::Log, &claim[..length]).unwrap_err();
//!     assert_eq!(rejected.reason(), "stream");
//!     println!("{rejected}");
//!     Ok(())
//! }
//! ```

use core::fmt;

/// The schema id every message of `spec/cluster.md` carries.
pub const SCHEMA_ID: u16 = 1;

/// The version of `spec/cluster.md` this crate reads and writes.
pub const VERSION: u16 = 0;

/// The message header's length: block length, template id, schema id and version.
pub const HEADER_LENGTH: usize = 8;

/// A protocol version field's value for this version, in semantic form.
pub const PROTOCOL_VERSION: i32 = 0;

/// An id or a position that is none: Aeron's null value (`spec/cluster.md`, "Encoding").
pub const NULL_VALUE: i64 = -1;

/// How many members a members string may name (`spec/cluster.md`, "Values").
pub const MAX_MEMBERS: usize = 8;

/// How many sessions a cluster holds open or connecting.
pub const MAX_SESSIONS: usize = 10;

/// The log stream's id.
pub const LOG_STREAM_ID: i32 = 100;

/// The ingress stream's id.
pub const INGRESS_STREAM_ID: i32 = 101;

/// The egress stream's id.
pub const EGRESS_STREAM_ID: i32 = 102;

/// The consensus stream's id.
pub const CONSENSUS_STREAM_ID: i32 = 108;

/// The leader heartbeat interval, in nanoseconds: 200 ms.
pub const LEADER_HEARTBEAT_INTERVAL_NS: u64 = 200_000_000;

/// The leader heartbeat timeout, in nanoseconds: 10 s.
pub const LEADER_HEARTBEAT_TIMEOUT_NS: u64 = 10_000_000_000;

/// The election timeout, in nanoseconds: 1 s.
pub const ELECTION_TIMEOUT_NS: u64 = 1_000_000_000;

/// The election status interval, in nanoseconds: 100 ms.
pub const ELECTION_STATUS_INTERVAL_NS: u64 = 100_000_000;

/// The startup canvass timeout, in nanoseconds: 60 s.
pub const STARTUP_CANVASS_TIMEOUT_NS: u64 = 60_000_000_000;

/// The session timeout, in nanoseconds: 10 s.
pub const SESSION_TIMEOUT_NS: u64 = 10_000_000_000;

/// The keepalive interval, in nanoseconds: 500 ms.
pub const KEEPALIVE_INTERVAL_NS: u64 = 500_000_000;

/// The session connect timeout, in nanoseconds: 5 s.
pub const SESSION_CONNECT_TIMEOUT_NS: u64 = 5_000_000_000;

/// The live-add window's cap, in bytes: a catching-up follower adds its live
/// subscription within the lesser of this and term length ÷ 4 of the leader's position.
pub const LIVE_ADD_MAX_WINDOW: i64 = 1_048_576;

/// The snapshot type id of the consensus module's own snapshot.
pub const CONSENSUS_MODULE_SNAPSHOT_TYPE_ID: i64 = 1;

/// The reason detail of an error session event for a client of another major version.
pub const INVALID_CLIENT_VERSION: &[u8] = b"invalid client version";

/// The reason detail of an error session event when the sessions are at their bound.
pub const CONCURRENT_SESSION_LIMIT: &[u8] = b"concurrent session limit";

/// The reason detail of an error session event for a connect whose response stream id
/// is not the egress stream's or whose response channel is not
/// `cavalry:udp?endpoint=<host:port>`.
pub const INVALID_RESPONSE_CHANNEL: &[u8] = b"invalid response channel";

/// How many redirects or refusals a member holds unsent at a time.
pub const MAX_PENDING_ANSWERS: usize = 10;

/// The docs anchor every error message names.
const DOC: &str = "docs/reference/cluster.md#what-a-reader-rejects";

/// The streams of `spec/cluster.md`, "Members and streams", and a snapshot, each
/// carrying its own templates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Stream {
    /// The leader's log, stream id 100.
    Log,
    /// The consensus messages between members, stream id 108.
    Consensus,
    /// A client's messages to a member, stream id 101.
    Ingress,
    /// The leader's events and replies to a client, stream id 102.
    Egress,
    /// A snapshot's messages, which a recording holds.
    Snapshot,
}

impl Stream {
    /// The stream id, or `None` for a snapshot, which is not a stream.
    pub const fn stream_id(self) -> Option<i32> {
        match self {
            Stream::Log => Some(LOG_STREAM_ID),
            Stream::Consensus => Some(CONSENSUS_STREAM_ID),
            Stream::Ingress => Some(INGRESS_STREAM_ID),
            Stream::Egress => Some(EGRESS_STREAM_ID),
            Stream::Snapshot => None,
        }
    }

    /// The stream's name as the vectors spell it.
    pub const fn name(self) -> &'static str {
        match self {
            Stream::Log => "log",
            Stream::Consensus => "consensus",
            Stream::Ingress => "ingress",
            Stream::Egress => "egress",
            Stream::Snapshot => "snapshot",
        }
    }

    /// Whether the stream carries the template (`spec/cluster.md`, "What a reader
    /// rejects", `stream`).
    pub const fn carries(self, template: Template) -> bool {
        match self {
            Stream::Log => matches!(
                template,
                Template::SessionMessage
                    | Template::TimerEvent
                    | Template::SessionOpenEvent
                    | Template::SessionCloseEvent
                    | Template::ClusterAction
                    | Template::NewLeadershipTermEvent
                    | Template::MembershipChangeEvent
            ),
            Stream::Consensus => matches!(
                template,
                Template::CanvassPosition
                    | Template::RequestVote
                    | Template::Vote
                    | Template::NewLeadershipTerm
                    | Template::AppendPosition
                    | Template::CommitPosition
                    | Template::CatchupPosition
                    | Template::StopCatchup
            ),
            Stream::Ingress => matches!(
                template,
                Template::SessionMessage
                    | Template::SessionConnect
                    | Template::SessionClose
                    | Template::SessionKeepalive
            ),
            Stream::Egress => matches!(
                template,
                Template::SessionMessage | Template::SessionEvent | Template::NewLeaderEvent
            ),
            Stream::Snapshot => matches!(template, Template::SnapshotMarker),
        }
    }
}

/// The templates of this version, by Aeron's ids (`spec/cluster.md`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum Template {
    /// The session message header and its payload.
    SessionMessage = 1,
    /// A session event, the leader's answer to a connect or a closed session.
    SessionEvent = 2,
    /// A client's session connect.
    SessionConnect = 3,
    /// A client's session close.
    SessionClose = 4,
    /// A client's session keepalive.
    SessionKeepalive = 5,
    /// The new leader's word to a session.
    NewLeaderEvent = 6,
    /// A timer that fell due, a log entry.
    TimerEvent = 20,
    /// A session opened, a log entry.
    SessionOpenEvent = 21,
    /// A session closed, a log entry.
    SessionCloseEvent = 22,
    /// A cluster action, a log entry.
    ClusterAction = 23,
    /// A term's first entry.
    NewLeadershipTermEvent = 24,
    /// A member joined or quit, a log entry.
    MembershipChangeEvent = 25,
    /// A member's report of its log while it has no leader.
    CanvassPosition = 50,
    /// A candidate's request.
    RequestVote = 51,
    /// A member's answer to a candidate.
    Vote = 52,
    /// The leader's announcement of its term.
    NewLeadershipTerm = 53,
    /// A follower's append position.
    AppendPosition = 54,
    /// The leader's commit position and heartbeat.
    CommitPosition = 55,
    /// A follower's request for the log from a position.
    CatchupPosition = 56,
    /// The leader's word that a catch-up is over.
    StopCatchup = 57,
    /// A snapshot's begin, section or end.
    SnapshotMarker = 100,
}

impl Template {
    /// The template for an id, or `None` for one this version does not name, the
    /// reserved 7 and 8 among them.
    pub const fn from_id(id: u16) -> Option<Template> {
        Some(match id {
            1 => Template::SessionMessage,
            2 => Template::SessionEvent,
            3 => Template::SessionConnect,
            4 => Template::SessionClose,
            5 => Template::SessionKeepalive,
            6 => Template::NewLeaderEvent,
            20 => Template::TimerEvent,
            21 => Template::SessionOpenEvent,
            22 => Template::SessionCloseEvent,
            23 => Template::ClusterAction,
            24 => Template::NewLeadershipTermEvent,
            25 => Template::MembershipChangeEvent,
            50 => Template::CanvassPosition,
            51 => Template::RequestVote,
            52 => Template::Vote,
            53 => Template::NewLeadershipTerm,
            54 => Template::AppendPosition,
            55 => Template::CommitPosition,
            56 => Template::CatchupPosition,
            57 => Template::StopCatchup,
            100 => Template::SnapshotMarker,
            _ => return None,
        })
    }

    /// The template id.
    pub const fn id(self) -> u16 {
        self as u16
    }

    /// The block length: the fixed fields' length after the header.
    pub const fn block_length(self) -> u16 {
        match self {
            Template::SessionMessage => 24,
            Template::SessionEvent => 44,
            Template::SessionConnect => 16,
            Template::SessionClose | Template::SessionKeepalive => 16,
            Template::NewLeaderEvent => 20,
            Template::TimerEvent => 24,
            Template::SessionOpenEvent => 36,
            Template::SessionCloseEvent => 28,
            Template::ClusterAction => 32,
            Template::NewLeadershipTermEvent => 48,
            Template::MembershipChangeEvent => 40,
            Template::CanvassPosition | Template::RequestVote => 32,
            Template::Vote => 36,
            Template::NewLeadershipTerm => 96,
            Template::AppendPosition => 21,
            Template::CommitPosition | Template::CatchupPosition => 20,
            Template::StopCatchup => 12,
            Template::SnapshotMarker => 40,
        }
    }

    /// The template's name as the vectors spell it.
    pub const fn name(self) -> &'static str {
        match self {
            Template::SessionMessage => "session-message",
            Template::SessionEvent => "session-event",
            Template::SessionConnect => "session-connect",
            Template::SessionClose => "session-close",
            Template::SessionKeepalive => "session-keepalive",
            Template::NewLeaderEvent => "new-leader-event",
            Template::TimerEvent => "timer-event",
            Template::SessionOpenEvent => "session-open-event",
            Template::SessionCloseEvent => "session-close-event",
            Template::ClusterAction => "cluster-action",
            Template::NewLeadershipTermEvent => "new-leadership-term-event",
            Template::MembershipChangeEvent => "membership-change-event",
            Template::CanvassPosition => "canvass-position",
            Template::RequestVote => "request-vote",
            Template::Vote => "vote",
            Template::NewLeadershipTerm => "new-leadership-term",
            Template::AppendPosition => "append-position",
            Template::CommitPosition => "commit-position",
            Template::CatchupPosition => "catchup-position",
            Template::StopCatchup => "stop-catchup",
            Template::SnapshotMarker => "snapshot-marker",
        }
    }
}

/// A session event's code (`spec/cluster.md`, "Session event").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum EventCode {
    /// The connect was accepted and the session is open.
    Ok = 0,
    /// The connect was refused; the detail says why.
    Error = 1,
    /// The member is not the leader; the detail lists the ingress endpoints.
    Redirect = 2,
    /// Reserved: Aeron's authentication, sent by no member of this version.
    AuthenticationRejected = 3,
    /// The session was closed; the detail is the reason.
    Closed = 4,
}

/// Why a session closed (`spec/cluster.md`, "Session close event").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum CloseReason {
    /// The client sent session close.
    ClientAction = 0,
    /// The service closed it; out of this version.
    ServiceAction = 1,
    /// The session timeout passed.
    Timeout = 2,
}

/// What a cluster action asks (`spec/cluster.md`, "Cluster action").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum ClusterAction {
    /// Suspend the ingress.
    Suspend = 0,
    /// Resume the ingress.
    Resume = 1,
    /// Take a snapshot.
    Snapshot = 2,
}

/// How the membership changed (`spec/cluster.md`, "Membership change event").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum ChangeType {
    /// A member joined.
    Join = 0,
    /// A member quit.
    Quit = 1,
}

/// A snapshot marker's place in the snapshot (`spec/cluster.md`, "Snapshot marker").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum SnapshotMark {
    /// The first message.
    Begin = 0,
    /// The first message of a section.
    Section = 1,
    /// The last message.
    End = 2,
}

/// The unit of the cluster's time: milliseconds, the one unit of this version.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum TimeUnit {
    /// Milliseconds since 1970-01-01T00:00:00Z.
    Millis = 0,
}

macro_rules! enumeration {
    ($type:ident, $($value:expr => $variant:ident),+ $(,)?) => {
        impl $type {
            /// The variant for a wire value, or `None` for one the table does not name.
            pub const fn from_code(code: i32) -> Option<$type> {
                match code {
                    $($value => Some($type::$variant),)+
                    _ => None,
                }
            }

            /// The wire value.
            pub const fn code(self) -> i32 {
                self as i32
            }
        }
    };
}

enumeration!(EventCode, 0 => Ok, 1 => Error, 2 => Redirect, 3 => AuthenticationRejected, 4 => Closed);
enumeration!(CloseReason, 0 => ClientAction, 1 => ServiceAction, 2 => Timeout);
enumeration!(ClusterAction, 0 => Suspend, 1 => Resume, 2 => Snapshot);
enumeration!(ChangeType, 0 => Join, 1 => Quit);
enumeration!(SnapshotMark, 0 => Begin, 1 => Section, 2 => End);
enumeration!(TimeUnit, 0 => Millis);

/// Why a message was refused: the checks of `spec/cluster.md`, "What a reader rejects",
/// and [`ClusterError::reason`] is the name the spec and the vectors use. A writer
/// reports [`ClusterError::TooShort`] for a buffer too small.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ClusterError {
    /// The message, or the buffer to write into, is shorter than the header or the
    /// block needs.
    TooShort {
        /// How many bytes there are.
        given: usize,
        /// How many are needed.
        needed: usize,
    },
    /// The schema id is not 1.
    Schema {
        /// The schema id found.
        found: u16,
    },
    /// The version is not 0, or a protocol version field's major is not 0.
    Version {
        /// The version found: the header's, or the field's value.
        found: i32,
    },
    /// The template id is not one this version names.
    Template {
        /// The template id found.
        found: u16,
    },
    /// The template is not one the stream carries.
    Stream {
        /// The template found.
        template: Template,
        /// The stream it arrived on.
        stream: Stream,
    },
    /// The block length is not the template's.
    BlockLength {
        /// The block length found.
        found: u16,
        /// The template's.
        expected: u16,
    },
    /// A variable field runs past the message, or bytes remain after the last field.
    Length {
        /// The bytes available after the field's length, or left over.
        available: usize,
        /// The length the field claims, or 0 for bytes left over.
        claimed: usize,
    },
    /// An enumeration holds a value its table does not name.
    Enum {
        /// The field's name.
        field: &'static str,
        /// The value found.
        found: i32,
    },
    /// An ASCII field holds a byte outside 0x20 to 0x7E.
    Ascii {
        /// The field's name.
        field: &'static str,
        /// The byte found.
        byte: u8,
    },
    /// A member id is below what its field allows.
    MemberId {
        /// The field's name.
        field: &'static str,
        /// The value found.
        found: i32,
    },
    /// A position is below what its field allows.
    Position {
        /// The field's name.
        field: &'static str,
        /// The value found.
        found: i64,
    },
    /// A term id is below −1.
    Term {
        /// The field's name.
        field: &'static str,
        /// The value found.
        found: i64,
    },
}

impl ClusterError {
    /// The reason's name in `spec/cluster.md`, "What a reader rejects".
    pub const fn reason(self) -> &'static str {
        match self {
            ClusterError::TooShort { .. } => "too-short",
            ClusterError::Schema { .. } => "schema",
            ClusterError::Version { .. } => "version",
            ClusterError::Template { .. } => "template",
            ClusterError::Stream { .. } => "stream",
            ClusterError::BlockLength { .. } => "block-length",
            ClusterError::Length { .. } => "length",
            ClusterError::Enum { .. } => "enum",
            ClusterError::Ascii { .. } => "ascii",
            ClusterError::MemberId { .. } => "member-id",
            ClusterError::Position { .. } => "position",
            ClusterError::Term { .. } => "term",
        }
    }
}

impl fmt::Display for ClusterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            ClusterError::TooShort { given, needed } => write!(
                f,
                "cluster message rejected (too-short): {given} bytes, {needed} needed; the \
                 message is truncated or the writer runs another Cavalry version; see {DOC}"
            ),
            ClusterError::Schema { found } => write!(
                f,
                "cluster message rejected (schema): schema id {found}, this reader knows \
                 {SCHEMA_ID}; the stream carries another protocol; see {DOC}"
            ),
            ClusterError::Version { found } => write!(
                f,
                "cluster message rejected (version): version {found}, this reader knows \
                 {VERSION}; the writer runs another Cavalry version, upgrade the older side; \
                 see {DOC}"
            ),
            ClusterError::Template { found } => write!(
                f,
                "cluster message rejected (template): template {found} is not one of version \
                 {VERSION}; the writer runs another Cavalry version or a reserved message; see \
                 {DOC}"
            ),
            ClusterError::Stream { template, stream } => write!(
                f,
                "cluster message rejected (stream): {} on the {} stream, which does not carry \
                 it; the writer sent it to the wrong endpoint; see {DOC}",
                template.name(),
                stream.name()
            ),
            ClusterError::BlockLength { found, expected } => write!(
                f,
                "cluster message rejected (block-length): block length {found}, the template's \
                 is {expected}; the writer runs another Cavalry version; see {DOC}"
            ),
            ClusterError::Length { available, claimed } => write!(
                f,
                "cluster message rejected (length): a field of {claimed} bytes with {available} \
                 available, or {available} bytes left after the last field; the message is \
                 corrupt or the writer does not conform; see {DOC}"
            ),
            ClusterError::Enum { field, found } => write!(
                f,
                "cluster message rejected (enum): {field} is {found}, which this version does \
                 not name; the writer runs another Cavalry version; see {DOC}"
            ),
            ClusterError::Ascii { field, byte } => write!(
                f,
                "cluster message rejected (ascii): {field} holds the byte 0x{byte:02x}, outside \
                 printable ASCII; the writer does not conform; see {DOC}"
            ),
            ClusterError::MemberId { field, found } => write!(
                f,
                "cluster message rejected (member-id): {field} is {found}, below what the field \
                 allows; the writer does not conform; see {DOC}"
            ),
            ClusterError::Position { field, found } => write!(
                f,
                "cluster message rejected (position): {field} is {found}, below what the field \
                 allows; the writer does not conform; see {DOC}"
            ),
            ClusterError::Term { field, found } => write!(
                f,
                "cluster message rejected (term): {field} is {found}, below -1; the writer does \
                 not conform; see {DOC}"
            ),
        }
    }
}

impl core::error::Error for ClusterError {}

/// One message of `spec/cluster.md`, decoded from or to be written as a message of its
/// stream. Variable fields and the payload borrow the bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Message<'a> {
    /// Template 1: the session message header and the payload after it.
    SessionMessage {
        /// The term the message belongs to.
        leadership_term_id: i64,
        /// The session's id.
        cluster_session_id: i64,
        /// The cluster's time; 0 on the ingress stream.
        timestamp: i64,
        /// The service's message, the rest of the bytes.
        payload: &'a [u8],
    },
    /// Template 2.
    SessionEvent {
        /// The session's id, or −1 for none.
        cluster_session_id: i64,
        /// The connect's correlation id, or −1 for an event the client did not ask for.
        correlation_id: i64,
        /// The sender's term.
        leadership_term_id: i64,
        /// The leader's member id, or −1 for none.
        leader_member_id: i32,
        /// The code.
        code: EventCode,
        /// The cluster's protocol version, in semantic form, read whatever its major: a
        /// client that reads a major not its own reports it to its caller and sends
        /// nothing more.
        version: i32,
        /// The leader heartbeat timeout in nanoseconds.
        leader_heartbeat_timeout_ns: i64,
        /// Empty for ok, the reason for error and closed, the ingress endpoints for redirect.
        detail: &'a [u8],
    },
    /// Template 3.
    SessionConnect {
        /// The client's correlation id.
        correlation_id: i64,
        /// The egress stream id.
        response_stream_id: i32,
        /// The client's protocol version, in semantic form.
        version: i32,
        /// The channel the client subscribes to on the egress stream.
        response_channel: &'a [u8],
        /// Empty in this version.
        encoded_credentials: &'a [u8],
        /// Any text the client gives, empty allowed.
        client_info: &'a [u8],
    },
    /// Template 4.
    SessionClose {
        /// The term the client knows.
        leadership_term_id: i64,
        /// The session's id.
        cluster_session_id: i64,
    },
    /// Template 5.
    SessionKeepalive {
        /// The term the client knows.
        leadership_term_id: i64,
        /// The session's id.
        cluster_session_id: i64,
    },
    /// Template 6.
    NewLeaderEvent {
        /// The new term.
        leadership_term_id: i64,
        /// The session's id.
        cluster_session_id: i64,
        /// The new leader's member id.
        leader_member_id: i32,
        /// The ingress endpoints, `<id>=<host:port>` separated by `,`.
        ingress_endpoints: &'a [u8],
    },
    /// Template 20.
    TimerEvent {
        /// The term.
        leadership_term_id: i64,
        /// The timer's correlation id.
        correlation_id: i64,
        /// The cluster's time.
        timestamp: i64,
    },
    /// Template 21.
    SessionOpenEvent {
        /// The term.
        leadership_term_id: i64,
        /// The session connect's correlation id.
        correlation_id: i64,
        /// The id the leader gave the session.
        cluster_session_id: i64,
        /// The cluster's time.
        timestamp: i64,
        /// The client's egress stream id.
        response_stream_id: i32,
        /// The client's response channel.
        response_channel: &'a [u8],
        /// Empty in this version.
        encoded_principal: &'a [u8],
    },
    /// Template 22.
    SessionCloseEvent {
        /// The term.
        leadership_term_id: i64,
        /// The session's id.
        cluster_session_id: i64,
        /// The cluster's time.
        timestamp: i64,
        /// Why the session closed.
        close_reason: CloseReason,
    },
    /// Template 23.
    ClusterAction {
        /// The term.
        leadership_term_id: i64,
        /// The entry's position.
        log_position: i64,
        /// The cluster's time.
        timestamp: i64,
        /// The action.
        action: ClusterAction,
        /// 0 in this version.
        flags: i32,
    },
    /// Template 24.
    NewLeadershipTermEvent {
        /// The new term.
        leadership_term_id: i64,
        /// The term base log position.
        log_position: i64,
        /// The cluster's time.
        timestamp: i64,
        /// The same position.
        term_base_log_position: i64,
        /// The leader's member id.
        leader_member_id: i32,
        /// The session id of the leader's log publication.
        log_session_id: i32,
        /// Milliseconds.
        time_unit: TimeUnit,
        /// The service's version, 0 for none.
        app_version: i32,
    },
    /// Template 25.
    MembershipChangeEvent {
        /// The term.
        leadership_term_id: i64,
        /// The entry's position.
        log_position: i64,
        /// The cluster's time.
        timestamp: i64,
        /// The leader's member id.
        leader_member_id: i32,
        /// The members after the change.
        cluster_size: i32,
        /// Join or quit.
        change_type: ChangeType,
        /// The member that joined or quit.
        member_id: i32,
        /// The members string after the change.
        cluster_members: &'a [u8],
    },
    /// Template 50.
    CanvassPosition {
        /// The sender's log leadership term id: the highest term in its record of
        /// recorded terms (`spec/cluster.md`, "The log"), which is not the term of its
        /// last entry; −1 for none.
        log_leadership_term_id: i64,
        /// The sender's append position.
        log_position: i64,
        /// The sender's candidate term: the highest term it has voted in, proposed or
        /// followed, at least its log leadership term id; −1 for none.
        leadership_term_id: i64,
        /// The sender's member id.
        follower_member_id: i32,
        /// 0.
        protocol_version: i32,
    },
    /// Template 51.
    RequestVote {
        /// The candidate's log leadership term id, the highest term in its record, as
        /// canvass position's; −1 for none.
        log_leadership_term_id: i64,
        /// The candidate's append position.
        log_position: i64,
        /// The term the candidate proposes to lead.
        candidate_term_id: i64,
        /// The candidate's member id.
        candidate_member_id: i32,
        /// 0.
        protocol_version: i32,
    },
    /// Template 52.
    Vote {
        /// The request's candidate term id.
        candidate_term_id: i64,
        /// The voter's log leadership term id, the highest term in its record, as
        /// canvass position's; −1 for none.
        log_leadership_term_id: i64,
        /// The voter's append position.
        log_position: i64,
        /// The request's candidate member id.
        candidate_member_id: i32,
        /// The voter's member id.
        follower_member_id: i32,
        /// For the candidate, or against.
        vote: bool,
    },
    /// Template 53.
    NewLeadershipTerm {
        /// The term the receiver reported, −1 for none.
        log_leadership_term_id: i64,
        /// The lowest term in the leader's record above the log leadership term id, the
        /// leader's own when none lies between; −1 when the log leadership term id is
        /// the leader's own term ([`next_fields`]).
        next_leadership_term_id: i64,
        /// That term's base position, or −1.
        next_term_base_log_position: i64,
        /// That term's end position, or −1.
        next_log_position: i64,
        /// The leader's term.
        leadership_term_id: i64,
        /// The position at which the leader's term began.
        term_base_log_position: i64,
        /// The leader's append position.
        log_position: i64,
        /// The recording id of the leader's recording of the log, or −1.
        leader_recording_id: i64,
        /// The cluster's time.
        timestamp: i64,
        /// The leader's member id.
        leader_member_id: i32,
        /// The session id of the leader's log publication.
        log_session_id: i32,
        /// The service's version, 0 for none.
        app_version: i32,
        /// Whether the leader started with the cluster.
        is_startup: bool,
        /// The leader's commit position.
        commit_position: i64,
    },
    /// Template 54.
    AppendPosition {
        /// The sender's log leadership term id, the highest term in its record of
        /// recorded terms, as canvass position's; not the term of its last entry.
        leadership_term_id: i64,
        /// The follower's append position.
        log_position: i64,
        /// The follower's member id.
        follower_member_id: i32,
        /// Bit 0 catch-up; the other bits reserved.
        flags: u8,
    },
    /// Template 55.
    CommitPosition {
        /// The leader's term.
        leadership_term_id: i64,
        /// The commit position.
        log_position: i64,
        /// The leader's member id.
        leader_member_id: i32,
    },
    /// Template 56.
    CatchupPosition {
        /// The leader's term.
        leadership_term_id: i64,
        /// The position the follower wants the log from.
        log_position: i64,
        /// The follower's member id.
        follower_member_id: i32,
        /// The follower's `catchup` endpoint, `host:port`.
        catchup_endpoint: &'a [u8],
    },
    /// Template 57.
    StopCatchup {
        /// The leader's term.
        leadership_term_id: i64,
        /// The follower's member id.
        follower_member_id: i32,
    },
    /// Template 100.
    SnapshotMarker {
        /// Whose snapshot: 1 the consensus module's.
        type_id: i64,
        /// The commit position the snapshot was taken at.
        log_position: i64,
        /// The term at that position.
        leadership_term_id: i64,
        /// The section's index, 0 for begin and end.
        index: i32,
        /// Begin, section or end.
        mark: SnapshotMark,
        /// Milliseconds.
        time_unit: TimeUnit,
        /// The service's version, 0 for none.
        app_version: i32,
    },
}

/// The catch-up bit of an append position's flags.
pub const APPEND_POSITION_FLAG_CATCHUP: u8 = 0x01;

/// A reader of little-endian fields from a message, every read bounds-checked.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, length: usize) -> Result<&'a [u8], ClusterError> {
        let end = self.at.checked_add(length).ok_or(ClusterError::Length {
            available: self.bytes.len().saturating_sub(self.at),
            claimed: length,
        })?;
        let slice = self.bytes.get(self.at..end).ok_or(ClusterError::Length {
            available: self.bytes.len().saturating_sub(self.at),
            claimed: length,
        })?;
        self.at = end;
        Ok(slice)
    }

    fn i64(&mut self) -> Result<i64, ClusterError> {
        let bytes = self.take(8)?;
        let mut array = [0u8; 8];
        array.copy_from_slice(bytes);
        Ok(i64::from_le_bytes(array))
    }

    fn i32(&mut self) -> Result<i32, ClusterError> {
        let bytes = self.take(4)?;
        let mut array = [0u8; 4];
        array.copy_from_slice(bytes);
        Ok(i32::from_le_bytes(array))
    }

    fn u8(&mut self) -> Result<u8, ClusterError> {
        let bytes = self.take(1)?;
        Ok(bytes.first().copied().unwrap_or(0))
    }

    /// A variable field: a u32 length and its bytes.
    fn variable(&mut self) -> Result<&'a [u8], ClusterError> {
        let bytes = self.take(4)?;
        let mut array = [0u8; 4];
        array.copy_from_slice(bytes);
        let length = u32::from_le_bytes(array) as usize;
        self.take(length)
    }

    fn rest(&mut self) -> &'a [u8] {
        let rest = self.bytes.get(self.at..).unwrap_or(&[]);
        self.at = self.bytes.len();
        rest
    }

    fn done(&self) -> Result<(), ClusterError> {
        let left = self.bytes.len().saturating_sub(self.at);
        if left == 0 {
            Ok(())
        } else {
            Err(ClusterError::Length {
                available: left,
                claimed: 0,
            })
        }
    }
}

/// A writer of little-endian fields into a buffer, every write bounds-checked.
struct Writer<'a> {
    buffer: &'a mut [u8],
    at: usize,
    needed: usize,
}

impl Writer<'_> {
    fn put(&mut self, bytes: &[u8]) -> Result<(), ClusterError> {
        let end = self.at + bytes.len();
        match self.buffer.get_mut(self.at..end) {
            Some(slot) => {
                slot.copy_from_slice(bytes);
                self.at = end;
                Ok(())
            }
            None => Err(ClusterError::TooShort {
                given: self.buffer.len(),
                needed: self.needed,
            }),
        }
    }

    fn i64(&mut self, value: i64) -> Result<(), ClusterError> {
        self.put(&value.to_le_bytes())
    }

    fn i32(&mut self, value: i32) -> Result<(), ClusterError> {
        self.put(&value.to_le_bytes())
    }

    fn u8(&mut self, value: u8) -> Result<(), ClusterError> {
        self.put(&[value])
    }

    fn variable(&mut self, bytes: &[u8]) -> Result<(), ClusterError> {
        let length = u32::try_from(bytes.len()).map_err(|_| ClusterError::Length {
            available: u32::MAX as usize,
            claimed: bytes.len(),
        })?;
        self.put(&length.to_le_bytes())?;
        self.put(bytes)
    }
}

fn ascii(field: &'static str, bytes: &[u8]) -> Result<(), ClusterError> {
    match bytes.iter().find(|&&byte| !(0x20..=0x7E).contains(&byte)) {
        Some(&byte) => Err(ClusterError::Ascii { field, byte }),
        None => Ok(()),
    }
}

fn member_id(field: &'static str, found: i32, null_allowed: bool) -> Result<(), ClusterError> {
    let least = if null_allowed { -1 } else { 0 };
    if found < least {
        Err(ClusterError::MemberId { field, found })
    } else {
        Ok(())
    }
}

fn position(field: &'static str, found: i64, null_allowed: bool) -> Result<(), ClusterError> {
    let least = if null_allowed { NULL_VALUE } else { 0 };
    if found < least {
        Err(ClusterError::Position { field, found })
    } else {
        Ok(())
    }
}

fn term(field: &'static str, found: i64) -> Result<(), ClusterError> {
    if found < NULL_VALUE {
        Err(ClusterError::Term { field, found })
    } else {
        Ok(())
    }
}

fn protocol_version(found: i32) -> Result<(), ClusterError> {
    // The major is the value shifted right by 16 bits, arithmetic, so a negative value
    // has a negative major and is rejected (spec/cluster.md, "Encoding").
    if found >> 16 != 0 {
        Err(ClusterError::Version { found })
    } else {
        Ok(())
    }
}

fn bool_field(field: &'static str, found: i32) -> Result<bool, ClusterError> {
    match found {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(ClusterError::Enum { field, found }),
    }
}

macro_rules! enum_field {
    ($type:ident, $field:literal, $found:expr) => {
        $type::from_code($found).ok_or(ClusterError::Enum {
            field: $field,
            found: $found,
        })
    };
}

impl<'a> Message<'a> {
    /// The template.
    pub const fn template(&self) -> Template {
        match self {
            Message::SessionMessage { .. } => Template::SessionMessage,
            Message::SessionEvent { .. } => Template::SessionEvent,
            Message::SessionConnect { .. } => Template::SessionConnect,
            Message::SessionClose { .. } => Template::SessionClose,
            Message::SessionKeepalive { .. } => Template::SessionKeepalive,
            Message::NewLeaderEvent { .. } => Template::NewLeaderEvent,
            Message::TimerEvent { .. } => Template::TimerEvent,
            Message::SessionOpenEvent { .. } => Template::SessionOpenEvent,
            Message::SessionCloseEvent { .. } => Template::SessionCloseEvent,
            Message::ClusterAction { .. } => Template::ClusterAction,
            Message::NewLeadershipTermEvent { .. } => Template::NewLeadershipTermEvent,
            Message::MembershipChangeEvent { .. } => Template::MembershipChangeEvent,
            Message::CanvassPosition { .. } => Template::CanvassPosition,
            Message::RequestVote { .. } => Template::RequestVote,
            Message::Vote { .. } => Template::Vote,
            Message::NewLeadershipTerm { .. } => Template::NewLeadershipTerm,
            Message::AppendPosition { .. } => Template::AppendPosition,
            Message::CommitPosition { .. } => Template::CommitPosition,
            Message::CatchupPosition { .. } => Template::CatchupPosition,
            Message::StopCatchup { .. } => Template::StopCatchup,
            Message::SnapshotMarker { .. } => Template::SnapshotMarker,
        }
    }

    /// Decodes one message of `stream`, running the spec's checks in the spec's order
    /// (`spec/cluster.md`, "What a reader rejects"): the header's length, schema id,
    /// version and template; the stream; the block length; every variable field's
    /// length and the bytes left over; a consensus message's protocol version field's
    /// major, a session connect's or a session event's version read whatever it is;
    /// the enumerations; the ASCII
    /// fields; the member ids; the positions; the term ids.
    pub fn parse(stream: Stream, bytes: &'a [u8]) -> Result<Self, ClusterError> {
        if bytes.len() < HEADER_LENGTH {
            return Err(ClusterError::TooShort {
                given: bytes.len(),
                needed: HEADER_LENGTH,
            });
        }
        let header = bytes.get(..HEADER_LENGTH).unwrap_or(&[]);
        let field = |at: usize| -> u16 {
            let mut array = [0u8; 2];
            array.copy_from_slice(header.get(at..at + 2).unwrap_or(&[0, 0]));
            u16::from_le_bytes(array)
        };
        let (block_length, template_id, schema_id, version) =
            (field(0), field(2), field(4), field(6));
        let needed = HEADER_LENGTH + block_length as usize;
        if bytes.len() < needed {
            return Err(ClusterError::TooShort {
                given: bytes.len(),
                needed,
            });
        }
        if schema_id != SCHEMA_ID {
            return Err(ClusterError::Schema { found: schema_id });
        }
        if version != VERSION {
            return Err(ClusterError::Version {
                found: i32::from(version),
            });
        }
        let template =
            Template::from_id(template_id).ok_or(ClusterError::Template { found: template_id })?;
        if !stream.carries(template) {
            return Err(ClusterError::Stream { template, stream });
        }
        if block_length != template.block_length() {
            return Err(ClusterError::BlockLength {
                found: block_length,
                expected: template.block_length(),
            });
        }
        let mut reader = Reader {
            bytes,
            at: HEADER_LENGTH,
        };
        let message = match template {
            Template::SessionMessage => {
                let leadership_term_id = reader.i64()?;
                let cluster_session_id = reader.i64()?;
                let timestamp = reader.i64()?;
                let payload = reader.rest();
                term("leadership term id", leadership_term_id)?;
                Message::SessionMessage {
                    leadership_term_id,
                    cluster_session_id,
                    timestamp,
                    payload,
                }
            }
            Template::SessionEvent => {
                let cluster_session_id = reader.i64()?;
                let correlation_id = reader.i64()?;
                let leadership_term_id = reader.i64()?;
                let leader_member_id = reader.i32()?;
                let code = reader.i32()?;
                let version = reader.i32()?;
                let leader_heartbeat_timeout_ns = reader.i64()?;
                let detail = reader.variable()?;
                reader.done()?;
                // Any major is read here: a client that reads one not its own reports the
                // cluster's version to its caller (spec/cluster.md, "Sessions").
                let code = enum_field!(EventCode, "code", code)?;
                ascii("detail", detail)?;
                member_id("leader member id", leader_member_id, true)?;
                term("leadership term id", leadership_term_id)?;
                Message::SessionEvent {
                    cluster_session_id,
                    correlation_id,
                    leadership_term_id,
                    leader_member_id,
                    code,
                    version,
                    leader_heartbeat_timeout_ns,
                    detail,
                }
            }
            Template::SessionConnect => {
                let correlation_id = reader.i64()?;
                let response_stream_id = reader.i32()?;
                let version = reader.i32()?;
                let response_channel = reader.variable()?;
                let encoded_credentials = reader.variable()?;
                let client_info = reader.variable()?;
                reader.done()?;
                // Any major is read here: the leader refuses one not its own with
                // `invalid client version` (spec/cluster.md, "Sessions").
                ascii("response channel", response_channel)?;
                ascii("client info", client_info)?;
                Message::SessionConnect {
                    correlation_id,
                    response_stream_id,
                    version,
                    response_channel,
                    encoded_credentials,
                    client_info,
                }
            }
            Template::SessionClose | Template::SessionKeepalive => {
                let leadership_term_id = reader.i64()?;
                let cluster_session_id = reader.i64()?;
                reader.done()?;
                term("leadership term id", leadership_term_id)?;
                if template == Template::SessionClose {
                    Message::SessionClose {
                        leadership_term_id,
                        cluster_session_id,
                    }
                } else {
                    Message::SessionKeepalive {
                        leadership_term_id,
                        cluster_session_id,
                    }
                }
            }
            Template::NewLeaderEvent => {
                let leadership_term_id = reader.i64()?;
                let cluster_session_id = reader.i64()?;
                let leader_member_id = reader.i32()?;
                let ingress_endpoints = reader.variable()?;
                reader.done()?;
                ascii("ingress endpoints", ingress_endpoints)?;
                member_id("leader member id", leader_member_id, false)?;
                term("leadership term id", leadership_term_id)?;
                Message::NewLeaderEvent {
                    leadership_term_id,
                    cluster_session_id,
                    leader_member_id,
                    ingress_endpoints,
                }
            }
            Template::TimerEvent => {
                let leadership_term_id = reader.i64()?;
                let correlation_id = reader.i64()?;
                let timestamp = reader.i64()?;
                reader.done()?;
                term("leadership term id", leadership_term_id)?;
                Message::TimerEvent {
                    leadership_term_id,
                    correlation_id,
                    timestamp,
                }
            }
            Template::SessionOpenEvent => {
                let leadership_term_id = reader.i64()?;
                let correlation_id = reader.i64()?;
                let cluster_session_id = reader.i64()?;
                let timestamp = reader.i64()?;
                let response_stream_id = reader.i32()?;
                let response_channel = reader.variable()?;
                let encoded_principal = reader.variable()?;
                reader.done()?;
                ascii("response channel", response_channel)?;
                term("leadership term id", leadership_term_id)?;
                Message::SessionOpenEvent {
                    leadership_term_id,
                    correlation_id,
                    cluster_session_id,
                    timestamp,
                    response_stream_id,
                    response_channel,
                    encoded_principal,
                }
            }
            Template::SessionCloseEvent => {
                let leadership_term_id = reader.i64()?;
                let cluster_session_id = reader.i64()?;
                let timestamp = reader.i64()?;
                let close_reason = reader.i32()?;
                reader.done()?;
                let close_reason = enum_field!(CloseReason, "close reason", close_reason)?;
                term("leadership term id", leadership_term_id)?;
                Message::SessionCloseEvent {
                    leadership_term_id,
                    cluster_session_id,
                    timestamp,
                    close_reason,
                }
            }
            Template::ClusterAction => {
                let leadership_term_id = reader.i64()?;
                let log_position = reader.i64()?;
                let timestamp = reader.i64()?;
                let action = reader.i32()?;
                let flags = reader.i32()?;
                reader.done()?;
                let action = enum_field!(ClusterAction, "action", action)?;
                position("log position", log_position, false)?;
                term("leadership term id", leadership_term_id)?;
                Message::ClusterAction {
                    leadership_term_id,
                    log_position,
                    timestamp,
                    action,
                    flags,
                }
            }
            Template::NewLeadershipTermEvent => {
                let leadership_term_id = reader.i64()?;
                let log_position = reader.i64()?;
                let timestamp = reader.i64()?;
                let term_base_log_position = reader.i64()?;
                let leader_member_id = reader.i32()?;
                let log_session_id = reader.i32()?;
                let time_unit = reader.i32()?;
                let app_version = reader.i32()?;
                reader.done()?;
                let time_unit = enum_field!(TimeUnit, "time unit", time_unit)?;
                member_id("leader member id", leader_member_id, false)?;
                position("log position", log_position, false)?;
                position("term base log position", term_base_log_position, false)?;
                term("leadership term id", leadership_term_id)?;
                Message::NewLeadershipTermEvent {
                    leadership_term_id,
                    log_position,
                    timestamp,
                    term_base_log_position,
                    leader_member_id,
                    log_session_id,
                    time_unit,
                    app_version,
                }
            }
            Template::MembershipChangeEvent => {
                let leadership_term_id = reader.i64()?;
                let log_position = reader.i64()?;
                let timestamp = reader.i64()?;
                let leader_member_id = reader.i32()?;
                let cluster_size = reader.i32()?;
                let change_type = reader.i32()?;
                let member = reader.i32()?;
                let cluster_members = reader.variable()?;
                reader.done()?;
                let change_type = enum_field!(ChangeType, "change type", change_type)?;
                ascii("cluster members", cluster_members)?;
                member_id("leader member id", leader_member_id, false)?;
                member_id("member id", member, false)?;
                position("log position", log_position, false)?;
                term("leadership term id", leadership_term_id)?;
                Message::MembershipChangeEvent {
                    leadership_term_id,
                    log_position,
                    timestamp,
                    leader_member_id,
                    cluster_size,
                    change_type,
                    member_id: member,
                    cluster_members,
                }
            }
            Template::CanvassPosition => {
                let log_leadership_term_id = reader.i64()?;
                let log_position = reader.i64()?;
                let leadership_term_id = reader.i64()?;
                let follower_member_id = reader.i32()?;
                let protocol = reader.i32()?;
                reader.done()?;
                protocol_version(protocol)?;
                member_id("follower member id", follower_member_id, false)?;
                position("log position", log_position, false)?;
                term("log leadership term id", log_leadership_term_id)?;
                term("leadership term id", leadership_term_id)?;
                Message::CanvassPosition {
                    log_leadership_term_id,
                    log_position,
                    leadership_term_id,
                    follower_member_id,
                    protocol_version: protocol,
                }
            }
            Template::RequestVote => {
                let log_leadership_term_id = reader.i64()?;
                let log_position = reader.i64()?;
                let candidate_term_id = reader.i64()?;
                let candidate_member_id = reader.i32()?;
                let protocol = reader.i32()?;
                reader.done()?;
                protocol_version(protocol)?;
                member_id("candidate member id", candidate_member_id, false)?;
                position("log position", log_position, false)?;
                term("log leadership term id", log_leadership_term_id)?;
                term("candidate term id", candidate_term_id)?;
                Message::RequestVote {
                    log_leadership_term_id,
                    log_position,
                    candidate_term_id,
                    candidate_member_id,
                    protocol_version: protocol,
                }
            }
            Template::Vote => {
                let candidate_term_id = reader.i64()?;
                let log_leadership_term_id = reader.i64()?;
                let log_position = reader.i64()?;
                let candidate_member_id = reader.i32()?;
                let follower_member_id = reader.i32()?;
                let vote = reader.i32()?;
                reader.done()?;
                let vote = bool_field("vote", vote)?;
                member_id("candidate member id", candidate_member_id, false)?;
                member_id("follower member id", follower_member_id, false)?;
                position("log position", log_position, false)?;
                term("candidate term id", candidate_term_id)?;
                term("log leadership term id", log_leadership_term_id)?;
                Message::Vote {
                    candidate_term_id,
                    log_leadership_term_id,
                    log_position,
                    candidate_member_id,
                    follower_member_id,
                    vote,
                }
            }
            Template::NewLeadershipTerm => {
                let log_leadership_term_id = reader.i64()?;
                let next_leadership_term_id = reader.i64()?;
                let next_term_base_log_position = reader.i64()?;
                let next_log_position = reader.i64()?;
                let leadership_term_id = reader.i64()?;
                let term_base_log_position = reader.i64()?;
                let log_position = reader.i64()?;
                let leader_recording_id = reader.i64()?;
                let timestamp = reader.i64()?;
                let leader_member_id = reader.i32()?;
                let log_session_id = reader.i32()?;
                let app_version = reader.i32()?;
                let is_startup = reader.i32()?;
                let commit_position = reader.i64()?;
                reader.done()?;
                let is_startup = bool_field("is startup", is_startup)?;
                member_id("leader member id", leader_member_id, false)?;
                position(
                    "next term base log position",
                    next_term_base_log_position,
                    true,
                )?;
                position("next log position", next_log_position, true)?;
                position("term base log position", term_base_log_position, false)?;
                position("log position", log_position, false)?;
                position("commit position", commit_position, false)?;
                term("log leadership term id", log_leadership_term_id)?;
                term("next leadership term id", next_leadership_term_id)?;
                term("leadership term id", leadership_term_id)?;
                Message::NewLeadershipTerm {
                    log_leadership_term_id,
                    next_leadership_term_id,
                    next_term_base_log_position,
                    next_log_position,
                    leadership_term_id,
                    term_base_log_position,
                    log_position,
                    leader_recording_id,
                    timestamp,
                    leader_member_id,
                    log_session_id,
                    app_version,
                    is_startup,
                    commit_position,
                }
            }
            Template::AppendPosition => {
                let leadership_term_id = reader.i64()?;
                let log_position = reader.i64()?;
                let follower_member_id = reader.i32()?;
                let flags = reader.u8()?;
                reader.done()?;
                member_id("follower member id", follower_member_id, false)?;
                position("log position", log_position, false)?;
                term("leadership term id", leadership_term_id)?;
                Message::AppendPosition {
                    leadership_term_id,
                    log_position,
                    follower_member_id,
                    flags,
                }
            }
            Template::CommitPosition => {
                let leadership_term_id = reader.i64()?;
                let log_position = reader.i64()?;
                let leader_member_id = reader.i32()?;
                reader.done()?;
                member_id("leader member id", leader_member_id, false)?;
                position("log position", log_position, false)?;
                term("leadership term id", leadership_term_id)?;
                Message::CommitPosition {
                    leadership_term_id,
                    log_position,
                    leader_member_id,
                }
            }
            Template::CatchupPosition => {
                let leadership_term_id = reader.i64()?;
                let log_position = reader.i64()?;
                let follower_member_id = reader.i32()?;
                let catchup_endpoint = reader.variable()?;
                reader.done()?;
                ascii("catchup endpoint", catchup_endpoint)?;
                member_id("follower member id", follower_member_id, false)?;
                position("log position", log_position, false)?;
                term("leadership term id", leadership_term_id)?;
                Message::CatchupPosition {
                    leadership_term_id,
                    log_position,
                    follower_member_id,
                    catchup_endpoint,
                }
            }
            Template::StopCatchup => {
                let leadership_term_id = reader.i64()?;
                let follower_member_id = reader.i32()?;
                reader.done()?;
                member_id("follower member id", follower_member_id, false)?;
                term("leadership term id", leadership_term_id)?;
                Message::StopCatchup {
                    leadership_term_id,
                    follower_member_id,
                }
            }
            Template::SnapshotMarker => {
                let type_id = reader.i64()?;
                let log_position = reader.i64()?;
                let leadership_term_id = reader.i64()?;
                let index = reader.i32()?;
                let mark = reader.i32()?;
                let time_unit = reader.i32()?;
                let app_version = reader.i32()?;
                reader.done()?;
                let mark = enum_field!(SnapshotMark, "mark", mark)?;
                let time_unit = enum_field!(TimeUnit, "time unit", time_unit)?;
                position("log position", log_position, false)?;
                term("leadership term id", leadership_term_id)?;
                Message::SnapshotMarker {
                    type_id,
                    log_position,
                    leadership_term_id,
                    index,
                    mark,
                    time_unit,
                    app_version,
                }
            }
        };
        Ok(message)
    }

    /// The message's length in bytes: the header, the block and the variable fields or
    /// the payload.
    pub fn encoded_length(&self) -> usize {
        let block = HEADER_LENGTH + self.template().block_length() as usize;
        let variable: usize = match *self {
            Message::SessionMessage { payload, .. } => payload.len(),
            Message::SessionEvent { detail, .. } => 4 + detail.len(),
            Message::SessionConnect {
                response_channel,
                encoded_credentials,
                client_info,
                ..
            } => 12 + response_channel.len() + encoded_credentials.len() + client_info.len(),
            Message::NewLeaderEvent {
                ingress_endpoints, ..
            } => 4 + ingress_endpoints.len(),
            Message::SessionOpenEvent {
                response_channel,
                encoded_principal,
                ..
            } => 8 + response_channel.len() + encoded_principal.len(),
            Message::MembershipChangeEvent {
                cluster_members, ..
            } => 4 + cluster_members.len(),
            Message::CatchupPosition {
                catchup_endpoint, ..
            } => 4 + catchup_endpoint.len(),
            _ => 0,
        };
        block + variable
    }

    /// Writes the message at the start of `buffer` and returns its length, or
    /// [`ClusterError::TooShort`] when the buffer is shorter than
    /// [`Message::encoded_length`]; nothing else fails, since a message built in Rust
    /// holds values the spec allows except where its field is a plain integer the
    /// reader checks on the way back in.
    pub fn write(&self, buffer: &mut [u8]) -> Result<usize, ClusterError> {
        let needed = self.encoded_length();
        if buffer.len() < needed {
            return Err(ClusterError::TooShort {
                given: buffer.len(),
                needed,
            });
        }
        let template = self.template();
        let mut writer = Writer {
            buffer,
            at: 0,
            needed,
        };
        writer.put(&template.block_length().to_le_bytes())?;
        writer.put(&template.id().to_le_bytes())?;
        writer.put(&SCHEMA_ID.to_le_bytes())?;
        writer.put(&VERSION.to_le_bytes())?;
        match *self {
            Message::SessionMessage {
                leadership_term_id,
                cluster_session_id,
                timestamp,
                payload,
            } => {
                writer.i64(leadership_term_id)?;
                writer.i64(cluster_session_id)?;
                writer.i64(timestamp)?;
                writer.put(payload)?;
            }
            Message::SessionEvent {
                cluster_session_id,
                correlation_id,
                leadership_term_id,
                leader_member_id,
                code,
                version,
                leader_heartbeat_timeout_ns,
                detail,
            } => {
                writer.i64(cluster_session_id)?;
                writer.i64(correlation_id)?;
                writer.i64(leadership_term_id)?;
                writer.i32(leader_member_id)?;
                writer.i32(code.code())?;
                writer.i32(version)?;
                writer.i64(leader_heartbeat_timeout_ns)?;
                writer.variable(detail)?;
            }
            Message::SessionConnect {
                correlation_id,
                response_stream_id,
                version,
                response_channel,
                encoded_credentials,
                client_info,
            } => {
                writer.i64(correlation_id)?;
                writer.i32(response_stream_id)?;
                writer.i32(version)?;
                writer.variable(response_channel)?;
                writer.variable(encoded_credentials)?;
                writer.variable(client_info)?;
            }
            Message::SessionClose {
                leadership_term_id,
                cluster_session_id,
            }
            | Message::SessionKeepalive {
                leadership_term_id,
                cluster_session_id,
            } => {
                writer.i64(leadership_term_id)?;
                writer.i64(cluster_session_id)?;
            }
            Message::NewLeaderEvent {
                leadership_term_id,
                cluster_session_id,
                leader_member_id,
                ingress_endpoints,
            } => {
                writer.i64(leadership_term_id)?;
                writer.i64(cluster_session_id)?;
                writer.i32(leader_member_id)?;
                writer.variable(ingress_endpoints)?;
            }
            Message::TimerEvent {
                leadership_term_id,
                correlation_id,
                timestamp,
            } => {
                writer.i64(leadership_term_id)?;
                writer.i64(correlation_id)?;
                writer.i64(timestamp)?;
            }
            Message::SessionOpenEvent {
                leadership_term_id,
                correlation_id,
                cluster_session_id,
                timestamp,
                response_stream_id,
                response_channel,
                encoded_principal,
            } => {
                writer.i64(leadership_term_id)?;
                writer.i64(correlation_id)?;
                writer.i64(cluster_session_id)?;
                writer.i64(timestamp)?;
                writer.i32(response_stream_id)?;
                writer.variable(response_channel)?;
                writer.variable(encoded_principal)?;
            }
            Message::SessionCloseEvent {
                leadership_term_id,
                cluster_session_id,
                timestamp,
                close_reason,
            } => {
                writer.i64(leadership_term_id)?;
                writer.i64(cluster_session_id)?;
                writer.i64(timestamp)?;
                writer.i32(close_reason.code())?;
            }
            Message::ClusterAction {
                leadership_term_id,
                log_position,
                timestamp,
                action,
                flags,
            } => {
                writer.i64(leadership_term_id)?;
                writer.i64(log_position)?;
                writer.i64(timestamp)?;
                writer.i32(action.code())?;
                writer.i32(flags)?;
            }
            Message::NewLeadershipTermEvent {
                leadership_term_id,
                log_position,
                timestamp,
                term_base_log_position,
                leader_member_id,
                log_session_id,
                time_unit,
                app_version,
            } => {
                writer.i64(leadership_term_id)?;
                writer.i64(log_position)?;
                writer.i64(timestamp)?;
                writer.i64(term_base_log_position)?;
                writer.i32(leader_member_id)?;
                writer.i32(log_session_id)?;
                writer.i32(time_unit.code())?;
                writer.i32(app_version)?;
            }
            Message::MembershipChangeEvent {
                leadership_term_id,
                log_position,
                timestamp,
                leader_member_id,
                cluster_size,
                change_type,
                member_id,
                cluster_members,
            } => {
                writer.i64(leadership_term_id)?;
                writer.i64(log_position)?;
                writer.i64(timestamp)?;
                writer.i32(leader_member_id)?;
                writer.i32(cluster_size)?;
                writer.i32(change_type.code())?;
                writer.i32(member_id)?;
                writer.variable(cluster_members)?;
            }
            Message::CanvassPosition {
                log_leadership_term_id,
                log_position,
                leadership_term_id,
                follower_member_id,
                protocol_version,
            } => {
                writer.i64(log_leadership_term_id)?;
                writer.i64(log_position)?;
                writer.i64(leadership_term_id)?;
                writer.i32(follower_member_id)?;
                writer.i32(protocol_version)?;
            }
            Message::RequestVote {
                log_leadership_term_id,
                log_position,
                candidate_term_id,
                candidate_member_id,
                protocol_version,
            } => {
                writer.i64(log_leadership_term_id)?;
                writer.i64(log_position)?;
                writer.i64(candidate_term_id)?;
                writer.i32(candidate_member_id)?;
                writer.i32(protocol_version)?;
            }
            Message::Vote {
                candidate_term_id,
                log_leadership_term_id,
                log_position,
                candidate_member_id,
                follower_member_id,
                vote,
            } => {
                writer.i64(candidate_term_id)?;
                writer.i64(log_leadership_term_id)?;
                writer.i64(log_position)?;
                writer.i32(candidate_member_id)?;
                writer.i32(follower_member_id)?;
                writer.i32(i32::from(vote))?;
            }
            Message::NewLeadershipTerm {
                log_leadership_term_id,
                next_leadership_term_id,
                next_term_base_log_position,
                next_log_position,
                leadership_term_id,
                term_base_log_position,
                log_position,
                leader_recording_id,
                timestamp,
                leader_member_id,
                log_session_id,
                app_version,
                is_startup,
                commit_position,
            } => {
                writer.i64(log_leadership_term_id)?;
                writer.i64(next_leadership_term_id)?;
                writer.i64(next_term_base_log_position)?;
                writer.i64(next_log_position)?;
                writer.i64(leadership_term_id)?;
                writer.i64(term_base_log_position)?;
                writer.i64(log_position)?;
                writer.i64(leader_recording_id)?;
                writer.i64(timestamp)?;
                writer.i32(leader_member_id)?;
                writer.i32(log_session_id)?;
                writer.i32(app_version)?;
                writer.i32(i32::from(is_startup))?;
                writer.i64(commit_position)?;
            }
            Message::AppendPosition {
                leadership_term_id,
                log_position,
                follower_member_id,
                flags,
            } => {
                writer.i64(leadership_term_id)?;
                writer.i64(log_position)?;
                writer.i32(follower_member_id)?;
                writer.u8(flags)?;
            }
            Message::CommitPosition {
                leadership_term_id,
                log_position,
                leader_member_id,
            } => {
                writer.i64(leadership_term_id)?;
                writer.i64(log_position)?;
                writer.i32(leader_member_id)?;
            }
            Message::CatchupPosition {
                leadership_term_id,
                log_position,
                follower_member_id,
                catchup_endpoint,
            } => {
                writer.i64(leadership_term_id)?;
                writer.i64(log_position)?;
                writer.i32(follower_member_id)?;
                writer.variable(catchup_endpoint)?;
            }
            Message::StopCatchup {
                leadership_term_id,
                follower_member_id,
            } => {
                writer.i64(leadership_term_id)?;
                writer.i32(follower_member_id)?;
            }
            Message::SnapshotMarker {
                type_id,
                log_position,
                leadership_term_id,
                index,
                mark,
                time_unit,
                app_version,
            } => {
                writer.i64(type_id)?;
                writer.i64(log_position)?;
                writer.i64(leadership_term_id)?;
                writer.i32(index)?;
                writer.i32(mark.code())?;
                writer.i32(time_unit.code())?;
                writer.i32(app_version)?;
            }
        }
        Ok(writer.at)
    }
}

// ---- the members string ---------------------------------------------------------------

/// Why a members string was refused: the checks of `spec/cluster.md`, "What a reader
/// rejects", for a members string, and [`MembersError::reason`] their names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MembersError {
    /// An empty string, a member with other than six fields, an id that is not an
    /// integer from 0 to 2147483647, or an endpoint without a host or a port from 1 to
    /// 65535.
    Grammar {
        /// Which member, counting from 0.
        member: usize,
    },
    /// Two members with one id.
    Duplicate {
        /// The id.
        id: i32,
    },
    /// More than [`MAX_MEMBERS`] members.
    Count {
        /// How many.
        found: usize,
    },
    /// The member's own id is not in the string.
    SelfNotFound {
        /// The id looked for.
        id: i32,
    },
}

impl MembersError {
    /// The reason's name in the spec and the vectors.
    pub const fn reason(self) -> &'static str {
        match self {
            MembersError::Grammar { .. } => "members-grammar",
            MembersError::Duplicate { .. } => "members-duplicate",
            MembersError::Count { .. } => "members-count",
            MembersError::SelfNotFound { .. } => "members-self",
        }
    }
}

impl fmt::Display for MembersError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            MembersError::Grammar { member } => write!(
                f,
                "members string rejected (members-grammar): member {member} is not \
                 `id,ingress,consensus,log,catchup,archive` with an id from 0 upward and \
                 every endpoint `host:port`; fix the string; see {DOC}"
            ),
            MembersError::Duplicate { id } => write!(
                f,
                "members string rejected (members-duplicate): two members have the id {id}; \
                 give every member its own; see {DOC}"
            ),
            MembersError::Count { found } => write!(
                f,
                "members string rejected (members-count): {found} members, at most \
                 {MAX_MEMBERS} allowed; see {DOC}"
            ),
            MembersError::SelfNotFound { id } => write!(
                f,
                "members string rejected (members-self): this member's id {id} is not in the \
                 string; add it or run with an id the string names; see {DOC}"
            ),
        }
    }
}

impl core::error::Error for MembersError {}

/// An endpoint of the members string, `host:port`, the host as written without an IPv6
/// literal's brackets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Endpoint<'a> {
    /// The host name or address literal.
    pub host: &'a [u8],
    /// The port, 1 to 65535.
    pub port: u16,
}

/// One member of the members string.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Member<'a> {
    /// The member id.
    pub id: i32,
    /// Where it receives clients.
    pub ingress: Endpoint<'a>,
    /// Where it receives consensus messages.
    pub consensus: Endpoint<'a>,
    /// Where it receives the log.
    pub log: Endpoint<'a>,
    /// Where it receives a catch-up replay.
    pub catchup: Endpoint<'a>,
    /// Where a peer asks it to replicate its recording.
    pub archive: Endpoint<'a>,
}

/// The integer that `text`'s ASCII digits make, any number of them, leading zeros
/// included, as the members string's grammar reads an id or a port; `None` when `text`
/// is empty or holds another byte. The value saturates at `u64::MAX`, so a run of
/// digits too large for its field fails the caller's range check, never overflows.
fn digits(text: &[u8]) -> Option<u64> {
    if text.is_empty() {
        return None;
    }
    let mut value: u64 = 0;
    for &digit in text {
        if !digit.is_ascii_digit() {
            return None;
        }
        value = value
            .saturating_mul(10)
            .saturating_add(u64::from(digit - b'0'));
    }
    Some(value)
}

fn parse_endpoint(text: &[u8]) -> Option<Endpoint<'_>> {
    let colon = text.iter().rposition(|&byte| byte == b':')?;
    let (host, port) = (text.get(..colon)?, text.get(colon + 1..)?);
    if host.is_empty() {
        return None;
    }
    let port = u16::try_from(digits(port)?)
        .ok()
        .filter(|&port| port != 0)?;
    let host = match (host.first(), host.last()) {
        (Some(b'['), Some(b']')) if host.len() > 2 => host.get(1..host.len() - 1)?,
        _ if host.contains(&b':') => return None, // an IPv6 literal is written in brackets
        _ => host,
    };
    Some(Endpoint { host, port })
}

fn parse_member(text: &[u8]) -> Option<Member<'_>> {
    let mut fields = text.split(|&byte| byte == b',');
    let id = i32::try_from(digits(fields.next()?)?).ok()?;
    let ingress = parse_endpoint(fields.next()?)?;
    let consensus = parse_endpoint(fields.next()?)?;
    let log = parse_endpoint(fields.next()?)?;
    let catchup = parse_endpoint(fields.next()?)?;
    let archive = parse_endpoint(fields.next()?)?;
    if fields.next().is_some() {
        return None;
    }
    Some(Member {
        id,
        ingress,
        consensus,
        log,
        catchup,
        archive,
    })
}

/// Parses a members string into `members`, returning how many it holds, with the
/// spec's checks in the spec's order: the grammar of every member, duplicate ids, the
/// count, and `self_id` named in the string when one is given. Allocates nothing: a
/// string of more than [`MAX_MEMBERS`] members is refused before any is stored.
pub fn parse_members<'a>(
    text: &'a [u8],
    members: &mut [Member<'a>; MAX_MEMBERS],
    self_id: Option<i32>,
) -> Result<usize, MembersError> {
    if text.is_empty() {
        return Err(MembersError::Grammar { member: 0 });
    }
    let items = || text.split(|&byte| byte == b'|');
    for (index, item) in items().enumerate() {
        if parse_member(item).is_none() {
            return Err(MembersError::Grammar { member: index });
        }
    }
    for (index, item) in items().enumerate() {
        let id = parse_member(item).map(|m| m.id).unwrap_or(-1);
        if items()
            .skip(index + 1)
            .any(|other| parse_member(other).map(|m| m.id) == Some(id))
        {
            return Err(MembersError::Duplicate { id });
        }
    }
    let count = items().count();
    if count > MAX_MEMBERS {
        return Err(MembersError::Count { found: count });
    }
    if let Some(id) = self_id
        && !items().any(|item| parse_member(item).map(|m| m.id) == Some(id))
    {
        return Err(MembersError::SelfNotFound { id });
    }
    for (slot, item) in members.iter_mut().zip(items()) {
        if let Some(member) = parse_member(item) {
            *slot = member;
        }
    }
    Ok(count)
}

/// A member with nothing in it, for an array [`parse_members`] fills.
pub const EMPTY_MEMBER: Member<'static> = Member {
    id: -1,
    ingress: EMPTY_ENDPOINT,
    consensus: EMPTY_ENDPOINT,
    log: EMPTY_ENDPOINT,
    catchup: EMPTY_ENDPOINT,
    archive: EMPTY_ENDPOINT,
};

const EMPTY_ENDPOINT: Endpoint<'static> = Endpoint { host: b"", port: 0 };

// ---- arithmetic -------------------------------------------------------------------------

/// The quorum of a cluster of `members`: half plus one (`spec/cluster.md`, "Roles and
/// the election").
pub const fn quorum(members: usize) -> usize {
    members / 2 + 1
}

/// One member's last append position, the term it was reported for and how long ago
/// it arrived, for [`commit_position`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MemberPosition {
    /// The append position.
    pub position: i64,
    /// How long ago it arrived, in nanoseconds; 0 for the leader's own.
    pub age_ns: u64,
    /// The leadership term id the report carried, the leader's own for its position.
    pub term: i64,
}

/// The quorum position and the commit position.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CommitPosition {
    /// The quorum-th highest append position among the active members, 0 with fewer
    /// than a quorum active.
    pub quorum_position: i64,
    /// The lesser of the quorum position and the leader's own append position.
    pub commit_position: i64,
}

/// The commit position from the members' append positions, the leader's own among
/// them, and the leader's append position, term and term base log position
/// (`spec/cluster.md`, "The commit position"): a member counts when it is active, its
/// position younger than `timeout_ns`, its last report was for `leader_term` and its
/// position is at or past `term_base`; Raft's rule, not Aeron's. At most
/// [`MAX_MEMBERS`] positions are ranked; any beyond are ignored.
pub fn commit_position(
    positions: &[MemberPosition],
    leader_position: i64,
    leader_term: i64,
    term_base: i64,
    timeout_ns: u64,
) -> CommitPosition {
    let count = positions.len().min(MAX_MEMBERS);
    let mut ranked = [0i64; MAX_MEMBERS];
    let mut active = 0usize;
    for member in positions.iter().take(count) {
        if member.age_ns >= timeout_ns || member.term != leader_term || member.position < term_base
        {
            continue;
        }
        active += 1;
        // Insert into the descending ranking, pushing the smaller down.
        let mut new = member.position;
        for slot in ranked.iter_mut().take(count) {
            if new > *slot {
                core::mem::swap(&mut new, slot);
            }
        }
    }
    let q = quorum(count);
    let quorum_position = if active >= q && q >= 1 {
        ranked.get(q - 1).copied().unwrap_or(0)
    } else {
        0
    };
    CommitPosition {
        quorum_position,
        commit_position: quorum_position.min(leader_position),
    }
}

/// Whether the log (`term`, `position`) is at least the log (`other_term`,
/// `other_position`): the term decides, then the position (`spec/cluster.md`, "Logs
/// compared").
pub const fn log_at_least(term: i64, position: i64, other_term: i64, other_position: i64) -> bool {
    if term != other_term {
        term > other_term
    } else {
        position >= other_position
    }
}

/// A ballot's outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Ballot {
    /// Whether the candidate leads the term.
    pub leader: bool,
    /// Whether every member voted for it, which decides at once.
    pub unanimous: bool,
}

/// One vote a candidate recorded against a member.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Vote {
    /// For the candidate, or against.
    pub vote: bool,
    /// The candidate term the vote answers.
    pub term: i64,
}

/// A candidate's ballot for `candidate_term` among `members` members, `votes[i]` the
/// vote of member `i`, `None` for one not yet cast (`spec/cluster.md`, "candidate
/// ballot"): over the votes for the candidate's current candidate term alone, its own
/// among them, leader at once when every member voted for, else leader once the
/// election timeout has passed when a quorum voted for and none against; a vote for an
/// older candidate term of the candidate's counts for nothing.
pub fn ballot(
    members: usize,
    candidate_term: i64,
    votes: &[Option<Vote>],
    timeout_passed: bool,
) -> Ballot {
    let cast = votes
        .iter()
        .take(members)
        .map(|v| v.filter(|v| v.term == candidate_term));
    let for_ = cast.clone().filter(|v| v.is_some_and(|v| v.vote)).count();
    let against = cast.clone().filter(|v| v.is_some_and(|v| !v.vote)).count();
    let counted = cast.count();
    if counted == members && for_ == members && members >= 1 {
        return Ballot {
            leader: true,
            unanimous: true,
        };
    }
    Ballot {
        leader: timeout_passed && against == 0 && for_ >= quorum(members),
        unanimous: false,
    }
}

/// One member's canvass report: its log leadership term id and append position.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LogReport {
    /// Its log leadership term id: the highest term in its record of recorded terms
    /// (`spec/cluster.md`, "The log"), which is not the term of its last entry; −1 for
    /// none.
    pub term: i64,
    /// Its append position.
    pub position: i64,
}

/// Whether a canvassing member nominates itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Candidate {
    /// Whether it goes to nominate.
    pub nominate: bool,
    /// Whether every member reported and none is ahead, which decides at once.
    pub unanimous: bool,
}

/// A canvassing member's nomination among `members` members, `reports[i]` member `i`'s
/// canvass report or `None` while it has not reported, `own` the member's own log
/// (`spec/cluster.md`, "canvass"): a unanimous candidate at once when every member has
/// reported and none has a log ahead of `own`; a quorum candidate once the deadline has
/// passed when those that reported with a log not ahead make a quorum. The member
/// counts itself through its own entry in `reports`.
pub fn candidate(
    members: usize,
    reports: &[Option<LogReport>],
    own: LogReport,
    deadline_passed: bool,
) -> Candidate {
    let reported = reports.iter().take(members).flatten();
    let reported_count = reported.clone().count();
    let not_ahead = reported
        .filter(|r| log_at_least(own.term, own.position, r.term, r.position))
        .count();
    if reported_count == members && not_ahead == members && members >= 1 {
        return Candidate {
            nominate: true,
            unanimous: true,
        };
    }
    Candidate {
        nominate: deadline_passed && not_ahead >= quorum(members),
        unanimous: false,
    }
}

/// The fields of a request vote a reader answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VoteRequest {
    /// The term the candidate proposes to lead.
    pub candidate_term_id: i64,
    /// The candidate's log leadership term id, the highest term in its record.
    pub log_leadership_term_id: i64,
    /// The candidate's append position.
    pub log_position: i64,
}

/// A reader's answer to a request vote.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VoteAnswer {
    /// `Some(true)` for the candidate, `Some(false)` against, `None` for no answer.
    pub vote: Option<bool>,
    /// The reader's candidate term after the request, stored before the vote is sent
    /// when it changed.
    pub candidate_term: i64,
}

/// The election states, numbered as `spec/cluster.md`'s table, in which a member votes
/// for a candidate: canvass, nominate, candidate ballot and follower ballot.
pub const VOTING_STATES: [u8; 4] = [1, 2, 3, 4];

/// What a reader with `candidate_term`, log `log` and election state `state`, numbered
/// as `spec/cluster.md`'s table, answers `request` (`spec/cluster.md`, "Messages during
/// an election", request vote): against with the term unchanged for a term at or below
/// its own, so that it votes once in a term; against, taking the term, for a higher
/// term from a log behind its own; for, taking the term, for a higher term from a log
/// at least its own in one of [`VOTING_STATES`]; nothing in init, once the election has
/// closed ([`CLOSED_STATE`]), when the member holds an election instead, or in any
/// other state.
pub const fn request_vote(
    candidate_term: i64,
    log: LogReport,
    state: u8,
    request: VoteRequest,
) -> VoteAnswer {
    let term = request.candidate_term_id;
    if state == 0 || state == CLOSED_STATE {
        return VoteAnswer {
            vote: None,
            candidate_term,
        };
    }
    if term <= candidate_term {
        return VoteAnswer {
            vote: Some(false),
            candidate_term,
        };
    }
    if !log_at_least(
        request.log_leadership_term_id,
        request.log_position,
        log.term,
        log.position,
    ) {
        return VoteAnswer {
            vote: Some(false),
            candidate_term: term,
        };
    }
    if matches!(state, 1..=4) {
        return VoteAnswer {
            vote: Some(true),
            candidate_term: term,
        };
    }
    VoteAnswer {
        vote: None,
        candidate_term,
    }
}

/// The election state "closed", numbered as `spec/cluster.md`'s table: no election runs.
pub const CLOSED_STATE: u8 = 17;

/// The two terms of a canvass position a leader reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CanvassTerms {
    /// The sender's candidate term.
    pub leadership_term_id: i64,
    /// The highest term in the sender's record.
    pub log_leadership_term_id: i64,
}

/// What a leader does with a canvass position.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CanvassAction {
    /// Answer with new leadership term for the reported log leadership term id.
    Answer,
    /// Hold an election from init: Raft's step-down once closed, a failed election in
    /// leader log replication or leader ready.
    Election,
    /// Update the sender's position in the table alone.
    Table,
}

impl CanvassAction {
    /// The action's name as the vectors spell it.
    pub const fn name(self) -> &'static str {
        match self {
            CanvassAction::Answer => "answer",
            CanvassAction::Election => "election",
            CanvassAction::Table => "table",
        }
    }
}

/// What a leader of `leader_term` in election state `state`, 5 to 8 or
/// [`CLOSED_STATE`], does with a canvass position whose terms are `message`
/// (`spec/cluster.md`, "Messages during an election" and "Messages outside an
/// election"): an election when either term is above its own, in leader log
/// replication, leader ready or closed, Raft's step-down; an answer when the log
/// leadership term id is below its term, or at it once closed; else the table alone.
pub const fn canvass_position_action(
    leader_term: i64,
    state: u8,
    message: CanvassTerms,
) -> CanvassAction {
    if message.leadership_term_id > leader_term || message.log_leadership_term_id > leader_term {
        return if matches!(state, 5 | 8 | CLOSED_STATE) {
            CanvassAction::Election
        } else {
            CanvassAction::Table
        };
    }
    if message.log_leadership_term_id < leader_term || state == CLOSED_STATE {
        return CanvassAction::Answer;
    }
    CanvassAction::Table
}

/// Where a follower's log replication stops.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ReplicationStop {
    /// Whether the follower replicates at all: false when the new leadership term gives
    /// it no stop position.
    pub replicate: bool,
    /// The stop position, or −1 for none.
    pub stop_position: i64,
}

/// The stop position of a follower whose append position is below the leader's term
/// base log position, from the new leadership term's next term base log position and
/// next log position, each −1 for none (`spec/cluster.md`, "follower log replication"):
/// the next term base log position when the append position is below it, else the
/// next log position when the message carries one.
pub const fn replication_stop(
    append_position: i64,
    next_term_base_log_position: i64,
    next_log_position: i64,
) -> ReplicationStop {
    if next_term_base_log_position != NULL_VALUE && append_position < next_term_base_log_position {
        ReplicationStop {
            replicate: true,
            stop_position: next_term_base_log_position,
        }
    } else if next_term_base_log_position != NULL_VALUE
        && append_position == next_term_base_log_position
        && next_log_position != NULL_VALUE
    {
        ReplicationStop {
            replicate: true,
            stop_position: next_log_position,
        }
    } else {
        ReplicationStop {
            replicate: false,
            stop_position: NULL_VALUE,
        }
    }
}

/// What a reader of new leadership term does with its log (`spec/cluster.md`,
/// "Messages during an election").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TermAction {
    /// A term below the reader's candidate term, or in a ballot state any term but the
    /// candidate term: the message is ignored.
    Ignore,
    /// The message's log leadership term id is not the reader's: back to canvass.
    Canvass,
    /// Below the leader's term base with a stop position: follower log replication.
    Replicate,
    /// At or above the leader's term base: follower replay.
    Replay,
    /// At the next term's base with no end carried: wait for a later message.
    Wait,
    /// Below the leader's term base with no next term carried: the election fails.
    Fail,
}

impl TermAction {
    /// The action's name as the vectors spell it.
    pub const fn name(self) -> &'static str {
        match self {
            TermAction::Ignore => "ignore",
            TermAction::Canvass => "canvass",
            TermAction::Replicate => "replicate",
            TermAction::Replay => "replay",
            TermAction::Wait => "wait",
            TermAction::Fail => "fail",
        }
    }
}

/// A reader's answer to new leadership term.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NewLeadershipTermAction {
    /// What the reader does next.
    pub action: TermAction,
    /// The position it truncates its log to first, or −1 for no truncation.
    pub truncate_to: i64,
    /// The stop position of a replication, or −1.
    pub stop_position: i64,
    /// The term and base the reader records before it reports: the leader's term at
    /// its base when the reader's append position, after any truncation, is at that
    /// base; `None` otherwise.
    pub records: Option<(i64, i64)>,
}

/// The fields of new leadership term a reader decides by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TermFields {
    /// The leader's term.
    pub leadership_term_id: i64,
    /// The term the message describes, the reader's reported log leadership term id.
    pub log_leadership_term_id: i64,
    /// The term after it in the leader's log, or −1.
    pub next_leadership_term_id: i64,
    /// That term's base position, or −1.
    pub next_term_base_log_position: i64,
    /// That term's end position, or −1.
    pub next_log_position: i64,
    /// The position at which the leader's term began.
    pub term_base_log_position: i64,
}

/// What a reader with `log_leadership_term_id`, `append_position` and `candidate_term`,
/// in canvass when `in_canvass` and in a ballot state otherwise, does with a new
/// leadership term whose fields are `message` (`spec/cluster.md`, "Messages during an
/// election"): ignore a term below the candidate term, or in a ballot state any term but
/// the candidate term, so that a member that voted in a term follows no leader of a
/// lower one; canvass when the log terms differ; else truncate to a next term base log
/// position the message carries when above it; then, below the leader's term base, fail
/// with no next term, replicate to the stop position, or wait; at or above it, replay.
pub const fn new_leadership_term_action(
    log_leadership_term_id: i64,
    append_position: i64,
    candidate_term: i64,
    in_canvass: bool,
    message: TermFields,
) -> NewLeadershipTermAction {
    let term = message.leadership_term_id;
    if term < candidate_term || (!in_canvass && term != candidate_term) {
        return NewLeadershipTermAction {
            action: TermAction::Ignore,
            truncate_to: NULL_VALUE,
            stop_position: NULL_VALUE,
            records: None,
        };
    }
    if message.log_leadership_term_id != log_leadership_term_id {
        return NewLeadershipTermAction {
            action: TermAction::Canvass,
            truncate_to: NULL_VALUE,
            stop_position: NULL_VALUE,
            records: None,
        };
    }
    let next_base = message.next_term_base_log_position;
    let (truncate_to, append) = if next_base != NULL_VALUE && append_position > next_base {
        (next_base, next_base)
    } else {
        (NULL_VALUE, append_position)
    };
    if append < message.term_base_log_position {
        if message.next_leadership_term_id == NULL_VALUE {
            return NewLeadershipTermAction {
                action: TermAction::Fail,
                truncate_to,
                stop_position: NULL_VALUE,
                records: None,
            };
        }
        let stop = replication_stop(append, next_base, message.next_log_position);
        if stop.replicate {
            return NewLeadershipTermAction {
                action: TermAction::Replicate,
                truncate_to,
                stop_position: stop.stop_position,
                records: None,
            };
        }
        return NewLeadershipTermAction {
            action: TermAction::Wait,
            truncate_to,
            stop_position: NULL_VALUE,
            records: None,
        };
    }
    NewLeadershipTermAction {
        action: TermAction::Replay,
        truncate_to,
        stop_position: NULL_VALUE,
        records: if append == message.term_base_log_position {
            Some((message.leadership_term_id, message.term_base_log_position))
        } else {
            None
        },
    }
}

/// The next fields of a new leadership term.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NextFields {
    /// The next leadership term id, or −1.
    pub next_leadership_term_id: i64,
    /// The next term base log position, or −1.
    pub next_term_base_log_position: i64,
    /// The next log position, or −1.
    pub next_log_position: i64,
}

/// The next fields a leader with `terms`, its record, of `leader_term` based at
/// `leader_base` writes into new leadership term for `reported_term`
/// (`spec/cluster.md`, "New leadership term, template 53"): −1 for all three when the
/// reported term is the leader's own; else the lowest term in the record above the
/// reported one, with its base and end, so that a reader holding an uncommitted tail
/// of a term the record skips truncates it to the next recorded term's base; else, for
/// a record without the leader's own term, that term at its base with no end.
pub fn next_fields(
    terms: &[TermSpan],
    reported_term: i64,
    leader_term: i64,
    leader_base: i64,
) -> NextFields {
    if reported_term == leader_term {
        return NextFields {
            next_leadership_term_id: NULL_VALUE,
            next_term_base_log_position: NULL_VALUE,
            next_log_position: NULL_VALUE,
        };
    }
    match terms
        .iter()
        .filter(|t| t.term > reported_term)
        .min_by_key(|t| t.term)
    {
        Some(entry) => NextFields {
            next_leadership_term_id: entry.term,
            next_term_base_log_position: entry.base,
            next_log_position: entry.end,
        },
        None => NextFields {
            next_leadership_term_id: leader_term,
            next_term_base_log_position: leader_base,
            next_log_position: NULL_VALUE,
        },
    }
}

/// One term of a leader's log, for [`replay_end`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TermSpan {
    /// The leadership term id.
    pub term: i64,
    /// The term base log position.
    pub base: i64,
    /// The position the term ends at, the next term's base, or −1 for an open term.
    pub end: i64,
}

/// Where the leader's replay for a position begins and ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ReplayEnd {
    /// The term whose recording the replay begins in, or −1 when none holds the position.
    pub term: i64,
    /// The first term base log position above the position, or −1 when the position
    /// lies in the leader's open term, a catch-up that follows the live recording.
    pub end_position: i64,
}

/// The leader's replay for `position` over its `terms` (`spec/cluster.md`, "Messages
/// outside an election"): it begins in the recording of the term that begins at or
/// below the position and ends above it, and a replication's replays end at the first
/// term base log position above the position.
pub fn replay_end(terms: &[TermSpan], position: i64) -> ReplayEnd {
    let holder = terms
        .iter()
        .find(|t| t.base <= position && (t.end == NULL_VALUE || position < t.end));
    let Some(holder) = holder else {
        return ReplayEnd {
            term: NULL_VALUE,
            end_position: NULL_VALUE,
        };
    };
    let end_position = terms
        .iter()
        .filter(|t| t.base > position)
        .map(|t| t.base)
        .min()
        .unwrap_or(NULL_VALUE);
    ReplayEnd {
        term: holder.term,
        end_position,
    }
}

/// The terms a follower records when its replication reaches its stop position.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RecordedTerms {
    /// The term replicated and its base, always recorded.
    pub replicated: (i64, i64),
    /// The leader's term and its base, recorded after it when the stop position is
    /// the leader's term base log position; `None` otherwise.
    pub leader: Option<(i64, i64)>,
    /// The member's log leadership term id afterwards, the highest term recorded.
    pub log_leadership_term_id: i64,
}

/// What a follower records when its replication of `term_replicated`, based at
/// `term_base`, reaches `stop_position` under a leader of `leader_term` based at
/// `leader_base` (`spec/cluster.md`, "follower log replication"): the term replicated,
/// and the leader's term at the leader's base after it when the stop is that base.
pub const fn replication_record(
    term_replicated: i64,
    term_base: i64,
    stop_position: i64,
    leader_term: i64,
    leader_base: i64,
) -> RecordedTerms {
    if stop_position == leader_base {
        RecordedTerms {
            replicated: (term_replicated, term_base),
            leader: Some((leader_term, leader_base)),
            log_leadership_term_id: if leader_term > term_replicated {
                leader_term
            } else {
                term_replicated
            },
        }
    } else {
        RecordedTerms {
            replicated: (term_replicated, term_base),
            leader: None,
            log_leadership_term_id: term_replicated,
        }
    }
}

/// The position after an entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct EntryPosition {
    /// How many frames carry it.
    pub fragments: usize,
    /// The position after its last frame.
    pub position_after: i64,
}

/// The position after an entry of `length` bytes that begins at `position` on a stream
/// with `mtu` (`spec/cluster.md`, "The log"): fragments of `mtu − 32` and a last of the
/// remainder, each frame's length rounded up to a multiple of 32. `None` when `mtu` is
/// 32 or less, which leaves a fragment no payload, or when a frame's length or the
/// position after the entry overflows; every step is checked, so no input panics.
pub const fn entry_position(position: i64, length: usize, mtu: usize) -> Option<EntryPosition> {
    if mtu <= 32 {
        return None;
    }
    let payload = mtu - 32;
    let fragments = if length == 0 {
        1
    } else {
        length.div_ceil(payload)
    };
    // The last fragment carries what the others leave: 0 bytes for an empty entry, else
    // the remainder, or a whole payload when the length is a multiple of it.
    let last_payload = match length % payload {
        0 if length > 0 => payload,
        remainder => remainder,
    };
    let (Some(full), Some(last)) = (aligned_frame(payload), aligned_frame(last_payload)) else {
        return None;
    };
    // `fragments` is at least 1, so `fragments - 1` cannot underflow.
    let Some(bytes) = full.checked_mul(fragments - 1) else {
        return None;
    };
    let Some(bytes) = bytes.checked_add(last) else {
        return None;
    };
    // A `usize` widens to a `u64` without loss; one above `i64::MAX` is no position.
    let bytes = bytes as u64;
    if bytes > i64::MAX as u64 {
        return None;
    }
    let Some(position_after) = position.checked_add(bytes as i64) else {
        return None;
    };
    Some(EntryPosition {
        fragments,
        position_after,
    })
}

/// A frame's length on the stream: the 32-byte header and `payload`, rounded up to a
/// multiple of 32; `None` when that does not fit a `usize`.
const fn aligned_frame(payload: usize) -> Option<usize> {
    match payload.checked_add(32 + 31) {
        Some(length) => Some(length / 32 * 32),
        None => None,
    }
}
