//! A client's session with a cluster (`spec/cluster.md`, "The session protocol" and
//! "Sessions"; ADR 0117). In Aeron, this is `AeronCluster` (`docs/glossary.md`).
//!
//! [`Cavalry::connect_cluster`] opens the session: a subscription on the client's
//! response channel, a publication to every member's `ingress` endpoint, and a session
//! connect sent to one member after another, a redirect followed, until the leader
//! answers ok. From then on [`ClusterSession::offer`] sends a message to the leader
//! and [`ClusterSession::poll`] reads what the cluster sends back, keeps the session
//! alive, and follows the leader when the cluster elects another.

use std::fmt;
use std::time::Duration;

use cavalry_proto::cluster::{
    EGRESS_STREAM_ID, EMPTY_MEMBER, Endpoint, EventCode, INGRESS_STREAM_ID, KEEPALIVE_INTERVAL_NS,
    LEADER_HEARTBEAT_TIMEOUT_NS, MAX_MEMBERS, MembersError, Message, PROTOCOL_VERSION,
    SESSION_CONNECT_TIMEOUT_NS, Stream, parse_members,
};
use cavalry_proto::frame::{DataFrameHeader, Flags};

use crate::client::Cavalry;
use crate::conductor::nano_time;
use crate::error::ClientError;
use crate::publication::{Offer, Publication};
use crate::subscription::{Fault, Subscription};

/// Where every message of this module is explained.
const DOC: &str = "docs/reference/cluster.md#sessions";

/// How long [`Cavalry::connect_cluster`] goes on trying the members before it gives
/// up: the leader heartbeat timeout, which is how long a cluster that has lost its
/// leader takes to notice, and the session connect timeout twice, for the connect
/// that was lost with that leader and the one the next leader answers.
const CONNECT_GIVE_UP_NS: u64 = LEADER_HEARTBEAT_TIMEOUT_NS + 2 * SESSION_CONNECT_TIMEOUT_NS;

/// How long a connect waits before it turns to the next member when the member it
/// asked knows no leader: an election is running.
const NO_LEADER_PAUSE: Duration = Duration::from_millis(100);

/// How many fragments of the response channel one step of a connect reads.
const CONNECT_POLL_LIMIT: usize = 16;

/// The longest detail of a session event that closes a session which the session
/// keeps for [`ClusterSession::closed`]: `TIMEOUT` is 7 bytes.
const CLOSED_DETAIL_LENGTH: usize = 32;

/// Why a session with a cluster could not be opened or closed.
#[derive(Debug)]
pub enum ClusterError {
    /// The members string failed a check of `spec/cluster.md`, "What a reader
    /// rejects".
    Members(MembersError),
    /// The client's own call failed: the driver refused a channel, or the client is
    /// closed.
    Client(ClientError),
    /// The leader refused the connect, with its reason as the session event's detail:
    /// `invalid client version`, `invalid response channel` or `concurrent session
    /// limit`.
    Refused {
        /// The leader's reason.
        detail: String,
    },
    /// The cluster speaks another major version of the cluster protocol.
    Version {
        /// The cluster's protocol version, in semantic form.
        version: i32,
    },
    /// No member opened a session within the time [`Cavalry::connect_cluster`] tries.
    ConnectTimeout {
        /// How many session connects were sent.
        connects: u32,
    },
    /// The read of the response channel stopped on a fault.
    Egress(Fault),
}

impl ClusterError {
    /// The reason's name: `members`, `client`, `refused`, `version`,
    /// `connect-timeout` or `egress`.
    pub const fn reason(&self) -> &'static str {
        match self {
            ClusterError::Members(_) => "members",
            ClusterError::Client(_) => "client",
            ClusterError::Refused { .. } => "refused",
            ClusterError::Version { .. } => "version",
            ClusterError::ConnectTimeout { .. } => "connect-timeout",
            ClusterError::Egress(_) => "egress",
        }
    }
}

impl fmt::Display for ClusterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let reason = self.reason();
        match self {
            ClusterError::Members(error) => write!(
                f,
                "cluster session refused ({reason}): {error}; pass the members string the \
                 cluster's members were started with; see {DOC}"
            ),
            ClusterError::Client(error) => write!(f, "{error}"),
            ClusterError::Refused { detail } => write!(
                f,
                "cluster session refused ({reason}): the leader answered `{detail}`; for \
                 `concurrent session limit` close a session that is done or connect again \
                 later, for `invalid response channel` pass \
                 `cavalry:udp?endpoint=<host:port>`, and for `invalid client version` use a \
                 client of the cluster's version; see {DOC}"
            ),
            ClusterError::Version { version } => write!(
                f,
                "cluster session refused ({reason}): the cluster speaks protocol version \
                 {}.{}.{} and this client speaks major {}; use a client of the cluster's \
                 version; see {DOC}",
                version >> 16,
                (version >> 8) & 0xff,
                version & 0xff,
                PROTOCOL_VERSION >> 16
            ),
            ClusterError::ConnectTimeout { connects } => write!(
                f,
                "cluster session refused ({reason}): no member opened a session in {} s, \
                 after {connects} session connects; check that the members run, that each \
                 member's ingress endpoint in the members string is reachable from this \
                 host, and that the members can reach this client's response channel; see \
                 {DOC}",
                CONNECT_GIVE_UP_NS / 1_000_000_000
            ),
            ClusterError::Egress(fault) => write!(f, "{fault}"),
        }
    }
}

impl std::error::Error for ClusterError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ClusterError::Members(error) => Some(error),
            ClusterError::Client(error) => Some(error),
            ClusterError::Egress(fault) => Some(fault),
            _ => None,
        }
    }
}

impl From<ClientError> for ClusterError {
    fn from(error: ClientError) -> Self {
        ClusterError::Client(error)
    }
}

/// What a session connect was answered with.
enum Answer {
    Ok {
        session_id: i64,
        leadership_term_id: i64,
        leader_member_id: i32,
    },
    Redirect {
        leader_member_id: i32,
    },
    Refused {
        detail: String,
    },
    Version {
        version: i32,
    },
}

/// A client's open session with a cluster: the handle for sending the cluster
/// messages and reading its replies.
///
/// One thread uses a session. [`poll`](Self::poll) is the session's duty cycle as
/// well as its read: call it at least every 500 ms, the keepalive interval, since a
/// session the leader hears nothing from for the session timeout, 10 s, is closed.
pub struct ClusterSession {
    egress: Subscription,
    /// A publication to each member's `ingress` endpoint, in the members string's
    /// order, held from the connect on so that following a new leader adds nothing.
    ingress: Vec<Publication>,
    member_ids: Vec<i32>,
    /// The place in `ingress` of the member the session sends to: the leader.
    leader: usize,
    session_id: i64,
    leadership_term_id: i64,
    /// The session's nano clock when it last sent anything, or 0 to send a keepalive
    /// at the next poll.
    last_sent_ns: i64,
    closed: bool,
    closed_detail: [u8; CLOSED_DETAIL_LENGTH],
    closed_detail_length: usize,
}

/// `<host>:<port>`, an IPv6 literal in its square brackets again.
fn host_port(endpoint: &Endpoint<'_>) -> String {
    let host = String::from_utf8_lossy(endpoint.host);
    if host.contains(':') {
        format!("[{host}]:{}", endpoint.port)
    } else {
        format!("{host}:{}", endpoint.port)
    }
}

/// Writes `message` into a claim of `publication` and commits it. Nothing is
/// allocated.
fn send(publication: &mut Publication, message: &Message<'_>) -> Result<i64, Offer> {
    let length = message.encoded_length();
    let mut claim = publication.try_claim(length)?;
    let limit = claim.body().len();
    match message.write(claim.body()) {
        Ok(_) => Ok(claim.commit()),
        Err(_) => {
            claim.abort();
            Err(Offer::TooLong {
                found: length,
                limit,
            })
        }
    }
}

impl ClusterSession {
    /// Opens a session, as [`Cavalry::connect_cluster`] says.
    pub(crate) fn connect(
        client: &mut Cavalry,
        members: &str,
        response_channel: &str,
    ) -> Result<Self, ClusterError> {
        let mut parsed = [EMPTY_MEMBER; MAX_MEMBERS];
        let count =
            parse_members(members.as_bytes(), &mut parsed, None).map_err(ClusterError::Members)?;
        let parsed = parsed.get(..count).unwrap_or(&[]);
        let mut egress = client.add_subscription(response_channel, EGRESS_STREAM_ID)?;
        let mut ingress = Vec::with_capacity(count);
        let mut member_ids = Vec::with_capacity(count);
        for member in parsed {
            ingress.push(client.add_exclusive_publication(
                &format!("cavalry:udp?endpoint={}", host_port(&member.ingress)),
                INGRESS_STREAM_ID,
            )?);
            member_ids.push(member.id);
        }
        let give_up = nano_time().saturating_add(CONNECT_GIVE_UP_NS as i64);
        // A correlation id no earlier connect of this client used: the clock, then
        // one more for each connect.
        let mut correlation_id = nano_time();
        let mut target = 0;
        let mut connects = 0u32;
        loop {
            if nano_time() >= give_up {
                return Err(ClusterError::ConnectTimeout { connects });
            }
            correlation_id = correlation_id.wrapping_add(1);
            let connect = Message::SessionConnect {
                correlation_id,
                response_stream_id: EGRESS_STREAM_ID,
                version: PROTOCOL_VERSION,
                response_channel: response_channel.as_bytes(),
                encoded_credentials: &[],
                client_info: &[],
            };
            let lost = nano_time()
                .saturating_add(SESSION_CONNECT_TIMEOUT_NS as i64)
                .min(give_up);
            let mut sent = false;
            let answer = loop {
                if !sent
                    && let Some(publication) = ingress.get_mut(target)
                    && send(publication, &connect).is_ok()
                {
                    sent = true;
                    connects += 1;
                }
                if let Some(answer) = Self::answer(&mut egress, correlation_id)? {
                    break Some(answer);
                }
                if nano_time() >= lost {
                    break None;
                }
                std::thread::sleep(Duration::from_millis(1));
            };
            match answer {
                Some(Answer::Ok {
                    session_id,
                    leadership_term_id,
                    leader_member_id,
                }) => {
                    // The member that answered ok is the leader, whatever id it
                    // names: a leader names itself.
                    let leader = member_ids
                        .iter()
                        .position(|&id| id == leader_member_id)
                        .unwrap_or(target);
                    return Ok(Self {
                        egress,
                        ingress,
                        member_ids,
                        leader,
                        session_id,
                        leadership_term_id,
                        last_sent_ns: nano_time(),
                        closed: false,
                        closed_detail: [0; CLOSED_DETAIL_LENGTH],
                        closed_detail_length: 0,
                    });
                }
                Some(Answer::Redirect { leader_member_id }) => {
                    match member_ids.iter().position(|&id| id == leader_member_id) {
                        Some(leader) => target = leader,
                        // The member knows no leader: an election is running.
                        None => {
                            std::thread::sleep(NO_LEADER_PAUSE);
                            target = (target + 1) % count;
                        }
                    }
                }
                Some(Answer::Refused { detail }) => return Err(ClusterError::Refused { detail }),
                Some(Answer::Version { version }) => return Err(ClusterError::Version { version }),
                // The connect was lost: the member is down, or cannot reach the
                // response channel. The next member may be reachable.
                None => target = (target + 1) % count,
            }
        }
    }

    /// Reads the response channel once for the session event that answers the connect
    /// with `correlation_id`.
    fn answer(
        egress: &mut Subscription,
        correlation_id: i64,
    ) -> Result<Option<Answer>, ClusterError> {
        let mut answer = None;
        egress
            .poll(
                |body: &[u8], header: &DataFrameHeader| {
                    if answer.is_some() || !header.flags().contains(Flags::UNFRAGMENTED) {
                        return;
                    }
                    let Ok(Message::SessionEvent {
                        cluster_session_id,
                        correlation_id: answered,
                        leadership_term_id,
                        leader_member_id,
                        code,
                        version,
                        detail,
                        ..
                    }) = Message::parse(Stream::Egress, body)
                    else {
                        return;
                    };
                    if answered != correlation_id {
                        return;
                    }
                    answer = Some(if version >> 16 != PROTOCOL_VERSION >> 16 {
                        Answer::Version { version }
                    } else {
                        match code {
                            EventCode::Ok => Answer::Ok {
                                session_id: cluster_session_id,
                                leadership_term_id,
                                leader_member_id,
                            },
                            EventCode::Redirect => Answer::Redirect { leader_member_id },
                            _ => Answer::Refused {
                                detail: String::from_utf8_lossy(detail).into_owned(),
                            },
                        }
                    });
                },
                CONNECT_POLL_LIMIT,
            )
            .map_err(ClusterError::Egress)?;
        Ok(answer)
    }

    /// Sends `message` to the leader as a session message: the session message
    /// header, with the term and the session id the session holds, then `message`.
    /// Returns the position after it on the session's publication to the leader.
    ///
    /// The refusals are [`Offer`]'s, and the caller decides what to do with each. A
    /// message is one frame in this version: one longer than
    /// [`max_message_length`](Self::max_message_length) is [`Offer::TooLong`]. Back
    /// pressure is the leader's log's, which reaches the caller here. A session the
    /// cluster closed is [`Offer::Closed`].
    ///
    /// An accepted message is on its way to the leader, not committed: the leader
    /// ignores one that carries an older term, and a leader lost before a quorum held
    /// the message loses it with no word. A caller that must know sends again when
    /// its service's reply does not come. Nothing here allocates.
    pub fn offer(&mut self, message: &[u8]) -> Result<i64, Offer> {
        if self.closed {
            return Err(Offer::Closed);
        }
        let Some(publication) = self.ingress.get_mut(self.leader) else {
            return Err(Offer::Closed);
        };
        let position = send(
            publication,
            &Message::SessionMessage {
                leadership_term_id: self.leadership_term_id,
                cluster_session_id: self.session_id,
                timestamp: 0,
                payload: message,
            },
        )?;
        self.last_sent_ns = nano_time();
        Ok(position)
    }

    /// The longest message [`offer`](Self::offer) takes: what one frame carries after
    /// the session message header.
    pub fn max_message_length(&self) -> usize {
        self.ingress
            .get(self.leader)
            .map_or(0, |publication| publication.max_payload_length())
            .saturating_sub(SESSION_MESSAGE_HEADER_LENGTH)
    }

    /// Reads up to `fragment_limit` messages the cluster sent this session, each
    /// handed to `handler` as its payload and the cluster's time when the leader sent
    /// it, milliseconds since 1970-01-01T00:00:00Z. Returns how many were handed
    /// over, or the [`Fault`] that stopped the read of the response channel.
    ///
    /// This is the session's duty cycle too. A new leader event turns the session to
    /// the new leader with its term, and the next thing sent is a keepalive. A session
    /// event of code closed closes the session, and [`closed`](Self::closed) then
    /// gives the cluster's reason. When the keepalive interval, 500 ms, has passed
    /// since the session last sent anything, a keepalive is sent. Nothing here
    /// allocates: the publication to each member was added by the connect.
    pub fn poll<F>(&mut self, mut handler: F, fragment_limit: usize) -> Result<usize, Fault>
    where
        F: FnMut(&[u8], i64),
    {
        let Self {
            egress,
            ingress,
            member_ids,
            leader,
            session_id,
            leadership_term_id,
            last_sent_ns,
            closed,
            closed_detail,
            closed_detail_length,
        } = self;
        let mut delivered = 0;
        egress.poll(
            |body: &[u8], header: &DataFrameHeader| {
                if !header.flags().contains(Flags::UNFRAGMENTED) {
                    return;
                }
                match Message::parse(Stream::Egress, body) {
                    Ok(Message::SessionMessage {
                        cluster_session_id,
                        timestamp,
                        payload,
                        ..
                    }) if cluster_session_id == *session_id => {
                        handler(payload, timestamp);
                        delivered += 1;
                    }
                    Ok(Message::NewLeaderEvent {
                        leadership_term_id: term,
                        cluster_session_id,
                        leader_member_id,
                        ..
                    }) if cluster_session_id == *session_id && term >= *leadership_term_id => {
                        *leadership_term_id = term;
                        if let Some(place) =
                            member_ids.iter().position(|&id| id == leader_member_id)
                        {
                            *leader = place;
                        }
                        // The next thing sent is a keepalive, with the new term.
                        *last_sent_ns = 0;
                    }
                    Ok(Message::SessionEvent {
                        cluster_session_id,
                        code: EventCode::Closed,
                        detail,
                        ..
                    }) if cluster_session_id == *session_id => {
                        *closed = true;
                        let length = detail.len().min(CLOSED_DETAIL_LENGTH);
                        if let (Some(held), Some(detail)) =
                            (closed_detail.get_mut(..length), detail.get(..length))
                        {
                            held.copy_from_slice(detail);
                            *closed_detail_length = length;
                        }
                    }
                    _ => {}
                }
            },
            fragment_limit,
        )?;
        if !*closed {
            let now = nano_time();
            if now.saturating_sub(*last_sent_ns) >= KEEPALIVE_INTERVAL_NS as i64
                && let Some(publication) = ingress.get_mut(*leader)
                && send(
                    publication,
                    &Message::SessionKeepalive {
                        leadership_term_id: *leadership_term_id,
                        cluster_session_id: *session_id,
                    },
                )
                .is_ok()
            {
                *last_sent_ns = now;
            }
        }
        Ok(delivered)
    }

    /// The id the leader gave this session.
    pub fn session_id(&self) -> i64 {
        self.session_id
    }

    /// The leadership term the session sends with: the term of the leader that
    /// opened it, or of the last new leader event.
    pub fn leadership_term_id(&self) -> i64 {
        self.leadership_term_id
    }

    /// The id of the member the session sends to: the leader, as the cluster last
    /// said.
    pub fn leader_member_id(&self) -> i32 {
        self.member_ids.get(self.leader).copied().unwrap_or(-1)
    }

    /// Whether the cluster closed the session, and its reason when it did: `TIMEOUT`
    /// for a session the leader heard nothing from for the session timeout. `None`
    /// while the session is open.
    pub fn closed(&self) -> Option<&str> {
        self.closed.then(|| {
            core::str::from_utf8(
                self.closed_detail
                    .get(..self.closed_detail_length)
                    .unwrap_or(&[]),
            )
            .unwrap_or("")
        })
    }

    /// Closes the session: a session close to the leader, which answers nothing,
    /// then the session's publications and its subscription. The session close is
    /// offered for at most the keepalive interval; a leader that does not take it in
    /// that time closes the session by its timeout.
    pub fn close(mut self) -> Result<(), ClusterError> {
        if !self.closed {
            let close = Message::SessionClose {
                leadership_term_id: self.leadership_term_id,
                cluster_session_id: self.session_id,
            };
            let give_up = nano_time().saturating_add(KEEPALIVE_INTERVAL_NS as i64);
            while let Some(publication) = self.ingress.get_mut(self.leader) {
                match send(publication, &close) {
                    Err(Offer::BackPressured | Offer::NotConnected | Offer::AdminAction)
                        if nano_time() < give_up =>
                    {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    _ => break,
                }
            }
        }
        for publication in self.ingress.drain(..) {
            publication.close()?;
        }
        self.egress.close()?;
        Ok(())
    }
}

/// The length of the session message header before a session message's payload: the
/// message header and its 24 bytes of fields (`spec/cluster.md`, "Session message
/// header, template 1").
const SESSION_MESSAGE_HEADER_LENGTH: usize = 32;

impl fmt::Debug for ClusterSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClusterSession")
            .field("session_id", &self.session_id)
            .field("leadership_term_id", &self.leadership_term_id)
            .field("leader_member_id", &self.leader_member_id())
            .field("closed", &self.closed())
            .finish()
    }
}
