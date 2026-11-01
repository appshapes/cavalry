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
//! the syscalls are the endpoint's own sends and receives, one per burst on Linux. The
//! sender is built on its own thread, since the headers its batched calls fill
//! ([`Headers`]) hold raw pointers and cannot be sent to it (ADR 0041, acceptance).

use std::collections::VecDeque;
use std::io::IoSlice;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use cavalry_proto::frame::Flags;
use cavalry_proto::udp::{
    CONNECTION_TIMEOUT, ControlFrame, DATAGRAMS_PER_BURST, HEARTBEAT_INTERVAL, Heartbeat, LINGER,
    Nak, RETRANSMIT_LINGER, RttMeasurement, SETUP_INTERVAL, SetupFrame, StatusMessage, parse,
    retransmit_window, sender_limit,
};
use cavalry_shm::counters::generated::{
    LOG_BUFFER_REJECTED_FRAME, PUBLICATION_CONNECTED, PUBLICATION_DISCONNECTED,
    RECEIVER_RECEIVE_FAILED, SENDER_BYTES_SENT, SENDER_FLOW_CONTROL_LIMITED, SENDER_FRAMES_SENT,
    SENDER_HEARTBEATS_SENT, SENDER_NAKS_IGNORED, SENDER_NAKS_RECEIVED, SENDER_RETRANSMIT_OVERFLOW,
    SENDER_RETRANSMITS_SENT, SENDER_RETRANSMITTED_BYTES, SENDER_RTT_REPLIES_SENT,
    SENDER_SEND_FAILED, SENDER_SETUPS_SENT, SENDER_SHORT_SENDS, SENDER_STATUS_MESSAGES_RECEIVED,
    SENDER_STATUS_MESSAGES_REJECTED,
};
use cavalry_shm::counters::{Counter, CounterId, CountersError};
use cavalry_shm::log::{Positions, TermReader};
use cavalry_shm::spsc::{Consumer, Producer};

use crate::conductor::now_nanos;
use crate::idle::{IdleStrategy, Idler};
use crate::socket::{Headers, SendSocket};
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
/// change and a close per publication, a close per endpoint, and one per request
/// handled in a cycle. With the pending replies at this bound the sender handles no
/// request and moves no publication's connection state until the conductor takes
/// replies again; it keeps sending (ADR 0041).
pub(crate) const PENDING_REPLIES: usize =
    2 * spec::MAX_PUBLICATIONS + spec::MAX_SEND_ENDPOINTS + spec::SENDER_REQUESTS_PER_CYCLE;

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
}

/// What the conductor asks the sender.
pub(crate) enum Request {
    /// A send endpoint the conductor opened: its socket moves to the sender.
    AddEndpoint {
        /// The endpoint's registration id: the correlation id of the command that
        /// opened it, which keys its `channel.status` counter.
        registration_id: i64,
        /// The socket, connected and non-blocking.
        socket: SendSocket,
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
    /// answers with [`Reply::Closed`] when a status message with the end of stream flag
    /// arrives or the linger passes.
    Close {
        /// The publication.
        registration_id: i64,
        /// The end of stream position the conductor stored.
        end_of_stream_position: i64,
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
    publication_connected: Counter,
    publication_disconnected: Counter,
    receive_failed: Counter,
    rejected_frame: Counter,
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
            publication_connected: counter(PUBLICATION_CONNECTED)?,
            publication_disconnected: counter(PUBLICATION_DISCONNECTED)?,
            receive_failed: counter(RECEIVER_RECEIVE_FAILED)?,
            rejected_frame: counter(LOG_BUFFER_REJECTED_FRAME)?,
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
    /// A status message with the end of stream flag arrived after the close.
    end_of_stream_acknowledged: bool,
    /// A frame to send met no window: `sender.flow_control_limited` counts the entry.
    limited: bool,
}

/// One send endpoint: its socket, on this thread from the conductor's add until its
/// remove.
struct Endpoint {
    registration_id: i64,
    socket: SendSocket,
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
            live: Vec::with_capacity(spec::MAX_PUBLICATIONS),
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
            } => {
                debug_assert!(self.endpoints.len() < spec::MAX_SEND_ENDPOINTS);
                self.endpoints.push(Endpoint {
                    registration_id,
                    socket,
                });
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
                debug_assert!(self.live.len() < spec::MAX_PUBLICATIONS);
                let sender_position = add.reader.position();
                add.position.store_release(sender_position);
                add.limit.store_release(0);
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
            // Every log of this version has the UDP term length (`spec/udp.md`, "Values").
            let frame = parse(
                self.endpoints[index].socket.datagram(datagram),
                spec::UDP_TERM_LENGTH,
            );
            match frame {
                Ok(ControlFrame::StatusMessage(message)) => {
                    self.on_status_message(index, message, now);
                }
                Ok(ControlFrame::Nak(nak)) => self.on_nak(index, nak),
                Ok(ControlFrame::RttMeasurement(measurement)) => {
                    self.on_rtt_measurement(index, measurement);
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
    /// publication.
    fn on_status_message(&mut self, endpoint_index: usize, message: StatusMessage, now: i64) {
        let Some(index) = self.find(endpoint_index, message.session_id, message.stream_id) else {
            return;
        };
        self.statics.status_messages_received.increment();
        let live = &mut self.live[index];
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
        if limit.sender_limit > live.sender_limit {
            live.sender_limit = limit.sender_limit;
            live.limit.store_release(live.sender_limit);
        }
        if message.is_end_of_stream() && live.closing.is_some() {
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
    /// the flag, the echo timestamp and receiver id unchanged, a reception delta of 0.
    fn on_rtt_measurement(&mut self, endpoint_index: usize, measurement: RttMeasurement) {
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
        let sent = self.endpoints[endpoint_index]
            .socket
            .send_one(&mut self.headers, &bytes);
        self.statics.count_one(sent, &self.statics.rtt_replies_sent);
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
                    match endpoint
                        .socket
                        .send_burst(headers, &burst.datagrams[..burst.count])
                    {
                        Ok(sent) => {
                            if sent.short {
                                statics.short_sends.increment();
                            }
                            if sent.failed {
                                statics.send_failed.increment();
                            }
                            let whole = sent.whole.min(burst.count);
                            let bytes: usize =
                                burst.datagrams[..whole].iter().map(|d| d[0].len()).sum();
                            statics.retransmitted_bytes.add(bytes as i64);
                            burst.lengths[..whole].iter().sum::<usize>() as i64
                        }
                        Err(_) => {
                            statics.send_failed.increment();
                            0
                        }
                    }
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
            let sent = endpoint.socket.send_one(headers, &frame.to_bytes());
            statics.count_one(sent, &statics.setups_sent);
            live.last_setup_at = now;
            live.elicited = false;
            work += 1;
        }

        // 3. Data: one burst of whole committed frames up to the sender limit.
        let room = live.sender_limit.saturating_sub(live.sender_position);
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
            match endpoint
                .socket
                .send_burst(headers, &burst.datagrams[..burst.count])
            {
                Ok(sent) => {
                    if sent.short {
                        statics.short_sends.increment();
                    }
                    if sent.failed {
                        statics.send_failed.increment();
                    }
                    let whole = sent.whole.min(burst.count);
                    if whole > 0 {
                        let bytes: usize =
                            burst.datagrams[..whole].iter().map(|d| d[0].len()).sum();
                        let frames: usize = burst.frames[..whole].iter().sum();
                        let advanced: usize = burst.lengths[..whole].iter().sum();
                        statics.bytes_sent.add(bytes as i64);
                        statics.frames_sent.add(frames as i64);
                        live.sender_position += advanced as i64;
                        // A burst ends at its term, so the seek is within the term.
                        let _ = live.reader.seek(live.sender_position);
                        live.position.store_release(live.sender_position);
                        live.last_sent_at = now;
                        live.limited = false;
                    }
                }
                Err(_) => statics.send_failed.increment(),
            }
            work += 1;
        } else if room <= 0 && !live.limited {
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
            let sent = endpoint.socket.send_one(headers, &frame.to_bytes());
            statics.count_one(sent, &statics.heartbeats_sent);
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

        // 6. The end of the linger: a status message with the end of stream flag, or the
        //    linger timeout since the close.
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
