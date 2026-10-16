//! The recorder: the driver's second thread, which writes every live recording's
//! committed frames from the publication's log buffer into the durable log
//! (`spec/client-protocol.md`, "Commands"; `spec/durable-log.md`, "The driver"; ADR
//! 0025). The conductor never touches the disk: it hands the recorder each start, attach
//! and stop through a bounded queue, the recorder answers through a second one, and the
//! conductor sends the client's event when the answer arrives. On each duty cycle the
//! recorder reads its clock once, handles a bounded number of requests, visits every
//! live recording, takes one block of whole committed frames with
//! [`TermReader::block`], writes it with one positional write, stores the recording's
//! position with a release store, syncs once per second, and stops a recording whose
//! publication reached its end of stream, whose write failed, or whose block scan found
//! a frame that fails a check. A block is taken exactly when the writer's position moved,
//! `cavalry-log`'s rule, so a failed write leaves the reader where it was. Nothing here
//! allocates per block, locks, or logs; the syscalls are the recorder's own writes and
//! syncs.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};

use cavalry_log::catalog::CatalogError;
use cavalry_log::dir::LogDir;
use cavalry_log::layout::Recording;
use cavalry_log::recorder::{RecorderError, RecordingWriter};
use cavalry_proto::frame::FRAME_ALIGNMENT;
use cavalry_shm::counters::Counter;
use cavalry_shm::log::TermReader;
use cavalry_shm::spsc::{Consumer, Producer};

use crate::conductor::now_nanos;
use crate::idle::{IdleStrategy, Idler};
use crate::spec;

/// How many replies the recorder holds back when the reply queue is full: one per live
/// recording, which may stop on its own, and one per request handled in a cycle. With
/// the pending replies at this bound the recorder handles no request and stops no
/// recording until the conductor takes replies again; it keeps writing.
pub(crate) const PENDING_REPLIES: usize = spec::MAX_RECORDINGS + spec::RECORDER_REQUESTS_PER_CYCLE;

/// What a `start recording` command fixed about the recording, carried to the recorder
/// once: the descriptor's fields and a reader on the log at the start position.
pub(crate) struct Start {
    /// The publication's registration id, which names its log.
    pub publication_id: i64,
    /// The command's correlation id, which the reply carries back.
    pub correlation_id: i64,
    /// When the command was handled: the descriptor's start timestamp.
    pub start_timestamp: i64,
    /// The producer position when the command was handled: the start position.
    pub start_position: i64,
    /// The log buffer's initial term id.
    pub initial_term_id: i32,
    /// The log buffer's term length.
    pub term_length: i32,
    /// The log buffer's MTU length.
    pub mtu_length: i32,
    /// The publication's session id.
    pub session_id: i32,
    /// The publication's stream id.
    pub stream_id: i32,
    /// The channel, `cavalry:ipc` in this version.
    pub channel: String,
    /// The publisher's identity, `cavalry:ipc` in this version.
    pub source_identity: String,
    /// A reader on the publication's log at the start position; it keeps the mapping
    /// alive on the recorder's thread.
    pub reader: TermReader,
}

/// What the conductor asks the recorder.
pub(crate) enum Request {
    /// Take a catalog slot and create the files; answered with [`Reply::Ready`] or
    /// [`Reply::StartFailed`].
    Start(Start),
    /// The two counters the conductor allocated once the recording id was known; the
    /// recorder stores its positions into them from now on. A recording already stopped
    /// drops them.
    Attach {
        /// The recording.
        recording_id: i64,
        /// Its `recording.position` counter.
        position: Counter,
        /// Its `recording.synced_position` counter.
        synced: Counter,
    },
    /// Stop a recording at the position written; answered with [`Reply::Stopped`], or
    /// [`Reply::StopIgnored`] when the recording already stopped on its own.
    Stop {
        /// The recording.
        recording_id: i64,
        /// The `stop recording` command's correlation id, when a command asked.
        correlation_id: Option<i64>,
    },
}

/// Why a start failed, for the error event's code.
pub(crate) enum StartFailure {
    /// Every catalog slot is used: error code 10 and `recording.catalog_full`.
    CatalogFull,
    /// The time index or the first segment file could not be created: error code 12 and
    /// `recording.start_failed`, with the message.
    Files(String),
}

/// What the recorder answers.
pub(crate) enum Reply {
    /// The catalog slot is valid and synced, and the time index and the first segment
    /// file exist.
    Ready {
        /// The publication being recorded.
        publication_id: i64,
        /// The start command's correlation id.
        correlation_id: i64,
        /// The slot the recording took.
        recording_id: i64,
        /// The catalog's slot count after the start: the next recording id.
        next_recording_id: i64,
    },
    /// The start failed; nothing records.
    StartFailed {
        /// The publication that was to be recorded.
        publication_id: i64,
        /// The start command's correlation id.
        correlation_id: i64,
        /// Why.
        failure: StartFailure,
        /// The catalog's slot count after the attempt.
        next_recording_id: i64,
    },
    /// A recording stopped: the catalog's stop fields are written and synced, or the
    /// stop itself failed and the slot stays live for the next open's recovery.
    Stopped {
        /// The recording.
        recording_id: i64,
        /// Its publication.
        publication_id: i64,
        /// The position written, which the catalog holds as the stop position.
        stop_position: i64,
        /// The `stop recording` command's correlation id, when a command asked.
        correlation_id: Option<i64>,
    },
    /// A stop named a recording that had already stopped on its own, whose
    /// [`Reply::Stopped`] went out before.
    StopIgnored {
        /// The command's correlation id, when a command asked.
        correlation_id: Option<i64>,
    },
}

/// One recording the recorder writes.
struct Live {
    recording_id: i64,
    publication_id: i64,
    reader: TermReader,
    writer: RecordingWriter,
    position: Option<Counter>,
    synced: Option<Counter>,
}

/// What one visit to a live recording did.
enum Step {
    /// Nothing was committed, due or stopped.
    Idle,
    /// A block was written or a sync ran.
    Worked,
    /// The recording stopped and left the list.
    Stopped,
}

/// The recorder's state: the log directory it owns, the queues, and the live recordings.
pub(crate) struct Recorder {
    log: LogDir,
    requests: Consumer<Request>,
    replies: Producer<Reply>,
    live: Vec<Live>,
    /// Replies the reply queue had no room for, offered again next cycle.
    pending: VecDeque<Reply>,
    write_failed: Counter,
    rejected_frame: Counter,
    idler: Idler,
}

impl Recorder {
    /// A recorder over `log`, opened and recovered by the caller, with the two static
    /// counters it moves: `recording.write_failed` and `log_buffer.rejected_frame`.
    pub(crate) fn new(
        log: LogDir,
        requests: Consumer<Request>,
        replies: Producer<Reply>,
        write_failed: Counter,
        rejected_frame: Counter,
        idle: IdleStrategy,
    ) -> Self {
        Self {
            log,
            requests,
            replies,
            live: Vec::with_capacity(spec::MAX_RECORDINGS),
            pending: VecDeque::with_capacity(PENDING_REPLIES),
            write_failed,
            rejected_frame,
            idler: Idler::new(idle),
        }
    }

    /// The recorder's loop: duty cycles until `stop`, then every live recording is
    /// stopped at the position written, so the catalog holds a clean stop and the next
    /// open repairs nothing.
    pub(crate) fn run(mut self, stop: &AtomicBool) {
        while !stop.load(Ordering::Acquire) {
            let now = now_nanos();
            if self.duty_cycle(now) == 0 {
                self.idler.idle();
            } else {
                self.idler.reset();
            }
        }
        // The conductor stops on the same flag and takes no more replies, so a stop here
        // keeps its reply only while the pending replies have room; the catalog's stop
        // is what the next open reads.
        let now = now_nanos();
        while let Some(last) = self.live.len().checked_sub(1) {
            self.flush_replies();
            self.stop_at(last, now, None);
        }
        self.flush_replies();
    }

    /// One duty cycle; returns how much work it found, 0 when the recorder may idle. The
    /// clock is read once by the caller and passed down.
    pub(crate) fn duty_cycle(&mut self, now: i64) -> usize {
        let mut work = self.flush_replies();
        for _ in 0..spec::RECORDER_REQUESTS_PER_CYCLE {
            if self.pending.len() >= PENDING_REPLIES {
                break;
            }
            let Some(request) = self.requests.poll() else {
                break;
            };
            self.handle(request, now);
            work += 1;
        }
        let mut index = 0;
        while index < self.live.len() {
            let may_stop = self.pending.len() < PENDING_REPLIES;
            match self.record(index, now, may_stop) {
                Step::Stopped => work += 1,
                Step::Worked => {
                    work += 1;
                    index += 1;
                }
                Step::Idle => index += 1,
            }
        }
        work + self.flush_replies()
    }

    fn handle(&mut self, request: Request, now: i64) {
        match request {
            Request::Start(start) => self.start(start, now),
            Request::Attach {
                recording_id,
                position,
                synced,
            } => {
                if let Some(live) = self
                    .live
                    .iter_mut()
                    .find(|live| live.recording_id == recording_id)
                {
                    position.store_release(live.writer.position());
                    synced.store_release(live.writer.synced_position());
                    live.position = Some(position);
                    live.synced = Some(synced);
                }
            }
            Request::Stop {
                recording_id,
                correlation_id,
            } => match self
                .live
                .iter()
                .position(|live| live.recording_id == recording_id)
            {
                Some(index) => {
                    self.stop_at(index, now, correlation_id);
                }
                None => self
                    .pending
                    .push_back(Reply::StopIgnored { correlation_id }),
            },
        }
    }

    /// Takes the catalog slot and creates the files (`LogDir::start`), or says why not.
    fn start(&mut self, start: Start, now: i64) {
        let next = |log: &LogDir| i64::try_from(log.catalog().slots()).unwrap_or(i64::MAX);
        let publication_id = start.publication_id;
        let correlation_id = start.correlation_id;
        if self.live.len() >= spec::MAX_RECORDINGS {
            // The conductor bounds its registry to the same number, so this is a bug
            // there; refused rather than grown.
            self.pending.push_back(Reply::StartFailed {
                publication_id,
                correlation_id,
                failure: StartFailure::Files(format!(
                    "the recorder holds {} recordings, its capacity",
                    spec::MAX_RECORDINGS
                )),
                next_recording_id: next(&self.log),
            });
            return;
        }
        let recording = Recording {
            recording_id: 0,
            start_timestamp: start.start_timestamp,
            start_position: start.start_position,
            initial_term_id: start.initial_term_id,
            term_length: start.term_length,
            mtu_length: start.mtu_length,
            session_id: start.session_id,
            stream_id: start.stream_id,
            channel: &start.channel,
            source_identity: &start.source_identity,
        };
        match self.log.start(&recording, now) {
            Ok(writer) => {
                let recording_id = writer.recording_id();
                self.live.push(Live {
                    recording_id,
                    publication_id,
                    reader: start.reader,
                    writer,
                    position: None,
                    synced: None,
                });
                self.pending.push_back(Reply::Ready {
                    publication_id,
                    correlation_id,
                    recording_id,
                    next_recording_id: next(&self.log),
                });
            }
            Err(error) => {
                self.pending.push_back(Reply::StartFailed {
                    publication_id,
                    correlation_id,
                    failure: start_failure(error),
                    next_recording_id: next(&self.log),
                });
            }
        }
    }

    /// One visit to the live recording at `index`: a block if one is committed, a sync
    /// if one is due, a stop if the stream ended or a write failed. With `may_stop`
    /// false a stop waits for the next cycle, since there is no room for its reply.
    fn record(&mut self, index: usize, now: i64, may_stop: bool) -> Step {
        let live = &mut self.live[index];
        let mut worked = false;
        // A committed frame that fails a check of spec/frame.md fails the whole scan,
        // so the frames before it are taken one at a time until it is the next frame;
        // then it is counted as a subscriber's report is, and the recording ends at the
        // last frame that checked (invariant 1).
        let block = match live.reader.block(spec::RECORDER_BLOCK_LIMIT) {
            Ok(block) => block,
            Err(_) => match live.reader.block(FRAME_ALIGNMENT) {
                Ok(block) => block,
                Err(_) => {
                    if !may_stop {
                        return Step::Idle;
                    }
                    self.rejected_frame.increment();
                    return self.stop_at(index, now, None);
                }
            },
        };
        if block.is_empty() {
            if live.reader.is_end_of_stream() {
                if !may_stop {
                    return Step::Idle;
                }
                return self.stop_at(index, now, None);
            }
        } else {
            let end = block.end();
            let before = live.writer.position();
            let written = live.writer.write(block.bytes, block.length, now);
            // The block was taken exactly when the position moved, whatever `write`
            // returned; a block not taken is scanned again next cycle.
            if live.writer.position() != before {
                // `end` is the end of a block of the reader's current term, a multiple
                // of 32 at most the term's end, which `seek` accepts: it cannot fail
                // without the reader and the writer disagreeing from here on.
                let sought = live.reader.seek(end);
                debug_assert!(sought.is_ok(), "the block's end is in the reader's term");
                if let Some(position) = &live.position {
                    position.store_release(live.writer.position());
                }
                worked = true;
            }
            if written.is_err() {
                if !may_stop {
                    return Step::Idle;
                }
                self.write_failed.increment();
                return self.stop_at(index, now, None);
            }
        }
        match live.writer.sync(now) {
            Ok(true) => {
                if let Some(synced) = &live.synced {
                    synced.store_release(live.writer.synced_position());
                }
                worked = true;
            }
            Ok(false) => {}
            Err(_) => {
                if !may_stop {
                    return Step::Idle;
                }
                self.write_failed.increment();
                return self.stop_at(index, now, None);
            }
        }
        if worked { Step::Worked } else { Step::Idle }
    }

    /// Stops the recording at `index` at the position written: the stop entry, the
    /// syncs and the catalog's stop fields (`LogDir::stop`). A stop that fails counts
    /// `recording.write_failed` and leaves the slot live for the next open's recovery;
    /// the reply still carries the position written.
    fn stop_at(&mut self, index: usize, now: i64, correlation_id: Option<i64>) -> Step {
        let live = self.live.swap_remove(index);
        let written = live.writer.position();
        let stop_position = match self.log.stop(live.writer, now) {
            Ok(descriptor) => {
                if let Some(synced) = &live.synced {
                    synced.store_release(descriptor.stop_position());
                }
                descriptor.stop_position()
            }
            Err(_) => {
                self.write_failed.increment();
                written
            }
        };
        if let Some(position) = &live.position {
            position.store_release(stop_position);
        }
        // During duty cycles a stop happens only with room for its reply; only the
        // shutdown loop finds none, after the conductor has stopped reading.
        if self.pending.len() < PENDING_REPLIES {
            self.pending.push_back(Reply::Stopped {
                recording_id: live.recording_id,
                publication_id: live.publication_id,
                stop_position,
                correlation_id,
            });
        }
        Step::Stopped
    }

    /// How many replies wait for room in the reply queue.
    #[cfg(test)]
    pub(crate) fn pending_replies(&self) -> usize {
        self.pending.len()
    }

    /// How many recordings the recorder writes.
    #[cfg(test)]
    pub(crate) fn live_recordings(&self) -> usize {
        self.live.len()
    }

    /// Offers the pending replies in order until the queue is full; returns how many went.
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
}

/// The code a failed start is answered with: a full catalog is code 10, anything else
/// is code 12 with the error's message (`spec/client-protocol.md`, "Commands").
pub(crate) fn start_failure(error: RecorderError) -> StartFailure {
    match error {
        RecorderError::Catalog(CatalogError::Full) => StartFailure::CatalogFull,
        other => StartFailure::Files(other.to_string()),
    }
}

impl std::fmt::Debug for Recorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Recorder")
            .field("log", &self.log)
            .field("live", &self.live.len())
            .field("pending_replies", &self.pending.len())
            .finish()
    }
}
