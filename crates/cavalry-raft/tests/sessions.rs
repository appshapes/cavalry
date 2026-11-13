//! A cluster member's sessions, on one [`Sessions`] driven by hand (`spec/cluster.md`,
//! "Sessions"; ADR 0115): what a member does with each message of the ingress stream,
//! what the committed session open and close events of its log do to its table, and
//! what its duty cycle sends, appends and counts. The world is in memory: the egress
//! publications are slots that take a message or do not, the log is a list of
//! entries, and a commit hands the entries not yet applied to `on_entry`, as the
//! member's read of its own log does. The bounds come first (`.claude/rules/data-plane.md`).

use std::convert::Infallible;

use cavalry_proto::cluster::{
    CONCURRENT_SESSION_LIMIT, CloseReason, EGRESS_STREAM_ID, EventCode, INVALID_CLIENT_VERSION,
    INVALID_RESPONSE_CHANNEL, MAX_PENDING_ANSWERS, MAX_SESSIONS, Message, Stream,
};
use cavalry_raft::sessions::{EGRESS_SLOTS, MAX_RESPONSE_CHANNEL_LENGTH, TIMEOUT_DETAIL};
use cavalry_raft::{Clock, Ingress, Leadership, SessionCounts, SessionIo, Sessions};

const ENDPOINTS: &[u8] = b"0=10.0.0.1:9010,1=10.0.0.2:9010,2=10.0.0.3:9010";
const TIME: i64 = 1_700_000_000_000;

fn at(ms: u64) -> Clock {
    Clock {
        now_ns: ms * 1_000_000,
        time_ms: TIME + ms as i64,
    }
}

/// Member 0 leading `term` with its election closed.
fn leader(term: i64) -> Leadership {
    Leadership {
        leads: true,
        leadership_term_id: term,
        leader_member_id: 0,
    }
}

/// A member that does not lead and knows `leader`, −1 for none.
fn follower(term: i64, leader: i32) -> Leadership {
    Leadership {
        leads: false,
        leadership_term_id: term,
        leader_member_id: leader,
    }
}

fn channel(port: u16) -> Vec<u8> {
    format!("cavalry:udp?endpoint=10.0.1.1:{port}").into_bytes()
}

fn connect(correlation_id: i64, response_channel: &[u8]) -> Message<'_> {
    Message::SessionConnect {
        correlation_id,
        response_stream_id: EGRESS_STREAM_ID,
        version: 0,
        response_channel,
        encoded_credentials: &[],
        client_info: b"test",
    }
}

fn encode(message: &Message<'_>) -> Vec<u8> {
    let mut bytes = vec![0u8; message.encoded_length()];
    let length = message.write(&mut bytes).expect("encodes");
    bytes.truncate(length);
    bytes
}

/// What the sessions reach: the egress publications by slot, what was sent on each,
/// and the leader's log.
struct World {
    egress: Vec<Option<Vec<u8>>>,
    /// Whether a publication takes a message: its client has connected to it.
    connected: bool,
    sent: Vec<(usize, Vec<u8>)>,
    log: Vec<Vec<u8>>,
    /// How many entries of the log have been handed to `on_entry`.
    applied: usize,
    back_pressured: bool,
    /// Whether the driver refuses the next channel it is asked to add.
    refuses: bool,
}

impl World {
    fn new() -> Self {
        Self {
            egress: vec![None; EGRESS_SLOTS],
            connected: true,
            sent: Vec::new(),
            log: Vec::new(),
            applied: 0,
            back_pressured: false,
            refuses: false,
        }
    }

    /// The slots that hold a publication.
    fn held(&self) -> Vec<usize> {
        (0..EGRESS_SLOTS)
            .filter(|&slot| self.egress[slot].is_some())
            .collect()
    }

    /// Every message sent since the last call, as its slot and a line.
    fn sent(&mut self) -> Vec<String> {
        let sent = std::mem::take(&mut self.sent);
        sent.iter()
            .map(|(slot, bytes)| {
                let line = match Message::parse(Stream::Egress, bytes).expect("an egress message") {
                    Message::SessionEvent {
                        cluster_session_id,
                        correlation_id,
                        leadership_term_id,
                        leader_member_id,
                        code,
                        detail,
                        ..
                    } => format!(
                        "event {code:?} session {cluster_session_id} correlation \
                         {correlation_id} term {leadership_term_id} leader {leader_member_id} \
                         detail {:?}",
                        String::from_utf8_lossy(detail)
                    ),
                    Message::NewLeaderEvent {
                        leadership_term_id,
                        cluster_session_id,
                        leader_member_id,
                        ingress_endpoints,
                    } => format!(
                        "new leader session {cluster_session_id} term {leadership_term_id} \
                         leader {leader_member_id} endpoints {:?}",
                        String::from_utf8_lossy(ingress_endpoints)
                    ),
                    other => format!("{other:?}"),
                };
                format!("{slot}: {line}")
            })
            .collect()
    }

    /// The entries of the log from `from` on, each as a line.
    fn entries(&self, from: usize) -> Vec<String> {
        self.log[from..]
            .iter()
            .map(
                |bytes| match Message::parse(Stream::Log, bytes).expect("an entry") {
                    Message::SessionOpenEvent {
                        leadership_term_id,
                        correlation_id,
                        cluster_session_id,
                        timestamp,
                        response_stream_id,
                        response_channel,
                        ..
                    } => format!(
                        "open session {cluster_session_id} correlation {correlation_id} term \
                         {leadership_term_id} at {} stream {response_stream_id} channel {}",
                        timestamp - TIME,
                        String::from_utf8_lossy(response_channel)
                    ),
                    Message::SessionCloseEvent {
                        leadership_term_id,
                        cluster_session_id,
                        timestamp,
                        close_reason,
                    } => format!(
                        "close session {cluster_session_id} term {leadership_term_id} at {} \
                         {close_reason:?}",
                        timestamp - TIME
                    ),
                    Message::SessionMessage {
                        leadership_term_id,
                        cluster_session_id,
                        timestamp,
                        payload,
                    } => format!(
                        "message session {cluster_session_id} term {leadership_term_id} at {} \
                         {:?}",
                        timestamp - TIME,
                        String::from_utf8_lossy(payload)
                    ),
                    other => format!("{other:?}"),
                },
            )
            .collect()
    }
}

impl SessionIo for World {
    type Fault = Infallible;

    fn add_egress(&mut self, slot: usize, channel: &[u8]) -> Result<bool, Infallible> {
        assert!(self.egress[slot].is_none(), "slot {slot} is held");
        if std::mem::take(&mut self.refuses) {
            return Ok(false);
        }
        self.egress[slot] = Some(channel.to_vec());
        Ok(true)
    }

    fn send_egress(&mut self, slot: usize, message: &Message<'_>) -> bool {
        if self.egress[slot].is_none() || !self.connected {
            return false;
        }
        self.sent.push((slot, encode(message)));
        true
    }

    fn remove_egress(&mut self, slot: usize) -> Result<(), Infallible> {
        self.egress[slot] = None;
        Ok(())
    }

    fn append(&mut self, entry: &Message<'_>) -> Result<bool, Infallible> {
        if self.back_pressured {
            return Ok(false);
        }
        self.log.push(encode(entry));
        Ok(true)
    }
}

fn ingress(
    sessions: &mut Sessions,
    world: &mut World,
    ms: u64,
    lead: Leadership,
    message: &Message<'_>,
) -> Ingress {
    let Ok(ingress) = sessions.on_ingress(at(ms), lead, message, world);
    ingress
}

fn work(sessions: &mut Sessions, world: &mut World, ms: u64, lead: Leadership) -> usize {
    let Ok(work) = sessions.do_work(at(ms), lead, world);
    work
}

/// Commits the log: every entry not yet applied is handed to `on_entry`, in order.
/// Returns what `on_entry` answered for each.
fn commit(sessions: &mut Sessions, world: &mut World, ms: u64) -> Vec<bool> {
    let entries: Vec<Vec<u8>> = world.log[world.applied..].to_vec();
    world.applied = world.log.len();
    entries
        .iter()
        .map(|bytes| {
            let entry = Message::parse(Stream::Log, bytes).expect("an entry");
            let Ok(known) = sessions.on_entry(at(ms), &entry, world);
            known
        })
        .collect()
}

/// A leader of term 1 with `count` sessions open, ids from 0, each told ok.
fn leading(count: usize) -> (Sessions, World) {
    let mut sessions = Sessions::new(0, ENDPOINTS);
    let mut world = World::new();
    work(&mut sessions, &mut world, 0, leader(1));
    for n in 0..count {
        let response = channel(20_000 + n as u16);
        assert_eq!(
            ingress(
                &mut sessions,
                &mut world,
                0,
                leader(1),
                &connect(n as i64, &response)
            ),
            Ingress::Taken
        );
    }
    commit(&mut sessions, &mut world, 0);
    work(&mut sessions, &mut world, 0, leader(1));
    assert_eq!(sessions.open(), count);
    assert_eq!(world.sent().len(), count, "each session was told ok");
    (sessions, world)
}

/// The bounds first: ten sessions, open or connecting, and ten pending answers
/// (`spec/cluster.md`, "Values"). An eleventh connect is refused with `concurrent
/// session limit`, and a connect that finds the pending answers taken is dropped
/// unanswered and counted.
#[test]
fn an_eleventh_session_is_refused_and_an_eleventh_answer_is_dropped() {
    let mut sessions = Sessions::new(0, ENDPOINTS);
    let mut world = World::new();
    // Nobody connects to an egress publication in this test: every answer stays owed.
    world.connected = false;
    for n in 0..MAX_SESSIONS {
        let response = channel(20_000 + n as u16);
        ingress(
            &mut sessions,
            &mut world,
            0,
            leader(1),
            &connect(n as i64, &response),
        );
    }
    assert_eq!(world.log.len(), MAX_SESSIONS, "ten session open events");
    assert_eq!(world.held(), (0..MAX_SESSIONS).collect::<Vec<_>>());
    assert_eq!(sessions.open(), 0, "connecting until their events commit");
    // The eleventh, while the ten are connecting: refused, in a pending answer's slot.
    for n in 0..MAX_PENDING_ANSWERS {
        let response = channel(21_000 + n as u16);
        ingress(
            &mut sessions,
            &mut world,
            0,
            leader(1),
            &connect(100 + n as i64, &response),
        );
    }
    assert_eq!(
        world.log.len(),
        MAX_SESSIONS,
        "nothing appended for a refusal"
    );
    assert_eq!(sessions.pending_answers(), MAX_PENDING_ANSWERS);
    assert_eq!(world.held(), (0..EGRESS_SLOTS).collect::<Vec<_>>());
    // One more: no session's slot and no answer's. Dropped, and no publication added.
    let response = channel(22_000);
    assert_eq!(
        ingress(
            &mut sessions,
            &mut world,
            0,
            leader(1),
            &connect(200, &response)
        ),
        Ingress::Taken
    );
    assert_eq!(
        sessions.counts(),
        SessionCounts {
            accepted: MAX_SESSIONS as u64,
            refused: MAX_PENDING_ANSWERS as u64,
            connects_dropped: 1,
            ..SessionCounts::default()
        }
    );
    // Committed, the ten are open and the bound still holds; the answers go out once
    // their clients have connected, each a refusal with the limit as its detail.
    commit(&mut sessions, &mut world, 1);
    assert_eq!(sessions.open(), MAX_SESSIONS);
    world.connected = true;
    work(&mut sessions, &mut world, 2, leader(1));
    let sent = world.sent();
    let refusals: Vec<&String> = sent
        .iter()
        .filter(|line| line.contains("event Error"))
        .collect();
    assert_eq!(refusals.len(), MAX_PENDING_ANSWERS);
    assert_eq!(
        refusals[0],
        &format!(
            "10: event Error session -1 correlation 100 term 1 leader 0 detail {:?}",
            String::from_utf8_lossy(CONCURRENT_SESSION_LIMIT)
        )
    );
    assert_eq!(sessions.pending_answers(), 0);
    assert_eq!(world.held(), (0..MAX_SESSIONS).collect::<Vec<_>>());
}

/// A connect the leader accepts: an egress publication on the response channel, the
/// next session id, a session open event in the log; the session is open once that
/// entry is committed, and the ok event follows with the connect's correlation id.
#[test]
fn a_connect_is_accepted_opened_by_its_committed_event_and_told_ok() {
    let mut sessions = Sessions::new(0, ENDPOINTS);
    let mut world = World::new();
    let response = channel(20_000);
    assert_eq!(
        ingress(
            &mut sessions,
            &mut world,
            5,
            leader(3),
            &connect(77, &response)
        ),
        Ingress::Taken
    );
    assert_eq!(world.egress[0].as_deref(), Some(response.as_slice()));
    assert_eq!(
        world.entries(0),
        [
            "open session 0 correlation 77 term 3 at 5 stream 102 channel \
          cavalry:udp?endpoint=10.0.1.1:20000"
        ]
    );
    // Not open, and nothing sent, until the entry is committed.
    work(&mut sessions, &mut world, 6, leader(3));
    assert!(!sessions.is_open(0));
    assert_eq!(world.sent(), [] as [String; 0]);
    assert_eq!(commit(&mut sessions, &mut world, 7), [true]);
    assert!(sessions.is_open(0));
    // The client has not connected to the publication yet: the ok event stays owed.
    world.connected = false;
    work(&mut sessions, &mut world, 8, leader(3));
    assert_eq!(world.sent(), [] as [String; 0]);
    world.connected = true;
    work(&mut sessions, &mut world, 9, leader(3));
    assert_eq!(
        world.sent(),
        ["0: event Ok session 0 correlation 77 term 3 leader 0 detail \"\""]
    );
    work(&mut sessions, &mut world, 10, leader(3));
    assert_eq!(world.sent(), [] as [String; 0], "sent once");
    // The next session has the next id.
    let second = channel(20_001);
    ingress(
        &mut sessions,
        &mut world,
        11,
        leader(3),
        &connect(78, &second),
    );
    assert!(world.entries(1)[0].starts_with("open session 1 correlation 78"));
    assert_eq!(
        sessions.counts(),
        SessionCounts {
            accepted: 2,
            opened: 1,
            ..SessionCounts::default()
        }
    );
}

/// A session open event the back-pressured log does not take waits, as an entry of
/// the leader's own does, and is appended by a later cycle; one the log does not take
/// for the session connect timeout is given up, counted as a connect dropped.
#[test]
fn a_session_open_event_waits_for_the_log_and_is_given_up_at_the_connect_timeout() {
    let mut sessions = Sessions::new(0, ENDPOINTS);
    let mut world = World::new();
    world.back_pressured = true;
    let response = channel(20_000);
    assert_eq!(
        ingress(
            &mut sessions,
            &mut world,
            0,
            leader(1),
            &connect(1, &response)
        ),
        Ingress::Taken,
        "accepted: the event waits, the connect does not"
    );
    work(&mut sessions, &mut world, 100, leader(1));
    assert!(world.log.is_empty());
    world.back_pressured = false;
    work(&mut sessions, &mut world, 200, leader(1));
    assert_eq!(world.log.len(), 1);
    work(&mut sessions, &mut world, 300, leader(1));
    assert_eq!(world.log.len(), 1, "appended once");

    // A second, which the log never takes.
    world.back_pressured = true;
    let second = channel(20_001);
    ingress(
        &mut sessions,
        &mut world,
        1_000,
        leader(1),
        &connect(2, &second),
    );
    work(&mut sessions, &mut world, 5_999, leader(1));
    assert_eq!(world.held(), [0, 1]);
    work(&mut sessions, &mut world, 6_000, leader(1));
    assert_eq!(world.held(), [0], "its publication is removed");
    assert_eq!(sessions.counts().connects_dropped, 1);
    assert_eq!(world.log.len(), 1);
}

/// A member that does not lead opens nothing: it answers a connect with a redirect
/// that names its leader, −1 when it knows none, and every member's ingress endpoint.
/// A member whose own election as leader has not closed leaves the connect unread.
#[test]
fn a_follower_redirects_and_a_leader_whose_election_runs_leaves_the_connect_unread() {
    let mut sessions = Sessions::new(1, ENDPOINTS);
    let mut world = World::new();
    let response = channel(20_000);
    assert_eq!(
        ingress(
            &mut sessions,
            &mut world,
            0,
            follower(4, 2),
            &connect(9, &response)
        ),
        Ingress::Taken
    );
    assert_eq!(world.held(), [MAX_SESSIONS], "a pending answer's slot");
    assert!(world.log.is_empty());
    work(&mut sessions, &mut world, 1, follower(4, 2));
    assert_eq!(
        world.sent(),
        [format!(
            "10: event Redirect session -1 correlation 9 term 4 leader 2 detail {:?}",
            String::from_utf8_lossy(ENDPOINTS)
        )]
    );
    assert_eq!(world.held(), [] as [usize; 0], "removed once sent");
    // No leader known.
    ingress(
        &mut sessions,
        &mut world,
        2,
        follower(4, -1),
        &connect(10, &response),
    );
    work(&mut sessions, &mut world, 3, follower(4, -1));
    assert!(world.sent()[0].contains("event Redirect session -1 correlation 10 term 4 leader -1"));
    // This member is the one being elected: the connect stays on the stream.
    assert_eq!(
        ingress(
            &mut sessions,
            &mut world,
            4,
            follower(5, 1),
            &connect(11, &response)
        ),
        Ingress::Unread
    );
    assert_eq!(world.held(), [] as [usize; 0]);
    assert_eq!(sessions.counts(), SessionCounts::default());
}

/// The leader's refusals, each an answer of code error with its reason, and the
/// connects no member can answer, each dropped: a response channel no publication
/// can be added on, and one the driver refuses.
#[test]
fn a_leader_refuses_a_version_and_a_response_channel_and_drops_what_it_cannot_answer() {
    let mut sessions = Sessions::new(0, ENDPOINTS);
    let mut world = World::new();
    let response = channel(20_000);
    // Another major version.
    ingress(
        &mut sessions,
        &mut world,
        0,
        leader(1),
        &Message::SessionConnect {
            correlation_id: 1,
            response_stream_id: EGRESS_STREAM_ID,
            version: 1 << 16,
            response_channel: &response,
            encoded_credentials: &[],
            client_info: &[],
        },
    );
    // Another response stream.
    ingress(
        &mut sessions,
        &mut world,
        0,
        leader(1),
        &Message::SessionConnect {
            correlation_id: 2,
            response_stream_id: EGRESS_STREAM_ID + 1,
            version: 0,
            response_channel: &response,
            encoded_credentials: &[],
            client_info: &[],
        },
    );
    // A `cavalry:udp` channel that is more than an endpoint, one with no port, and
    // one longer than a session holds.
    let long = format!(
        "cavalry:udp?endpoint={}:1",
        "h".repeat(MAX_RESPONSE_CHANNEL_LENGTH)
    );
    for (correlation, bad) in [
        (3, "cavalry:udp?endpoint=10.0.1.1:20000|interface=10.0.1.1"),
        (4, "cavalry:udp?control=10.0.1.1:20000|control-mode=manual"),
        (5, long.as_str()),
    ] {
        ingress(
            &mut sessions,
            &mut world,
            0,
            leader(1),
            &connect(correlation, bad.as_bytes()),
        );
    }
    work(&mut sessions, &mut world, 1, leader(1));
    let sent = world.sent();
    let reasons: Vec<(bool, bool)> = sent
        .iter()
        .map(|line| {
            (
                line.contains(&*String::from_utf8_lossy(INVALID_CLIENT_VERSION)),
                line.contains(&*String::from_utf8_lossy(INVALID_RESPONSE_CHANNEL)),
            )
        })
        .collect();
    assert_eq!(
        reasons,
        [
            (true, false),
            (false, true),
            (false, true),
            (false, true),
            (false, true)
        ],
        "{sent:?}"
    );
    assert!(
        sent.iter()
            .all(|line| line.contains("event Error session -1"))
    );
    assert_eq!(sessions.counts().refused, 5);
    assert!(world.log.is_empty() && world.held().is_empty());

    // Not `cavalry:udp`, or not a channel: no publication to it can exist.
    for bad in ["cavalry:ipc", "", "udp://10.0.1.1:20000"] {
        assert_eq!(
            ingress(
                &mut sessions,
                &mut world,
                2,
                leader(1),
                &connect(6, bad.as_bytes())
            ),
            Ingress::Taken
        );
    }
    // A host the driver cannot resolve, on the leader's accept and on a follower's
    // redirect: the driver refuses the channel.
    world.refuses = true;
    ingress(
        &mut sessions,
        &mut world,
        2,
        leader(1),
        &connect(7, &response),
    );
    world.refuses = true;
    ingress(
        &mut sessions,
        &mut world,
        2,
        follower(1, 2),
        &connect(8, &response),
    );
    assert_eq!(
        sessions.counts(),
        SessionCounts {
            refused: 5,
            connects_dropped: 5,
            ..SessionCounts::default()
        }
    );
    assert!(world.log.is_empty() && world.held().is_empty());
    assert_eq!(sessions.pending_answers(), 0);
}

/// An answer the member cannot send, because the client never connects to the
/// publication, is dropped at the session connect timeout, 5 s after the connect.
#[test]
fn an_answer_unsent_at_the_session_connect_timeout_is_dropped() {
    let mut sessions = Sessions::new(1, ENDPOINTS);
    let mut world = World::new();
    world.connected = false;
    let response = channel(20_000);
    ingress(
        &mut sessions,
        &mut world,
        1_000,
        follower(1, 0),
        &connect(1, &response),
    );
    work(&mut sessions, &mut world, 5_999, follower(1, 0));
    assert_eq!((sessions.pending_answers(), world.held().len()), (1, 1));
    work(&mut sessions, &mut world, 6_000, follower(1, 0));
    assert_eq!((sessions.pending_answers(), world.held().len()), (0, 0));
    assert_eq!(sessions.counts().connects_dropped, 1);
}

/// A session message of an open session is appended with the leader's term and the
/// cluster's time, and the client's payload. One the back-pressured log does not
/// take is left unread, and nothing of it is kept. A message for another term, for
/// a session that is not open, or read by a member that does not lead is ignored
/// and counted.
#[test]
fn a_session_message_is_appended_or_left_unread_and_never_kept() {
    let (mut sessions, mut world) = leading(1);
    let message = |term: i64, session: i64| Message::SessionMessage {
        leadership_term_id: term,
        cluster_session_id: session,
        timestamp: 0,
        payload: b"hello",
    };
    world.back_pressured = true;
    for ms in [10, 11] {
        assert_eq!(
            ingress(&mut sessions, &mut world, ms, leader(1), &message(1, 0)),
            Ingress::Unread
        );
    }
    assert_eq!(world.log.len(), 1, "the session open event alone");
    world.back_pressured = false;
    assert_eq!(
        ingress(&mut sessions, &mut world, 12, leader(1), &message(1, 0)),
        Ingress::Taken
    );
    assert_eq!(
        world.entries(1),
        ["message session 0 term 1 at 12 \"hello\""],
        "once, with the leader's time"
    );
    for (lead, term, session) in [
        (leader(1), 0, 0),
        (leader(1), 2, 0),
        (leader(1), 1, 1),
        (follower(1, 2), 1, 0),
    ] {
        assert_eq!(
            ingress(&mut sessions, &mut world, 13, lead, &message(term, session)),
            Ingress::Taken
        );
    }
    assert_eq!(world.log.len(), 2);
    assert_eq!(sessions.counts().ingress_ignored, 4);
}

/// A session with nothing from its client for the session timeout, 10 s, is closed:
/// a session close event with the reason timeout, and a session event of code closed
/// with the detail `TIMEOUT`. A keepalive and a session message are each the
/// session's activity. The session is closed, and its publication removed, once the
/// close event is committed.
#[test]
fn a_silent_session_is_closed_at_the_session_timeout_and_a_keepalive_holds_it() {
    let (mut sessions, mut world) = leading(2);
    let keepalive = Message::SessionKeepalive {
        leadership_term_id: 1,
        cluster_session_id: 1,
    };
    ingress(&mut sessions, &mut world, 9_000, leader(1), &keepalive);
    work(&mut sessions, &mut world, 9_999, leader(1));
    assert_eq!(world.log.len(), 2, "two session open events");
    work(&mut sessions, &mut world, 10_000, leader(1));
    assert_eq!(
        world.entries(2),
        ["close session 0 term 1 at 10000 Timeout"]
    );
    assert_eq!(
        world.sent(),
        [format!(
            "0: event Closed session 0 correlation -1 term 1 leader 0 detail {:?}",
            String::from_utf8_lossy(TIMEOUT_DETAIL)
        )]
    );
    // Decided once: the next cycles append and send nothing more for it, and what
    // the client sends for it now is ignored.
    work(&mut sessions, &mut world, 10_001, leader(1));
    assert_eq!((world.log.len(), world.sent().len()), (3, 0));
    let late = Message::SessionKeepalive {
        leadership_term_id: 1,
        cluster_session_id: 0,
    };
    ingress(&mut sessions, &mut world, 10_002, leader(1), &late);
    assert_eq!(sessions.counts().ingress_ignored, 1);
    assert!(sessions.is_open(0), "open until the close is committed");
    assert_eq!(commit(&mut sessions, &mut world, 10_003), [true]);
    assert!(!sessions.is_open(0) && sessions.is_open(1));
    assert_eq!(world.held(), [1]);
    // The other session's keepalive was at 9 s: it is closed at 19 s and not before.
    work(&mut sessions, &mut world, 18_999, leader(1));
    assert_eq!(world.log.len(), 3);
    work(&mut sessions, &mut world, 19_000, leader(1));
    assert_eq!(
        world.entries(3),
        ["close session 1 term 1 at 19000 Timeout"]
    );
    assert_eq!(
        sessions.counts(),
        SessionCounts {
            accepted: 2,
            opened: 2,
            closed: 1,
            timed_out: 2,
            ingress_ignored: 1,
            ..SessionCounts::default()
        }
    );
}

/// A timeout's session close event goes to the log before its closed event goes to
/// the client, as in Aeron's `SessionManager.checkSessions`: while the log is
/// back-pressured nothing is appended and nothing is sent, and the closed event is
/// sent in the cycle the log takes the close event. Each once, and counted once.
#[test]
fn a_timeouts_closed_event_waits_for_its_close_event_in_the_log() {
    let (mut sessions, mut world) = leading(1);
    world.back_pressured = true;
    work(&mut sessions, &mut world, 10_000, leader(1));
    work(&mut sessions, &mut world, 10_100, leader(1));
    assert_eq!(world.log.len(), 1);
    assert_eq!(
        world.sent(),
        [] as [String; 0],
        "nothing told while the log holds no close"
    );
    assert_eq!(sessions.counts().timed_out, 1, "counted once");
    world.back_pressured = false;
    work(&mut sessions, &mut world, 10_200, leader(1));
    assert_eq!(
        world.entries(1),
        ["close session 0 term 1 at 10200 Timeout"]
    );
    assert_eq!(
        world.sent(),
        [format!(
            "0: event Closed session 0 correlation -1 term 1 leader 0 detail {:?}",
            String::from_utf8_lossy(TIMEOUT_DETAIL)
        )],
        "in the cycle the log took the close"
    );
    work(&mut sessions, &mut world, 10_300, leader(1));
    assert_eq!(
        (world.log.len(), world.sent().len()),
        (2, 0),
        "appended and sent once"
    );
    assert_eq!(sessions.counts().timed_out, 1);
}

/// A closed event the session's publication does not take is owed, as an ok event
/// is, and is sent in the cycle the publication takes it. The committed close frees
/// the session and its publication, and the debt with them. What the session was
/// still owed when the leader decided its close, an ok event here, is never sent:
/// nothing follows the closed event.
#[test]
fn a_closed_event_is_owed_until_taken_or_the_close_is_committed() {
    let mut sessions = Sessions::new(0, ENDPOINTS);
    let mut world = World::new();
    work(&mut sessions, &mut world, 0, leader(1));
    // Two sessions, opened at 0 s and at 5 s, whose clients have not connected to
    // their publications: each ok event is owed.
    world.connected = false;
    for (n, ms) in [(0u16, 0), (1, 5_000)] {
        let response = channel(20_000 + n);
        ingress(
            &mut sessions,
            &mut world,
            ms,
            leader(1),
            &connect(i64::from(n), &response),
        );
        commit(&mut sessions, &mut world, ms);
    }
    work(&mut sessions, &mut world, 5_001, leader(1));
    assert_eq!(sessions.open(), 2);
    // Session 0 times out at 10 s: its close event is in the log, its closed event
    // owed while the publication takes nothing.
    work(&mut sessions, &mut world, 10_000, leader(1));
    work(&mut sessions, &mut world, 10_100, leader(1));
    assert_eq!(
        world.entries(2),
        ["close session 0 term 1 at 10000 Timeout"]
    );
    assert_eq!(world.sent(), [] as [String; 0]);
    // The clients connect: session 0 is told it is closed and not that it is open;
    // session 1, still open, is told ok.
    world.connected = true;
    work(&mut sessions, &mut world, 10_200, leader(1));
    assert_eq!(
        world.sent(),
        [
            format!(
                "0: event Closed session 0 correlation -1 term 1 leader 0 detail {:?}",
                String::from_utf8_lossy(TIMEOUT_DETAIL)
            ),
            "1: event Ok session 1 correlation 1 term 1 leader 0 detail \"\"".to_owned(),
        ]
    );
    work(&mut sessions, &mut world, 10_300, leader(1));
    assert_eq!(world.sent(), [] as [String; 0], "sent once");
    assert_eq!(commit(&mut sessions, &mut world, 10_300), [true]);
    assert_eq!(world.held(), [1]);
    // Session 1 times out at 15 s while its publication takes nothing, and its close
    // is committed before it takes anything: nothing is sent for it after.
    world.connected = false;
    work(&mut sessions, &mut world, 15_000, leader(1));
    assert_eq!(
        world.entries(3),
        ["close session 1 term 1 at 15000 Timeout"]
    );
    assert_eq!(commit(&mut sessions, &mut world, 15_001), [true]);
    world.connected = true;
    work(&mut sessions, &mut world, 15_002, leader(1));
    assert_eq!(
        world.sent(),
        [] as [String; 0],
        "the committed close ended it"
    );
    assert_eq!((sessions.open(), world.held().len()), (0, 0));
    assert_eq!(
        sessions.counts(),
        SessionCounts {
            accepted: 2,
            opened: 2,
            closed: 2,
            timed_out: 2,
            ..SessionCounts::default()
        }
    );
}

/// A client's session close: a session close event with the reason client action,
/// and nothing sent. Left unread while the log is back-pressured.
#[test]
fn a_clients_close_is_appended_and_answered_with_nothing() {
    let (mut sessions, mut world) = leading(1);
    let close = Message::SessionClose {
        leadership_term_id: 1,
        cluster_session_id: 0,
    };
    world.back_pressured = true;
    assert_eq!(
        ingress(&mut sessions, &mut world, 100, leader(1), &close),
        Ingress::Unread
    );
    world.back_pressured = false;
    assert_eq!(
        ingress(&mut sessions, &mut world, 101, leader(1), &close),
        Ingress::Taken
    );
    assert_eq!(
        world.entries(1),
        ["close session 0 term 1 at 101 ClientAction"]
    );
    // A second close, and a message, for a session that is closing: ignored.
    ingress(&mut sessions, &mut world, 102, leader(1), &close);
    assert_eq!(sessions.counts().ingress_ignored, 1);
    work(&mut sessions, &mut world, 103, leader(1));
    assert_eq!((world.log.len(), world.sent().len()), (2, 0));
    commit(&mut sessions, &mut world, 104);
    assert_eq!(sessions.open(), 0);
    assert_eq!(world.held(), [] as [usize; 0]);
    assert_eq!(sessions.counts().closed, 1);
    // Nothing times out a session that is closed.
    work(&mut sessions, &mut world, 20_000, leader(1));
    assert_eq!(sessions.counts().timed_out, 0);
}

/// The sessions are the log's: a follower that reads the same committed entries holds
/// the same sessions, with no publication, and when it leads a term it takes them
/// up, each with a publication on its response channel, the term's new leader event
/// and the session timeout counted from the close of its election. The leader it
/// replaces gives up its publications and the connect it had accepted, and keeps
/// the sessions.
#[test]
fn a_new_leader_takes_up_the_sessions_its_log_holds() {
    let (mut old, mut old_world) = leading(2);
    let mut new = Sessions::new(1, ENDPOINTS);
    let mut new_world = World::new();
    // The follower reads the log the leader wrote.
    new_world.log = old_world.log.clone();
    work(&mut new, &mut new_world, 0, follower(1, 0));
    assert_eq!(commit(&mut new, &mut new_world, 0), [true, true]);
    assert_eq!((new.open(), new_world.held().len()), (2, 0));
    assert_eq!(new.counts().opened, 2);

    // The old leader had accepted a third connect, not committed, and stops leading.
    let third = channel(20_002);
    ingress(
        &mut old,
        &mut old_world,
        8_000,
        leader(1),
        &connect(3, &third),
    );
    assert_eq!(old_world.held(), [0, 1, 2]);
    work(&mut old, &mut old_world, 8_001, follower(1, -1));
    assert_eq!(old_world.held(), [] as [usize; 0]);
    assert_eq!((old.open(), old.counts().connects_dropped), (2, 1));

    // The follower leads term 2 from 9 s: publications, and a new leader event each.
    let lead = Leadership {
        leads: true,
        leadership_term_id: 2,
        leader_member_id: 1,
    };
    work(&mut new, &mut new_world, 9_000, lead);
    assert_eq!(new_world.held(), [0, 1]);
    assert_eq!(
        new_world.egress[1].as_deref(),
        Some(channel(20_001).as_slice())
    );
    let endpoints = String::from_utf8_lossy(ENDPOINTS).into_owned();
    assert_eq!(
        new_world.sent(),
        [
            format!("0: new leader session 0 term 2 leader 1 endpoints {endpoints:?}"),
            format!("1: new leader session 1 term 2 leader 1 endpoints {endpoints:?}"),
        ]
    );
    // A session open event of the term before, committed only now that this leader's
    // first entry is: the session is taken up as the two the election found open.
    let late = channel(20_009);
    let Ok(known) = new.on_entry(
        at(9_500),
        &Message::SessionOpenEvent {
            leadership_term_id: 1,
            correlation_id: 9,
            cluster_session_id: 5,
            timestamp: TIME,
            response_stream_id: EGRESS_STREAM_ID,
            response_channel: &late,
            encoded_principal: &[],
        },
        &mut new_world,
    );
    assert!(known);
    assert_eq!(new_world.held(), [0, 1, 2]);
    work(&mut new, &mut new_world, 9_501, lead);
    assert_eq!(
        new_world.sent(),
        [format!(
            "2: new leader session 5 term 2 leader 1 endpoints {endpoints:?}"
        )],
        "no ok event: this leader did not accept its connect"
    );
    let Ok(known) = new.on_entry(
        at(9_502),
        &Message::SessionCloseEvent {
            leadership_term_id: 2,
            cluster_session_id: 5,
            timestamp: TIME,
            close_reason: CloseReason::ClientAction,
        },
        &mut new_world,
    );
    assert!(known);
    assert_eq!(new_world.held(), [0, 1]);
    // The timeout runs from the election's close, not from the sessions' last
    // message under the old leader: nothing at 18.999 s, both at 19 s.
    work(&mut new, &mut new_world, 18_999, lead);
    assert_eq!(new_world.log.len(), 2);
    // A message with the old term is ignored; one with the new term is appended,
    // with the next session's id still the next after the log's.
    let message = |term: i64| Message::SessionMessage {
        leadership_term_id: term,
        cluster_session_id: 0,
        timestamp: 0,
        payload: b"again",
    };
    ingress(&mut new, &mut new_world, 18_999, lead, &message(1));
    assert_eq!(new.counts().ingress_ignored, 1);
    ingress(&mut new, &mut new_world, 18_999, lead, &message(2));
    assert_eq!(
        new_world.entries(2),
        ["message session 0 term 2 at 18999 \"again\""]
    );
    work(&mut new, &mut new_world, 19_000, lead);
    assert_eq!(
        new_world.entries(3),
        ["close session 1 term 2 at 19000 Timeout"]
    );
    let fourth = channel(20_003);
    ingress(&mut new, &mut new_world, 19_001, lead, &connect(4, &fourth));
    assert!(
        new_world.entries(4)[0].starts_with("open session 6 correlation 4 term 2"),
        "{:?}",
        new_world.entries(4)
    );
}

/// The entries no leader of this version writes: a session open event for a session
/// that is open, one that finds the table full, and a session close event for a
/// session that is not open. Nothing is done with one, and the caller is told.
#[test]
fn an_entry_the_table_cannot_hold_is_reported_and_changes_nothing() {
    let (mut sessions, mut world) = leading(MAX_SESSIONS);
    let before = sessions.counts();
    let response = channel(30_000);
    let open = |id: i64| Message::SessionOpenEvent {
        leadership_term_id: 1,
        correlation_id: 0,
        cluster_session_id: id,
        timestamp: TIME,
        response_stream_id: EGRESS_STREAM_ID,
        response_channel: &response,
        encoded_principal: &[],
    };
    for entry in [
        open(3),
        open(MAX_SESSIONS as i64),
        Message::SessionCloseEvent {
            leadership_term_id: 1,
            cluster_session_id: 99,
            timestamp: TIME,
            close_reason: CloseReason::ClientAction,
        },
    ] {
        let Ok(known) = sessions.on_entry(at(1), &entry, &mut world);
        assert!(!known, "{entry:?}");
    }
    assert_eq!(sessions.open(), MAX_SESSIONS);
    assert_eq!(sessions.counts(), before);
    // An entry that is none of the sessions' is passed.
    let Ok(known) = sessions.on_entry(
        at(1),
        &Message::SessionMessage {
            leadership_term_id: 1,
            cluster_session_id: 0,
            timestamp: TIME,
            payload: b"x",
        },
        &mut world,
    );
    assert!(known);
    // And the id a later leader gives is past every id the log has named.
    let Ok(_) = sessions.on_entry(
        at(1),
        &Message::SessionCloseEvent {
            leadership_term_id: 1,
            cluster_session_id: 0,
            timestamp: TIME,
            close_reason: CloseReason::Timeout,
        },
        &mut world,
    );
    ingress(
        &mut sessions,
        &mut world,
        2,
        leader(1),
        &connect(1, &response),
    );
    assert!(
        world.entries(MAX_SESSIONS)[0].starts_with("open session 11 "),
        "{:?}",
        world.entries(MAX_SESSIONS)
    );
    assert_eq!(EventCode::Ok as i32, 0);
}
