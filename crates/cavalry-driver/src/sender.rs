//! The sender: the driver's fourth thread, which puts every `cavalry:udp` publication's
//! log on the wire by the rules of `spec/udp.md`, "The sender" (ADR 0039, ADR 0040, ADR
//! 0041). The conductor opens each send endpoint's socket on the control plane and hands
//! it over through a bounded queue with every publication that sends on it, a reader on
//! the publication's log and its two counters; the sender answers through a second
//! queue with the states only it can see, connected, disconnected and closed, and the
//! conductor acts on them. On each duty cycle the sender reads its clock once, handles a
//! bounded number of requests, polls every endpoint's socket for the status messages,
//! NAKs and RTT measurements that arrived, and visits every publication: a retransmit in
//! progress, a setup frame when one is due, one burst of whole committed frames from the
//! sender position up to the sender limit in one socket call, a heartbeat when nothing
//! was sent for the heartbeat interval, the connection timeout, and the end of the
//! linger. Nothing here allocates per cycle, locks, logs or reads the clock per message;
//! the syscalls are the endpoint's own sends and receives, one per burst on Linux, one
//! per burst and destination on a control channel, whose endpoint sends every datagram
//! to each destination a client added, and whose `min` publications keep a bounded
//! table of receivers by receiver id for the slowest one's pace (ADR 0053). The
//! sender is built on its own thread, since the headers its batched calls fill
//! ([`Headers`]) hold raw pointers and cannot be sent to it (ADR 0041, acceptance).

use std::collections::VecDeque;
use std::io::IoSlice;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use cavalry_proto::channel::FlowControl;
use cavalry_proto::frame::FRAME_ALIGNMENT as FRAME_ALIGNMENT_BYTES;
use cavalry_proto::frame::Flags;
use cavalry_proto::udp::{
    CONNECTION_TIMEOUT, ControlFrame, DATAGRAMS_PER_BURST, FLOW_CONTROL_RECEIVER_TIMEOUT,
    HEARTBEAT_INTERVAL, Heartbeat, LINGER, MAX_DESTINATIONS, MAX_FLOW_CONTROL_RECEIVERS,
    MAX_TERM_LENGTH, Nak, RETRANSMIT_LINGER, RttMeasurement, SETUP_INTERVAL, SetupFrame,
    StatusMessage, parse, retransmit_window, sender_limit,
};
use cavalry_shm::counters::generated::{
    LOG_BUFFER_REJECTED_FRAME, LOG_BUFFER_REJECTED_POSITION, PUBLICATION_CONNECTED,
    PUBLICATION_DISCONNECTED, RECEIVER_RECEIVE_FAILED, SENDER_BYTES_SENT,
    SENDER_FLOW_CONTROL_LIMITED, SENDER_FLOW_CONTROL_RECEIVERS_FULL,
    SENDER_FLOW_CONTROL_RECEIVERS_TIMED_OUT, SENDER_FRAMES_SENT, SENDER_HEARTBEATS_SENT,
    SENDER_NAKS_IGNORED, SENDER_NAKS_RECEIVED, SENDER_RETRANSMIT_OVERFLOW, SENDER_RETRANSMITS_SENT,
    SENDER_RETRANSMITTED_BYTES, SENDER_RTT_REPLIES_SENT, SENDER_SEND_FAILED, SENDER_SETUPS_SENT,
    SENDER_SHORT_SENDS, SENDER_STATUS_MESSAGES_RECEIVED, SENDER_STATUS_MESSAGES_REJECTED,
};
use cavalry_shm::counters::{Counter, CounterId, CountersError};
use cavalry_shm::log::{Positions, TermReader};
use cavalry_shm::spsc::{Consumer, Producer};
use nix::sys::socket::SockaddrStorage;

use crate::conductor::now_nanos;
use crate::idle::{IdleStrategy, Idler};
use crate::socket::{Headers, SendSocket, Sent};
use crate::spec;

/// A heartbeat's flags: BEGIN and END (`spec/udp.md`, "Heartbeat"; `spec/frame.md`,
/// "Flags").
const HEARTBEAT_FLAGS: u8 = Flags::UNFRAGMENTED.bits();

/// The end of stream flag a heartbeat carries once the publication is closed and the
/// sender position has reached the end of stream position.
const END_OF_STREAM_FLAG: u8 = Flags::EOS.bits();

/// How long the sender parks while it holds no endpoint and no publication: the
/// conductor unparks it with every request, so a driver that sends nothing wakes no
/// CPU for the sender, as it wakes none for a replayer with no replay (ADR 0030).
const IDLE_PARK: Duration = Duration::from_secs(1);

/// How many replies the sender holds back when the reply queue is full: a connection
/// change and a close per publication it may hold, a client's or a replay's
/// (`spec::MAX_SENDER_PUBLICATIONS`; ADR 0043), a close per endpoint, and one per
/// request handled in a cycle. With the pending replies at this bound the sender
/// handles no request and moves no publication's connection state until the conductor
/// takes replies again; it keeps sending (ADR 0041).
pub(crate) const PENDING_REPLIES: usize =
    2 * spec::MAX_SENDER_PUBLICATIONS + spec::MAX_SEND_ENDPOINTS + spec::SENDER_REQUESTS_PER_CYCLE;

/// What `add publication` on a `cavalry:udp` channel fixed about the publication,
/// carried to the sender once: the log's sizes, a reader on it at the sender position,
/// and the two counters the sender moves.
pub(crate) struct AddPublication {
    /// The publication's registration id, which names its log.
    pub registration_id: i64,
    /// The send endpoint it sends on: the registration id of the command that opened it.
    pub endpoint_id: i64,
    /// The publication's session id.
    pub session_id: i32,
    /// The publication's stream id.
    pub stream_id: i32,
    /// The log's initial term id.
    pub initial_term_id: i32,
    /// The log's term length.
    pub term_length: u32,
    /// The publication's MTU length: the longest datagram.
    pub mtu_length: u32,
    /// A reader on the publication's log at the sender position; it keeps the mapping
    /// alive on the sender's thread.
    pub reader: TermReader,
    /// Its `sender.position` counter.
    pub position: Counter,
    /// Its `sender.limit` counter.
    pub limit: Counter,
    /// The flow control strategy, the channel's `fc` (`spec/udp.md`, "The sender";
    /// ADR 0053).
    pub flow_control: FlowControl,
    /// Its `sender.flow_control_receivers` gauge: the receivers in its `min` table.
    pub receivers: Counter,
    /// The `min` table, allocated by the conductor with room for
    /// [`cavalry_proto::udp::MAX_FLOW_CONTROL_RECEIVERS`] and empty, or with no room
    /// under `max`; the sender never grows it.
    pub receiver_table: Vec<Receiver>,
    /// Its `sender.bytes_unsent` counter, which a publication on a channel that names
    /// `ssc=true` has and no other (`spec/udp.md`, "The sender"; ADR 0098).
    pub unsent: Option<Counter>,
    /// Its `sender.joins_refused` counter, which a publication on a channel that names
    /// `ssc=true` has and no other (`spec/udp.md`, "The sender"; ADR 0099).
    pub joins_refused: Option<Counter>,
}

/// One receiver in a `min` publication's table, by its receiver id: the position plus
/// window of its last accepted status message and when it arrived (`spec/udp.md`, "The
/// sender"; ADR 0053).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Receiver {
    receiver_id: i64,
    /// Its consumption position plus its receiver window length: what it lets the
    /// sender send up to.
    limit: i64,
    last_status_message_at: i64,
    /// Its last status message carried the end-of-stream flag: dropped on the next
    /// sweep, and not counted as a timeout (ADR 0054); once the publication is closed,
    /// kept until the linger ends (ADR 0056).
    end_of_stream: bool,
}

/// What the conductor asks the sender.
pub(crate) enum Request {
    /// A send endpoint the conductor opened: its socket moves to the sender.
    AddEndpoint {
        /// The endpoint's registration id: the correlation id of the command that
        /// opened it, which keys its `channel.status` counter.
        registration_id: i64,
        /// The socket, non-blocking: connected to the endpoint, or bound to a control
        /// channel's control address and connected to nothing.
        socket: SendSocket,
        /// A control channel's destinations table, allocated by the conductor with
        /// room for [`cavalry_proto::udp::MAX_DESTINATIONS`] and empty; `None` for a
        /// channel with an endpoint (ADR 0053).
        destinations: Option<Vec<SockaddrStorage>>,
    },
    /// A destination for a control channel's endpoint, resolved and checked by the
    /// conductor against the table's bound and the ones it holds: the sender sends every
    /// datagram of the endpoint's publications to it from the next cycle.
    AddDestination {
        /// The endpoint.
        registration_id: i64,
        /// The destination's address, which the sender keeps as the socket's kind.
        address: std::net::SocketAddr,
    },
    /// A destination removed from a control channel's endpoint.
    RemoveDestination {
        /// The endpoint.
        registration_id: i64,
        /// The destination's address, as it was added.
        address: std::net::SocketAddr,
    },
    /// Close an endpoint whose last publication's linger ended: the socket is dropped;
    /// answered with [`Reply::EndpointClosed`].
    RemoveEndpoint {
        /// The endpoint.
        registration_id: i64,
    },
    /// A publication to put on the wire.
    AddPublication(AddPublication),
    /// The publication was closed: the sender sends what is left, heartbeats with EOS
    /// once the sender position reaches the end, answers NAKs through the linger, and
    /// answers with [`Reply::Closed`] when the linger passes, or on a channel with an
    /// endpoint when a status message with the end of stream flag arrives first (ADR
    /// 0054).
    Close {
        /// The publication.
        registration_id: i64,
        /// The end of stream position the conductor stored.
        end_of_stream_position: i64,
    },
    /// The recording a publication on a channel that names `ssc=true` follows while no
    /// receiver answers, or none once that recording has stopped (`spec/udp.md`, "The
    /// sender"; ADR 0098). A `None` is answered with [`Reply::Unfollowed`], whether or
    /// not the sender still holds the publication, so that the conductor frees the
    /// counter only after the sender has let it go.
    Follow {
        /// The publication.
        registration_id: i64,
        /// The recording's `recording.position` counter.
        recording: Option<Counter>,
    },
}

/// What the sender answers: the states only it can see.
pub(crate) enum Reply {
    /// The publication's first status message without the send-setup flag arrived, or
    /// one arrived again after the connection timeout had passed.
    Connected {
        /// The publication.
        registration_id: i64,
    },
    /// The connection timeout passed with no status message.
    Disconnected {
        /// The publication.
        registration_id: i64,
    },
    /// The publication's linger ended; the sender dropped its reader and its counters.
    Closed {
        /// The publication.
        registration_id: i64,
    },
    /// The endpoint's socket is closed.
    EndpointClosed {
        /// The endpoint.
        registration_id: i64,
    },
    /// The sender holds no recording's counter for the publication any more: the answer
    /// to a [`Request::Follow`] with no recording.
    Unfollowed {
        /// The publication.
        registration_id: i64,
    },
}

/// The static counters the sender moves (`spec/udp.md`, "Counters").
pub(crate) struct Statics {
    bytes_sent: Counter,
    frames_sent: Counter,
    setups_sent: Counter,
    heartbeats_sent: Counter,
    retransmits_sent: Counter,
    retransmitted_bytes: Counter,
    retransmit_overflow: Counter,
    short_sends: Counter,
    send_failed: Counter,
    flow_control_limited: Counter,
    naks_received: Counter,
    naks_ignored: Counter,
    status_messages_received: Counter,
    status_messages_rejected: Counter,
    rtt_replies_sent: Counter,
    flow_control_receivers_timed_out: Counter,
    flow_control_receivers_full: Counter,
    publication_connected: Counter,
    publication_disconnected: Counter,
    receive_failed: Counter,
    rejected_frame: Counter,
    rejected_position: Counter,
}

impl Statics {
    /// The sender's counters, each taken from the counters file through `counter`.
    pub(crate) fn new(
        counter: impl Fn(CounterId) -> Result<Counter, CountersError>,
    ) -> Result<Self, CountersError> {
        Ok(Self {
            bytes_sent: counter(SENDER_BYTES_SENT)?,
            frames_sent: counter(SENDER_FRAMES_SENT)?,
            setups_sent: counter(SENDER_SETUPS_SENT)?,
            heartbeats_sent: counter(SENDER_HEARTBEATS_SENT)?,
            retransmits_sent: counter(SENDER_RETRANSMITS_SENT)?,
            retransmitted_bytes: counter(SENDER_RETRANSMITTED_BYTES)?,
            retransmit_overflow: counter(SENDER_RETRANSMIT_OVERFLOW)?,
            short_sends: counter(SENDER_SHORT_SENDS)?,
            send_failed: counter(SENDER_SEND_FAILED)?,
            flow_control_limited: counter(SENDER_FLOW_CONTROL_LIMITED)?,
            naks_received: counter(SENDER_NAKS_RECEIVED)?,
            naks_ignored: counter(SENDER_NAKS_IGNORED)?,
            status_messages_received: counter(SENDER_STATUS_MESSAGES_RECEIVED)?,
            status_messages_rejected: counter(SENDER_STATUS_MESSAGES_REJECTED)?,
            rtt_replies_sent: counter(SENDER_RTT_REPLIES_SENT)?,
            flow_control_receivers_timed_out: counter(SENDER_FLOW_CONTROL_RECEIVERS_TIMED_OUT)?,
            flow_control_receivers_full: counter(SENDER_FLOW_CONTROL_RECEIVERS_FULL)?,
            publication_connected: counter(PUBLICATION_CONNECTED)?,
            publication_disconnected: counter(PUBLICATION_DISCONNECTED)?,
            receive_failed: counter(RECEIVER_RECEIVE_FAILED)?,
            rejected_frame: counter(LOG_BUFFER_REJECTED_FRAME)?,
            rejected_position: counter(LOG_BUFFER_REJECTED_POSITION)?,
        })
    }

    /// Counts what the socket did with one control frame.
    fn count_one(&self, sent: std::io::Result<bool>, whole: &Counter) {
        match sent {
            Ok(true) => whole.increment(),
            Ok(false) => self.short_sends.increment(),
            Err(_) => self.send_failed.increment(),
        }
    }
}

/// Whether a receiver has answered the publication within the connection timeout
/// (`spec/udp.md`, "The sender"); every transition adds one to a counter, and
/// `counters.toml` lists them.
// state machine: publication_connection
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConnectionState {
    /// No status message without the send-setup flag within the connection timeout.
    Disconnected,
    /// One arrived within it: the publisher's limit runs a window ahead of the sender.
    Connected,
}

/// A NAK being answered: the range named, how far the answer has gone, and how long the
/// answer stays outstanding once it is complete.
#[derive(Clone, Copy, Debug)]
struct Retransmit {
    /// The first position of the range.
    start: i64,
    /// The position after the range.
    end: i64,
    /// The next position to send; `end` once the range went.
    next: i64,
    /// When the retransmit stops being outstanding: the retransmit linger after the last
    /// datagram; `i64::MAX` while the range is still going.
    outstanding_until: i64,
}

/// One publication the sender puts on the wire.
struct Live {
    registration_id: i64,
    endpoint_id: i64,
    session_id: i32,
    stream_id: i32,
    positions: Positions,
    mtu_length: usize,
    reader: TermReader,
    position: Counter,
    limit: Counter,
    /// The sender position: the next byte to send, the reader's position.
    sender_position: i64,
    /// The sender limit: the last accepted consumption position plus its window.
    sender_limit: i64,
    connection: ConnectionState,
    /// Whether the first status message without the send-setup flag has arrived: until
    /// it has, setup frames go and heartbeats do not.
    answered: bool,
    /// A status message with the send-setup flag asked for a setup frame not yet sent.
    elicited: bool,
    last_setup_at: i64,
    last_status_message_at: i64,
    /// When a data frame or a heartbeat last went: the heartbeat clock.
    last_sent_at: i64,
    retransmit: Option<Retransmit>,
    /// The end of stream position once the publication is closed, and when it was.
    closing: Option<(i64, i64)>,
    /// A status message with the end of stream flag arrived after the close, on a
    /// channel with an endpoint; never set on a control channel, whose publication
    /// lingers for the linger timeout (ADR 0054).
    end_of_stream_acknowledged: bool,
    /// A frame to send met no window: `sender.flow_control_limited` counts the entry.
    limited: bool,
    /// The channel's `fc`: how the sender limit moves on status messages (ADR 0053).
    flow_control: FlowControl,
    /// The `min` table: empty with no room under `max`, so nothing below reads it.
    receivers: Vec<Receiver>,
    /// Its `sender.flow_control_receivers` gauge.
    receivers_gauge: Counter,
    /// The `recording.position` of the live recording of a publication whose channel
    /// names `ssc=true`: while no receiver answers, the sender position follows it and
    /// no data goes (`spec/udp.md`, "The sender"; ADR 0098). `None` for every other
    /// publication, and once the recording has stopped.
    recording: Option<Counter>,
    /// Its `sender.bytes_unsent`: what the sender position passed while following above
    /// `sent_position`, less what it went back over above `sent_position` when a
    /// receiver joined (ADR 0112).
    unsent: Option<Counter>,
    /// Its `sender.joins_refused`: the status messages of a receiver that would have
    /// joined it while it was alone with its recording, and named a position more than
    /// a term behind.
    joins_refused: Option<Counter>,
    /// The highest position a data burst took the sender position to: every byte below
    /// it was sent or was passed while following, and nothing at or above it was sent.
    /// `sender.bytes_unsent` counts no byte below it, so that a byte sent before the
    /// publication was alone is never counted as unsent nor taken off (ADR 0112).
    sent_position: i64,
    /// The last recording position the sender could not follow, counted once.
    refused_position: i64,
}

impl Live {
    /// Whether a receiver answers: the first status message without the send-setup
    /// flag has arrived, and one arrived within the connection timeout. The sender's
    /// own reading, which waits for no reply to the conductor.
    fn receiving(&self, now: i64) -> bool {
        self.answered
            && now.saturating_sub(self.last_status_message_at) < spec::nanos(CONNECTION_TIMEOUT)
    }

    /// Whether the publication is alone with its recording: a recording to follow and
    /// no receiver answering (`spec/udp.md`, "The sender"; ADR 0098).
    fn alone(&self, now: i64) -> bool {
        self.recording.is_some() && !self.receiving(now)
    }

    /// Moves the sender position up to the recording's position, for a publication
    /// alone with its recording, and returns whether it moved; `sender.bytes_unsent`
    /// counts the bytes passed above the highest position sent, the others having
    /// been sent already or counted already (ADR 0112). The recording's position is a
    /// frame boundary at most the producer position, which the publisher's limit holds
    /// within a term and a half of the sender position; one that is not a multiple of
    /// the frame alignment, or more than two terms ahead, is not this publication's
    /// recording's: it is counted once in `log_buffer.rejected_position` and the
    /// sender stays (`spec/udp.md`, "The sender"; ADR 0112).
    fn follow_recording(&mut self, statics: &Statics) -> bool {
        let Some(recording) = &self.recording else {
            return false;
        };
        let target = recording.load_acquire();
        if target <= self.sender_position {
            return false;
        }
        let term_length = i64::from(self.positions.term_length());
        if target % FRAME_ALIGNMENT_BYTES as i64 != 0
            || target - self.sender_position > 2 * term_length
        {
            if self.refused_position != target {
                self.refused_position = target;
                statics.rejected_position.increment();
            }
            return false;
        }
        let passed = target - self.sender_position.max(self.sent_position);
        if let Some(unsent) = &self.unsent
            && passed > 0
        {
            unsent.add(passed);
        }
        self.sender_position = target;
        self.position.store_release(target);
        true
    }
}

/// One send endpoint: its socket, on this thread from the conductor's add until its
/// remove, and on a control channel its destinations, in the order they were added
/// (`spec/udp.md`, "The sender"; ADR 0053).
struct Endpoint {
    registration_id: i64,
    socket: SendSocket,
    /// `Some` on a control channel, with room for the destinations bound and never
    /// grown here; `None` on a channel with an endpoint, where the socket is connected.
    destinations: Option<Vec<SockaddrStorage>>,
}

/// Why a send was refused (`spec/udp.md`, "The sender", step 2; ADR 0053).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Refusal {
    /// The socket's own: no room in its send buffer, `EAGAIN` or `ENOBUFS`, or an
    /// interrupted send, `EINTR`; holds the sender position for every destination, as
    /// a short send does, and on a channel with an endpoint.
    Socket,
    /// A destination's own: no route to it from the control address, an address that
    /// has gone; nobody waits for it.
    Destination,
}

impl Refusal {
    /// Which kind of refusal the operating system's error is: `EAGAIN`, `ENOBUFS` and
    /// `EINTR` are the socket's own, and every other error a destination's, such as
    /// `ENETUNREACH`, `EHOSTUNREACH` or the `EINVAL` of a loopback socket sending off
    /// the host.
    fn of(error: &std::io::Error) -> Self {
        let socket = matches!(
            error.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
        ) || error.raw_os_error() == Some(nix::libc::ENOBUFS);
        if socket {
            Refusal::Socket
        } else {
            Refusal::Destination
        }
    }
}

/// What a burst came to across an endpoint's targets ([`Endpoint::send_burst`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Delivery {
    /// Datagrams every target took whole, from the first: how far the sender position
    /// advances.
    whole: usize,
    /// Bytes put on the wire, every target's.
    bytes: usize,
    /// Frames put on the wire, every target's.
    frames: usize,
}

impl Delivery {
    /// A control channel's burst of `count` datagrams, folded over what each
    /// destination's socket call came to, in the order they were sent (`spec/udp.md`,
    /// "The sender", step 2; ADR 0053): `whole` is the fewest datagrams any destination
    /// took whole, so a short send holds the sender position for all; a refusal of the
    /// socket's own holds it at none of the burst for all; a refusal of a destination's
    /// own holds back no other destination, unless every destination was refused so,
    /// when the position holds as on an endpoint. The bytes and frames are those of the
    /// destinations that took any. With no destination every datagram counts as taken
    /// and nothing as on the wire, so that the sender position advances past the burst.
    fn fold(count: usize, outcomes: impl Iterator<Item = Result<Delivery, Refusal>>) -> Self {
        let mut delivery = Delivery {
            whole: count,
            bytes: 0,
            frames: 0,
        };
        let (mut destinations, mut refused) = (0usize, 0usize);
        for outcome in outcomes {
            destinations += 1;
            match outcome {
                Ok(one) => {
                    delivery.whole = delivery.whole.min(one.whole);
                    delivery.bytes += one.bytes;
                    delivery.frames += one.frames;
                }
                Err(Refusal::Socket) => delivery.whole = 0,
                Err(Refusal::Destination) => refused += 1,
            }
        }
        if destinations > 0 && refused == destinations {
            delivery.whole = 0;
        }
        delivery
    }
}

impl Endpoint {
    /// Sends a burst to the endpoint, or to each destination of a control channel in
    /// turn, and counts what each socket call did (`spec/udp.md`, "The sender", step
    /// 2; ADR 0053). `whole` is how far the sender position advances: the fewest
    /// datagrams any destination took whole, so a short send or a refusal of the
    /// socket's own holds the position for all and the destinations that took more get
    /// those again, a duplicate their receivers drop; a destination the socket refused
    /// for a reason of its own is not waited for, its datagrams from the refused one on
    /// lost to it alone and NAKed back, unless every destination was refused so, when
    /// the position holds as on an endpoint. The bytes and frames are every
    /// destination's, as the counters count them. With no destination nothing is sent
    /// and every datagram counts as taken, so that the sender position advances past
    /// the burst and nothing is counted as on the wire. [`Delivery::fold`] is the
    /// rule, over what each destination's call came to.
    fn send_burst(
        &self,
        headers: &mut Headers,
        datagrams: &[[IoSlice<'_>; 1]],
        frames: &[usize],
        statics: &Statics,
    ) -> Delivery {
        let count = datagrams.len();
        let taken = |whole: usize| Delivery {
            whole,
            bytes: datagrams[..whole].iter().map(|d| d[0].len()).sum(),
            frames: frames[..whole.min(frames.len())].iter().sum(),
        };
        match &self.destinations {
            None => taken(
                Self::count(
                    self.socket.send_burst(headers, datagrams, None),
                    count,
                    statics,
                )
                .unwrap_or(0),
            ),
            Some(destinations) => Delivery::fold(
                count,
                destinations.iter().map(|to| {
                    Self::count(
                        self.socket.send_burst(headers, datagrams, Some(to)),
                        count,
                        statics,
                    )
                    .map(taken)
                }),
            ),
        }
    }

    /// How many datagrams one socket call took whole, at most `count`, with a short
    /// send and a refusal counted, or which kind of refusal it was.
    fn count(
        sent: std::io::Result<Sent>,
        count: usize,
        statics: &Statics,
    ) -> Result<usize, Refusal> {
        match sent {
            Ok(sent) => {
                if sent.short {
                    statics.short_sends.increment();
                }
                if sent.failed {
                    statics.send_failed.increment();
                }
                Ok(sent.whole.min(count))
            }
            Err(error) => {
                statics.send_failed.increment();
                Err(Refusal::of(&error))
            }
        }
    }

    /// Sends one control frame: to the endpoint; to `to` when the frame answers one
    /// datagram, an RTT measurement's, on a control channel; else to each destination
    /// in turn. `counter` counts each send the socket took whole, so a setup frame to
    /// three destinations counts three and one to none counts nothing.
    fn send_one(
        &self,
        headers: &mut Headers,
        bytes: &[u8],
        to: Option<&SockaddrStorage>,
        statics: &Statics,
        counter: &Counter,
    ) {
        match (&self.destinations, to) {
            // A connected socket sends to its endpoint alone, whatever the answer's
            // source: the endpoint is the source.
            (None, _) => statics.count_one(self.socket.send_one(headers, bytes, None), counter),
            (Some(_), Some(to)) => {
                statics.count_one(self.socket.send_one(headers, bytes, Some(to)), counter);
            }
            (Some(destinations), None) => {
                for to in destinations {
                    statics.count_one(self.socket.send_one(headers, bytes, Some(to)), counter);
                }
            }
        }
    }
}

/// What one burst gathered: the datagrams and what each one is in the stream.
struct Burst<'a> {
    datagrams: [[IoSlice<'a>; 1]; DATAGRAMS_PER_BURST],
    /// Each datagram's length in the stream: its bytes, or a padding frame's aligned
    /// length.
    lengths: [usize; DATAGRAMS_PER_BURST],
    /// Each datagram's frames.
    frames: [usize; DATAGRAMS_PER_BURST],
    count: usize,
    /// A committed frame was found that the window does not cover.
    limited: bool,
}

impl<'a> Burst<'a> {
    /// Gathers whole committed frames from `from` into datagrams of at most `mtu`, up to
    /// `room` bytes in all, [`DATAGRAMS_PER_BURST`] datagrams, or the end of `from`'s
    /// term, whichever comes first; a position past `to`, when one is given, ends it
    /// too. A padding frame goes as its 32-byte header, counted whether or not it is
    /// sent, so the window does not bound it. A frame that fails a check ends the burst
    /// before it and is counted in `log_buffer.rejected_frame`.
    fn gather(
        reader: &'a TermReader,
        positions: Positions,
        from: i64,
        room: i64,
        to: Option<i64>,
        mtu: usize,
        statics: &Statics,
    ) -> Self {
        let mut burst = Burst {
            datagrams: [[IoSlice::new(&[]); 1]; DATAGRAMS_PER_BURST],
            lengths: [0; DATAGRAMS_PER_BURST],
            frames: [0; DATAGRAMS_PER_BURST],
            count: 0,
            limited: false,
        };
        let term_end =
            from - i64::from(positions.term_offset_of(from)) + i64::from(positions.term_length());
        let mut position = from;
        let mut room = room;
        while burst.count < DATAGRAMS_PER_BURST && position < term_end && room > 0 {
            if let Some(to) = to
                && position >= to
            {
                break;
            }
            let limit = usize::try_from(room.min(to.map_or(room, |to| to - position)))
                .map_or(mtu, |room| room.min(mtu));
            let block = match reader.block_from(position, limit) {
                Ok(block) => block,
                Err(_) => {
                    statics.rejected_frame.increment();
                    break;
                }
            };
            if block.is_empty() {
                break;
            }
            let length = block.length as i64;
            // The block was scanned under `limit`, at most `room`, and the scan always
            // takes the first committed frame, so a block longer than `room` is one
            // frame the window does not cover whole: it waits for the next status
            // message, which the status message interval bounds, and the burst is
            // limited at it. A window that is not a multiple of the frame length
            // leaves that remainder unspent until then.
            if !block.padding && length > room {
                burst.limited = true;
                break;
            }
            burst.datagrams[burst.count] = [IoSlice::new(block.bytes)];
            burst.lengths[burst.count] = block.length;
            burst.frames[burst.count] = block.frames;
            burst.count += 1;
            position += length;
            room -= length;
        }
        burst
    }
}

/// The sender's state: the queues, the endpoints, the publications, the counters and
/// the headers every endpoint's batched calls fill. Not `Send`, because of the headers:
/// [`Sender::new`] runs on the sender's own thread, and the tests that drive a sender
/// by hand build it on theirs.
pub(crate) struct Sender {
    requests: Consumer<Request>,
    replies: Producer<Reply>,
    /// Replies the reply queue had no room for, offered again next cycle.
    pending: VecDeque<Reply>,
    endpoints: Vec<Endpoint>,
    live: Vec<Live>,
    statics: Statics,
    /// The headers `sendmmsg` and `recvmmsg` fill, lent to every endpoint's call: the
    /// sender uses one socket at a time.
    headers: Headers,
    idler: Idler,
}

impl Sender {
    /// A sender over the queues to and from the conductor, with its counters; the
    /// headers for a burst and a poll are preallocated here, on the thread that calls
    /// this, which must be the one that runs the sender.
    pub(crate) fn new(
        requests: Consumer<Request>,
        replies: Producer<Reply>,
        statics: Statics,
        idle: IdleStrategy,
    ) -> Self {
        Self {
            requests,
            replies,
            pending: VecDeque::with_capacity(PENDING_REPLIES),
            endpoints: Vec::with_capacity(spec::MAX_SEND_ENDPOINTS),
            live: Vec::with_capacity(spec::MAX_SENDER_PUBLICATIONS),
            statics,
            headers: Headers::preallocate(),
            idler: Idler::new(idle),
        }
    }

    /// The sender position and the sender limit of a publication, for the tests.
    #[cfg(test)]
    pub(crate) fn positions_of(&self, registration_id: i64) -> Option<(i64, i64)> {
        self.live
            .iter()
            .find(|live| live.registration_id == registration_id)
            .map(|live| (live.sender_position, live.sender_limit))
    }

    /// How many replies the sender holds back, for the tests.
    #[cfg(test)]
    pub(crate) fn pending_replies(&self) -> usize {
        self.pending.len()
    }

    /// The sender's loop: duty cycles until `stop`, idling with the strategy while it
    /// holds an endpoint or a publication and parked while it holds none. The sockets
    /// close with the sender; a receiver sees the heartbeats stop and times its images
    /// out.
    pub(crate) fn run(mut self, stop: &AtomicBool) {
        while !stop.load(Ordering::Acquire) {
            let now = now_nanos();
            if self.duty_cycle(now) != 0 {
                self.idler.reset();
            } else if self.endpoints.is_empty() && self.live.is_empty() && self.pending.is_empty() {
                // Nothing can change until the conductor offers a request, and it
                // unparks this thread when it does; an unpark that came before this
                // park is a token the park consumes at once, so no request waits for
                // the timeout. `Driver::close` unparks it too, after storing `stop`.
                std::thread::park_timeout(IDLE_PARK);
                self.idler.reset();
            } else {
                self.idler.idle();
            }
        }
        self.flush_replies();
    }

    /// One duty cycle; returns how much work it found, 0 when the sender may idle. The
    /// clock is read once by the caller and passed down.
    pub(crate) fn duty_cycle(&mut self, now: i64) -> usize {
        let mut work = self.flush_replies();
        for _ in 0..spec::SENDER_REQUESTS_PER_CYCLE {
            if self.pending.len() >= PENDING_REPLIES {
                break;
            }
            let Some(request) = self.requests.poll() else {
                break;
            };
            self.handle(request, now);
            work += 1;
        }
        for index in 0..self.endpoints.len() {
            work += self.poll_endpoint(index, now);
        }
        let mut index = 0;
        while index < self.live.len() {
            let (did, removed) = self.serve(index, now);
            work += did;
            if !removed {
                index += 1;
            }
        }
        work
    }

    fn flush_replies(&mut self) -> usize {
        let mut sent = 0;
        while let Some(reply) = self.pending.pop_front() {
            match self.replies.offer(reply) {
                Ok(()) => sent += 1,
                Err(reply) => {
                    self.pending.push_front(reply);
                    break;
                }
            }
        }
        sent
    }

    /// Queues a reply: into the queue, or into `pending` when it is full. Callers check
    /// [`PENDING_REPLIES`] first, so the bound holds.
    fn reply(&mut self, reply: Reply) {
        let reply = if self.pending.is_empty() {
            match self.replies.offer(reply) {
                Ok(()) => return,
                Err(reply) => reply,
            }
        } else {
            reply
        };
        debug_assert!(
            self.pending.len() < PENDING_REPLIES,
            "the pending replies passed their bound"
        );
        self.pending.push_back(reply);
    }

    fn handle(&mut self, request: Request, now: i64) {
        match request {
            Request::AddEndpoint {
                registration_id,
                socket,
                destinations,
            } => {
                debug_assert!(self.endpoints.len() < spec::MAX_SEND_ENDPOINTS);
                self.endpoints.push(Endpoint {
                    registration_id,
                    socket,
                    destinations,
                });
            }
            // The conductor checked the bound and the endpoints held, as written: two
            // writings of one address are two destinations (`spec/client-protocol.md`,
            // "Commands", add destination). The table holds the spec's bound and never
            // grows past the room it came with, at least that bound, so the push
            // allocates nothing.
            Request::AddDestination {
                registration_id,
                address,
            } => {
                if let Some(endpoint) = self
                    .endpoints
                    .iter_mut()
                    .find(|endpoint| endpoint.registration_id == registration_id)
                    && let Some(destinations) = &mut endpoint.destinations
                    && destinations.len() < MAX_DESTINATIONS.min(destinations.capacity())
                {
                    destinations.push(SockaddrStorage::from(address));
                }
            }
            Request::RemoveDestination {
                registration_id,
                address,
            } => {
                let address = SockaddrStorage::from(address);
                if let Some(endpoint) = self
                    .endpoints
                    .iter_mut()
                    .find(|endpoint| endpoint.registration_id == registration_id)
                    && let Some(destinations) = &mut endpoint.destinations
                    && let Some(index) = destinations.iter().position(|held| *held == address)
                {
                    // Shifts the ones after it: the order is the sending order.
                    destinations.remove(index);
                }
            }
            Request::RemoveEndpoint { registration_id } => {
                if let Some(index) = self
                    .endpoints
                    .iter()
                    .position(|endpoint| endpoint.registration_id == registration_id)
                {
                    drop(self.endpoints.swap_remove(index));
                }
                self.reply(Reply::EndpointClosed { registration_id });
            }
            Request::AddPublication(add) => {
                debug_assert!(self.live.len() < spec::MAX_SENDER_PUBLICATIONS);
                let sender_position = add.reader.position();
                add.position.store_release(sender_position);
                add.limit.store_release(0);
                add.receivers.set(0);
                self.live.push(Live {
                    registration_id: add.registration_id,
                    endpoint_id: add.endpoint_id,
                    session_id: add.session_id,
                    stream_id: add.stream_id,
                    positions: Positions::new(add.initial_term_id, add.term_length),
                    mtu_length: add.mtu_length as usize,
                    reader: add.reader,
                    position: add.position,
                    limit: add.limit,
                    sender_position,
                    sender_limit: 0,
                    connection: ConnectionState::Disconnected,
                    answered: false,
                    elicited: false,
                    // Due at once: the first setup frame goes on this cycle.
                    last_setup_at: now.saturating_sub(spec::nanos(SETUP_INTERVAL)),
                    last_status_message_at: now,
                    last_sent_at: now,
                    retransmit: None,
                    closing: None,
                    end_of_stream_acknowledged: false,
                    limited: false,
                    flow_control: add.flow_control,
                    receivers: add.receiver_table,
                    receivers_gauge: add.receivers,
                    recording: None,
                    unsent: add.unsent,
                    joins_refused: add.joins_refused,
                    sent_position: sender_position,
                    refused_position: -1,
                });
            }
            Request::Close {
                registration_id,
                end_of_stream_position,
            } => {
                match self
                    .live
                    .iter_mut()
                    .find(|live| live.registration_id == registration_id)
                {
                    Some(live) => {
                        if live.closing.is_none() {
                            live.closing = Some((end_of_stream_position, now));
                        }
                    }
                    // A publication this sender never held, or one whose linger already
                    // ended: closed at once, so the conductor's close completes, as a
                    // remove of an endpoint it does not hold is answered.
                    None => self.reply(Reply::Closed { registration_id }),
                }
            }
            Request::Follow {
                registration_id,
                recording,
            } => {
                let live = self
                    .live
                    .iter_mut()
                    .find(|live| live.registration_id == registration_id);
                match recording {
                    // A publication the sender no longer holds follows nothing: the
                    // handle is dropped here.
                    Some(recording) => {
                        if let Some(live) = live {
                            live.recording = Some(recording);
                        }
                    }
                    // The recording stopped: one last look, so that a publication alone
                    // with it ends where the recording ended, then the counter goes.
                    None => {
                        if let Some(live) = live {
                            if live.alone(now) {
                                live.follow_recording(&self.statics);
                            }
                            live.recording = None;
                        }
                        self.reply(Reply::Unfollowed { registration_id });
                    }
                }
            }
        }
    }

    /// Takes every control frame that arrived at the endpoint and acts on it; returns
    /// how many arrived. A receive the operating system refused is counted in
    /// `receiver.receive_failed` and retried on the next cycle, and is not work: a
    /// socket that refuses every poll would otherwise keep the sender from idling
    /// under the backoff strategy, a core spent on a broken socket.
    fn poll_endpoint(&mut self, index: usize, now: i64) -> usize {
        let count = match self.endpoints[index].socket.receive(&mut self.headers) {
            Ok(count) => count,
            Err(_) => {
                self.statics.receive_failed.increment();
                return 0;
            }
        };
        for datagram in 0..count {
            // The frame's shape against the largest term this version allows; its offset
            // is held against the publication's own term once the frame has found one,
            // since a replay's publication keeps its recording's term (ADR 0043), as the
            // receiver holds a heartbeat's against the image's (ADR 0042).
            let frame = parse(
                self.endpoints[index].socket.datagram(datagram),
                MAX_TERM_LENGTH,
            );
            match frame {
                Ok(ControlFrame::StatusMessage(message)) => {
                    self.on_status_message(index, message, now);
                }
                Ok(ControlFrame::Nak(nak)) => self.on_nak(index, nak),
                Ok(ControlFrame::RttMeasurement(measurement)) => {
                    self.on_rtt_measurement(index, measurement, datagram);
                }
                // A setup frame, a heartbeat or data at a send endpoint, or a frame this
                // version does not read: nothing of it is a sender's business.
                Ok(ControlFrame::Setup(_) | ControlFrame::Heartbeat(_) | ControlFrame::Data)
                | Err(_) => {}
            }
        }
        count
    }

    fn find(&self, endpoint_index: usize, session_id: i32, stream_id: i32) -> Option<usize> {
        let endpoint_id = self.endpoints.get(endpoint_index)?.registration_id;
        self.live.iter().position(|live| {
            live.endpoint_id == endpoint_id
                && live.session_id == session_id
                && live.stream_id == stream_id
        })
    }

    /// A status message (`spec/udp.md`, "The sender"): one with the send-setup flag asks
    /// for a setup frame and nothing else; one without it moves the sender limit when
    /// its position lies within the window around the sender position, and connects the
    /// publication; one with the end-of-stream flag after the close ends the linger on
    /// a channel with an endpoint and not on a control channel (ADR 0054).
    fn on_status_message(&mut self, endpoint_index: usize, message: StatusMessage, now: i64) {
        let Some(index) = self.find(endpoint_index, message.session_id, message.stream_id) else {
            return;
        };
        let control = self
            .endpoints
            .get(endpoint_index)
            .is_some_and(|endpoint| endpoint.destinations.is_some());
        self.statics.status_messages_received.increment();
        let live = &mut self.live[index];
        // An offset the publication's term does not hold: rejected as one outside the
        // window is (`spec/udp.md`, "The sender"; ADR 0043).
        if message.check(live.positions.term_length()).is_err() {
            self.statics.status_messages_rejected.increment();
            return;
        }
        if message.asks_for_setup() {
            if live
                .closing
                .is_some_and(|(end, _)| live.sender_position >= end)
            {
                self.statics.status_messages_rejected.increment();
            } else {
                live.elicited = true;
            }
            return;
        }
        // A publication alone with its recording has moved its sender position with
        // the recorder's and sent nothing (`spec/udp.md`, "The sender"; ADR 0098). The
        // receiver this status message is from stands where a setup frame, or the end
        // of an earlier connection, left it: at most a term behind, the sender goes
        // back to it and sends from there, so the receiver has nothing to NAK; the
        // term behind the sender position is never cleaned. Of the bytes gone back
        // over, those above the highest position sent were passed while alone and
        // are now to be sent, and come off `sender.bytes_unsent`; those below it went
        // to a receiver before, the one that comes back among them, and were never
        // counted (ADR 0112). Further behind, what it would need is no longer in the
        // log: rejected and counted in the publication's own `sender.joins_refused`,
        // and its image times out and joins again. On a publication no receiver has
        // joined, that is a publisher that writes more than a term between a setup
        // frame and its answer, one whose term is too short for it; on one whose
        // receiver left, more than a term went by while it was away (ADR 0099). The
        // limit is set anew from this receiver. A receiver ahead of the sender position,
        // whose image began where an earlier setup frame put it before the sender went
        // back for another, is no join to go back for: its status message is taken below
        // as any is, and it drops what the sender sends below its position as under-runs.
        if live.alone(now) {
            let position = cavalry_proto::udp::position(
                message.consumption_term_id,
                message.consumption_term_offset,
                live.positions.initial_term_id(),
                live.positions.term_length(),
            );
            let behind = live.sender_position.wrapping_sub(position);
            if behind > i64::from(live.positions.term_length()) {
                self.statics.status_messages_rejected.increment();
                if let Some(joins_refused) = &live.joins_refused {
                    joins_refused.add(1);
                }
                return;
            }
            if behind >= 0 {
                // At most a term: the position is within a term below the sender
                // position, so neither difference overflows.
                let to_send = live.sender_position - position.max(live.sent_position);
                if let Some(unsent) = &live.unsent
                    && to_send > 0
                {
                    unsent.add(-to_send);
                }
                live.sender_position = position;
                live.position.store_release(position);
                live.sender_limit = position;
                live.limit.store_release(position);
                live.limited = false;
            }
        }
        let limit = sender_limit(
            live.sender_position,
            live.sender_limit,
            message.consumption_term_id,
            message.consumption_term_offset,
            message.receiver_window_length,
            live.positions.initial_term_id(),
            live.positions.term_length(),
        );
        if !limit.accepted {
            self.statics.status_messages_rejected.increment();
            return;
        }
        live.answered = true;
        live.last_status_message_at = now;
        // The strategy (`spec/udp.md`, "The sender"; ADR 0053): under `max` the greater
        // of the limit and this position plus window; under `min` the table's entry
        // for the receiver id is added or moved, and the limit becomes the greater of
        // itself and the least in the table. Neither lowers it. A status message with
        // the end-of-stream flag moves its receiver's entry, which the next sweep
        // drops, or keeps once the publication is closed (ADR 0056), and adds none for
        // a receiver the table does not hold, whose image has ended: Aeron's `!isEos`
        // (ADR 0054).
        let raised = match live.flow_control {
            FlowControl::Max => limit.sender_limit,
            FlowControl::Min => {
                let plus_window = limit
                    .position
                    .wrapping_add(i64::from(message.receiver_window_length));
                let known = live
                    .receivers
                    .iter()
                    .position(|receiver| receiver.receiver_id == message.receiver_id);
                match known {
                    Some(index) => {
                        let receiver = &mut live.receivers[index];
                        receiver.limit = plus_window;
                        receiver.last_status_message_at = now;
                        receiver.end_of_stream = message.is_end_of_stream();
                    }
                    None if message.is_end_of_stream() => {}
                    // The spec's bound, within the room the conductor gave, which is at
                    // least that bound, so the push allocates nothing.
                    None if live.receivers.len()
                        < MAX_FLOW_CONTROL_RECEIVERS.min(live.receivers.capacity()) =>
                    {
                        live.receivers.push(Receiver {
                            receiver_id: message.receiver_id,
                            limit: plus_window,
                            last_status_message_at: now,
                            end_of_stream: false,
                        });
                        live.receivers_gauge.set(live.receivers.len() as i64);
                    }
                    // The table is full: the status message connects the publication
                    // below and moves nothing.
                    None => self.statics.flow_control_receivers_full.increment(),
                }
                live.receivers
                    .iter()
                    .map(|receiver| receiver.limit)
                    .min()
                    .map_or(live.sender_limit, |least| least.max(live.sender_limit))
            }
        };
        if raised > live.sender_limit {
            live.sender_limit = raised;
            live.limit.store_release(live.sender_limit);
        }
        // The flag says the receiver it came from has the stream to its end: on a
        // channel with an endpoint that is the one receiver, and the linger ends; on a
        // control channel another destination's receiver may still be NAKing the tail,
        // so the publication lingers for the linger timeout, Aeron's rule for a
        // multi-destination publication (`spec/udp.md`, "The sender"; ADR 0054).
        if message.is_end_of_stream() && live.closing.is_some() && !control {
            live.end_of_stream_acknowledged = true;
        }
        if live.connection == ConnectionState::Disconnected && self.pending.len() < PENDING_REPLIES
        {
            live.connection = ConnectionState::Connected;
            self.statics.publication_connected.increment();
            let registration_id = live.registration_id;
            self.reply(Reply::Connected { registration_id });
        }
    }

    /// A NAK (`spec/udp.md`, "The sender"): answered from the term when its position
    /// lies in the resend window and no retransmit is outstanding; one inside the
    /// outstanding range is dropped silently, one for another range is counted.
    fn on_nak(&mut self, endpoint_index: usize, nak: Nak) {
        let Some(index) = self.find(endpoint_index, nak.session_id, nak.stream_id) else {
            return;
        };
        self.statics.naks_received.increment();
        let live = &mut self.live[index];
        // An offset or a length the publication's term does not hold: dropped and
        // counted as a NAK outside the resend window is (ADR 0043).
        if nak.check(live.positions.term_length()).is_err() {
            self.statics.naks_ignored.increment();
            return;
        }
        let window = retransmit_window(
            live.sender_position,
            nak.term_id,
            nak.term_offset,
            live.positions.initial_term_id(),
            live.positions.term_length(),
        );
        if !window.in_window {
            self.statics.naks_ignored.increment();
            return;
        }
        let start = window.resend_position;
        let end = start
            .saturating_add(i64::from(nak.length))
            .min(live.sender_position);
        if let Some(outstanding) = live.retransmit {
            if !(outstanding.start <= start && start < outstanding.end) {
                self.statics.retransmit_overflow.increment();
            }
            return;
        }
        live.retransmit = Some(Retransmit {
            start,
            end,
            next: start,
            outstanding_until: i64::MAX,
        });
        self.statics.retransmits_sent.increment();
    }

    /// An RTT measurement with the reply flag is answered at once: the same frame without
    /// the flag, the echo timestamp and receiver id unchanged, a reception delta of 0,
    /// to the address it came from, which on a channel with an endpoint is the endpoint
    /// (`spec/udp.md`, "The sender", step 5). `datagram` is the measurement's index in
    /// the endpoint's last poll, whose source a control channel's socket kept.
    fn on_rtt_measurement(
        &mut self,
        endpoint_index: usize,
        measurement: RttMeasurement,
        datagram: usize,
    ) {
        if !measurement.is_probe()
            || self
                .find(
                    endpoint_index,
                    measurement.session_id,
                    measurement.stream_id,
                )
                .is_none()
        {
            return;
        }
        let bytes = measurement.answer().to_bytes();
        let endpoint = &self.endpoints[endpoint_index];
        let to = endpoint.socket.source(datagram);
        if endpoint.destinations.is_some() && to.is_none() {
            // No source to answer: the operating system gave none, which it does not.
            return;
        }
        endpoint.send_one(
            &mut self.headers,
            &bytes,
            to,
            &self.statics,
            &self.statics.rtt_replies_sent,
        );
    }

    /// One publication's duties for this cycle; returns how much work they were and
    /// whether the publication left the list.
    fn serve(&mut self, index: usize, now: i64) -> (usize, bool) {
        let Some(endpoint_index) = self
            .endpoints
            .iter()
            .position(|endpoint| endpoint.registration_id == self.live[index].endpoint_id)
        else {
            // The endpoint is gone before its last publication: the conductor removes
            // the publication next.
            return (0, false);
        };
        let live = &mut self.live[index];
        let endpoint = &self.endpoints[endpoint_index];
        let statics = &self.statics;
        let headers = &mut self.headers;
        let mut work = 0;

        // 1. A retransmit in progress, or one whose linger ended.
        if let Some(mut retransmit) = live.retransmit {
            if retransmit.next < retransmit.end {
                let burst = Burst::gather(
                    &live.reader,
                    live.positions,
                    retransmit.next,
                    i64::MAX,
                    Some(retransmit.end),
                    live.mtu_length,
                    statics,
                );
                let advanced = if burst.count == 0 {
                    // Cleaned, or nothing there: the range is answered as far as it can be.
                    retransmit.end - retransmit.next
                } else {
                    let delivery = endpoint.send_burst(
                        headers,
                        &burst.datagrams[..burst.count],
                        &burst.frames[..burst.count],
                        statics,
                    );
                    statics.retransmitted_bytes.add(delivery.bytes as i64);
                    burst.lengths[..delivery.whole].iter().sum::<usize>() as i64
                };
                retransmit.next += advanced;
                if retransmit.next >= retransmit.end {
                    retransmit.outstanding_until =
                        now.saturating_add(spec::nanos(RETRANSMIT_LINGER));
                }
                live.retransmit = Some(retransmit);
                work += 1;
            } else if now >= retransmit.outstanding_until {
                live.retransmit = None;
            }
        }

        // The `min` table's sweep (`spec/udp.md`, "The sender"; ADR 0053, ADR 0054, ADR
        // 0056): a receiver whose status message is older than the flow control
        // receiver timeout leaves the table, counted; one whose last status message
        // carried end of stream leaves it uncounted, its image ended as every image
        // ends after a close, which its own driver counts in `image.ended`, except
        // once the publication is closed, when it stays until the linger ends and
        // is never counted as timed out, so that the gauge still counts every
        // receiver that took the stream to its end and `doctor` reads a clean close
        // as no destination lost; it holds nothing back there, since a receiver ends
        // its stream only after the heartbeat with EOS, which goes once the sender
        // position has reached the end of stream position, and the retransmits a
        // NAK asks for after that meet no limit. When any remain the limit becomes
        // the greater of itself and the least of them, so that a receiver that went
        // away holds the stream back for the timeout and no longer. Empty with no
        // room under `max`, so this reads nothing there.
        if !live.receivers.is_empty() {
            let closed = live.closing.is_some();
            let before = live.receivers.len();
            let mut index = 0;
            while index < live.receivers.len() {
                let receiver = live.receivers[index];
                if receiver.end_of_stream && closed {
                    index += 1;
                } else if receiver.end_of_stream {
                    live.receivers.swap_remove(index);
                } else if now.saturating_sub(receiver.last_status_message_at)
                    >= spec::nanos(FLOW_CONTROL_RECEIVER_TIMEOUT)
                {
                    live.receivers.swap_remove(index);
                    statics.flow_control_receivers_timed_out.increment();
                } else {
                    index += 1;
                }
            }
            if live.receivers.len() != before {
                live.receivers_gauge.set(live.receivers.len() as i64);
                if let Some(least) = live.receivers.iter().map(|receiver| receiver.limit).min()
                    && least > live.sender_limit
                {
                    live.sender_limit = least;
                    live.limit.store_release(least);
                }
                work += 1;
            }
        }

        // 2. A setup frame: every setup interval until the first status message without
        //    the send-setup flag, and once for each status message with it.
        if (!live.answered || live.elicited)
            && now.saturating_sub(live.last_setup_at) >= spec::nanos(SETUP_INTERVAL)
        {
            let frame = SetupFrame {
                flags: 0,
                term_offset: live.positions.term_offset_of(live.sender_position) as i32,
                session_id: live.session_id,
                stream_id: live.stream_id,
                initial_term_id: live.positions.initial_term_id(),
                active_term_id: live.positions.term_id_of(live.sender_position),
                term_length: live.positions.term_length() as i32,
                mtu_length: live.mtu_length as i32,
                ttl: 0,
            };
            endpoint.send_one(
                headers,
                &frame.to_bytes(),
                None,
                statics,
                &statics.setups_sent,
            );
            live.last_setup_at = now;
            live.elicited = false;
            work += 1;
        }

        // Alone with its recording (`spec/udp.md`, "The sender"; ADR 0098): a recording
        // followed and no receiver answering. The sender position moves with the
        // recorder's and no data goes; a setup frame above or a heartbeat below names
        // the position a receiver joins at.
        let alone = live.alone(now);
        if alone && live.follow_recording(statics) {
            work += 1;
        }

        // 3. Data: one burst of whole committed frames up to the sender limit; none
        //    while the publication is alone with its recording.
        let room = if alone {
            0
        } else {
            live.sender_limit.saturating_sub(live.sender_position)
        };
        let burst = Burst::gather(
            &live.reader,
            live.positions,
            live.sender_position,
            room.max(0),
            None,
            live.mtu_length,
            statics,
        );
        let mut limited = burst.limited;
        if burst.count > 0 {
            let delivery = endpoint.send_burst(
                headers,
                &burst.datagrams[..burst.count],
                &burst.frames[..burst.count],
                statics,
            );
            if delivery.whole > 0 {
                let advanced: usize = burst.lengths[..delivery.whole].iter().sum();
                statics.bytes_sent.add(delivery.bytes as i64);
                statics.frames_sent.add(delivery.frames as i64);
                live.sender_position += advanced as i64;
                live.sent_position = live.sent_position.max(live.sender_position);
                // A burst ends at its term, so the seek is within the term. The reader's
                // own position is read by nothing: every read names its position, and a
                // sender position that followed a recording or went back for a receiver
                // leaves the reader's behind or ahead, where the seek is refused and
                // nothing depends on it (ADR 0098).
                let _ = live.reader.seek(live.sender_position);
                live.position.store_release(live.sender_position);
                live.last_sent_at = now;
                live.limited = false;
            }
            work += 1;
        } else if room <= 0 && !live.limited && !alone {
            // No window at all: a committed frame waiting is the limited state.
            limited = live
                .reader
                .block_from(live.sender_position, 1)
                .is_ok_and(|block| !block.is_empty());
        }
        if limited && !live.limited {
            live.limited = true;
            statics.flow_control_limited.increment();
        }

        // 4. A heartbeat after the heartbeat interval without a data frame, once the
        //    publication has its first status message; with EOS once closed and drained.
        if live.answered && now.saturating_sub(live.last_sent_at) >= spec::nanos(HEARTBEAT_INTERVAL)
        {
            let drained = live
                .closing
                .is_some_and(|(end, _)| live.sender_position >= end);
            let frame = Heartbeat {
                flags: if drained {
                    HEARTBEAT_FLAGS | END_OF_STREAM_FLAG
                } else {
                    HEARTBEAT_FLAGS
                },
                term_offset: live.positions.term_offset_of(live.sender_position) as i32,
                session_id: live.session_id,
                stream_id: live.stream_id,
                term_id: live.positions.term_id_of(live.sender_position),
                reserved_value: 0,
            };
            endpoint.send_one(
                headers,
                &frame.to_bytes(),
                None,
                statics,
                &statics.heartbeats_sent,
            );
            live.last_sent_at = now;
            work += 1;
        }

        // 5. The connection timeout.
        if live.connection == ConnectionState::Connected
            && now.saturating_sub(live.last_status_message_at) >= spec::nanos(CONNECTION_TIMEOUT)
            && self.pending.len() < PENDING_REPLIES
        {
            live.connection = ConnectionState::Disconnected;
            statics.publication_disconnected.increment();
            let registration_id = live.registration_id;
            self.reply(Reply::Disconnected { registration_id });
            return (work + 1, false);
        }

        // 6. The end of the linger: the linger timeout since the close, or on a channel
        //    with an endpoint a status message with the end of stream flag (ADR 0054).
        if let Some((_, closed_at)) = live.closing
            && (live.end_of_stream_acknowledged
                || now.saturating_sub(closed_at) >= spec::nanos(LINGER))
            && self.pending.len() < PENDING_REPLIES
        {
            let registration_id = live.registration_id;
            drop(self.live.swap_remove(index));
            self.reply(Reply::Closed { registration_id });
            return (work + 1, true);
        }
        (work, false)
    }
}

#[cfg(test)]
mod tests {
    //! ADR 0053's refusal split alone: which refusals are the socket's own, and how a
    //! control channel's burst folds over what each destination's call came to.

    use super::*;

    fn os(code: i32) -> std::io::Error {
        std::io::Error::from_raw_os_error(code)
    }

    /// What one destination's call came to: `whole` of the burst's datagrams, 100
    /// bytes and one frame each.
    fn took(whole: usize) -> Result<Delivery, Refusal> {
        Ok(Delivery {
            whole,
            bytes: 100 * whole,
            frames: whole,
        })
    }

    /// No room in the send buffer and an interrupted send are the socket's own; no
    /// route, an address that has gone and a loopback socket sending off the host are
    /// a destination's.
    #[test]
    fn a_refusal_is_the_sockets_own_only_for_no_room_or_an_interrupted_send() {
        use nix::libc;
        for code in [libc::EAGAIN, libc::EWOULDBLOCK, libc::ENOBUFS, libc::EINTR] {
            assert_eq!(Refusal::of(&os(code)), Refusal::Socket, "{}", os(code));
        }
        for code in [
            libc::ENETUNREACH,
            libc::EHOSTUNREACH,
            libc::EINVAL,
            libc::EADDRNOTAVAIL,
            libc::EACCES,
            libc::EPERM,
            libc::EMSGSIZE,
        ] {
            assert_eq!(Refusal::of(&os(code)), Refusal::Destination, "{}", os(code));
        }
    }

    /// Every destination took the burst whole: the position advances by all of it,
    /// and the bytes and frames are each destination's.
    #[test]
    fn a_burst_every_destination_took_advances_by_all_of_it() {
        assert_eq!(
            Delivery::fold(16, [took(16), took(16), took(16)].into_iter()),
            Delivery {
                whole: 16,
                bytes: 4800,
                frames: 48,
            }
        );
    }

    /// A short send to one destination holds the position at the fewest any took, so
    /// the others get those datagrams again, a duplicate their receivers drop.
    #[test]
    fn a_short_send_to_one_destination_holds_the_position_for_all() {
        assert_eq!(
            Delivery::fold(16, [took(16), took(5), took(16)].into_iter()).whole,
            5
        );
    }

    /// A refusal of the socket's own, at any destination, holds the whole burst for
    /// every destination, so that back-pressure costs none of them a frame.
    #[test]
    fn a_refusal_of_the_sockets_own_holds_the_burst_for_every_destination() {
        for outcomes in [
            [Err(Refusal::Socket), took(16), took(16)],
            [took(16), Err(Refusal::Socket), took(16)],
            [took(16), took(16), Err(Refusal::Socket)],
            [Err(Refusal::Destination), Err(Refusal::Socket), took(16)],
        ] {
            assert_eq!(Delivery::fold(16, outcomes.into_iter()).whole, 0);
        }
    }

    /// A refusal of a destination's own holds back no other destination: the position
    /// advances by what the others took, a short send among them still holding it, and
    /// the refused one's datagrams are lost to it alone, for its receiver to NAK.
    #[test]
    fn a_refusal_of_a_destinations_own_holds_back_no_other() {
        assert_eq!(
            Delivery::fold(
                16,
                [took(16), Err(Refusal::Destination), took(16)].into_iter()
            ),
            Delivery {
                whole: 16,
                bytes: 3200,
                frames: 32,
            }
        );
        assert_eq!(
            Delivery::fold(16, [Err(Refusal::Destination), took(7)].into_iter()).whole,
            7
        );
    }

    /// When the socket refused every destination for a reason of its own, the
    /// position holds, as on an endpoint, and nothing is counted as on the wire.
    #[test]
    fn a_burst_every_destination_refused_holds_the_position() {
        assert_eq!(
            Delivery::fold(
                16,
                [Err(Refusal::Destination), Err(Refusal::Destination)].into_iter()
            ),
            Delivery::default()
        );
        assert_eq!(
            Delivery::fold(16, [Err(Refusal::Destination)].into_iter()),
            Delivery::default()
        );
    }

    /// With no destination the burst counts as taken and nothing as on the wire, so
    /// the position advances past it.
    #[test]
    fn a_burst_to_no_destination_advances_past_it_and_counts_nothing() {
        assert_eq!(
            Delivery::fold(16, std::iter::empty()),
            Delivery {
                whole: 16,
                bytes: 0,
                frames: 0,
            }
        );
    }
}
