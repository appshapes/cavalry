//! A recording the driver writes for a client's stream: `start recording` and `stop
//! recording` of `spec/client-protocol.md` (ADR 0025). The recording belongs to the
//! driver, not to the client that started it: it outlives the client, any client may stop
//! it, and dropping a [`Recording`] stops nothing. The handle reads the recording's two
//! positions from the counters file while the recording is live, as `cavalry doctor`
//! does, and learns its stop from the `recording stopped` event the client's conductor
//! keeps for it: the driver frees a stopped recording's counters, and their records are
//! other counters' a second later. Between the two, a stop the conductor has not read
//! yet, the handle tells that a record is no longer its recording's from the record's
//! key, which holds the recording id, and returns what it last read from its own.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::Instant;

use cavalry_proto::client_protocol::ErrorCode;
use cavalry_shm::counters::generated::{RECORDING_POSITION, RECORDING_SYNCED_POSITION};
use cavalry_shm::counters::{CounterId, CounterValue, CountersReader};
use cavalry_shm::spsc;

use crate::conductor::{RESPONSE_TIMEOUT, Shared};
use crate::response::Response;
use crate::{Cavalry, ClientError};

/// Where a recording stopped, as `recording stopped` said it: shared between the
/// conductor, which stores it once, and the handle, which reads it. A position of −1
/// means the recording is live as far as this client has read. The conductor marks it
/// refused instead when its registry could not admit the registration
/// ([`crate::conductor::RecordingRegistry`]), which `start_recording` reads before it
/// hands the handle over.
pub(crate) struct RecordingStop {
    position: AtomicI64,
    refused: AtomicBool,
}

impl RecordingStop {
    pub(crate) fn new() -> Self {
        Self {
            position: AtomicI64::new(-1),
            refused: AtomicBool::new(false),
        }
    }

    /// The conductor's store, once, from the event: a release store, which the handle's
    /// acquire load orders after.
    pub(crate) fn set(&self, position: i64) {
        self.position.store(position, Ordering::Release);
    }

    /// The stop position, once stored.
    pub(crate) fn get(&self) -> Option<i64> {
        let position = self.position.load(Ordering::Acquire);
        (position >= 0).then_some(position)
    }

    /// The conductor's mark that its registry was full and the registration dropped:
    /// nobody will store this stop.
    pub(crate) fn refuse(&self) {
        self.refused.store(true, Ordering::Release);
    }

    /// Whether the registration was refused.
    pub(crate) fn refused(&self) -> bool {
        self.refused.load(Ordering::Acquire)
    }
}

/// One of a recording's two position counters as the handle follows it: the value's
/// slot, the type the record must have to be this recording's, and the last value read
/// while it was.
struct Followed {
    /// The record's value slot; `None` for a handle made after the recording stopped
    /// and its record was freed, which reads no counter.
    value: Option<CounterValue>,
    type_id: CounterId,
    /// The highest value read from the record while its key named this recording, or
    /// the stop position for a handle made after the stop: what a read returns when the
    /// record is no longer the recording's and the stop has not arrived. For the
    /// position counter this can be above the stop position, when the stop's own
    /// writes failed and the driver announced the last sync (ADR 0064). Written by any
    /// thread that reads, with `fetch_max`, since two readers may finish out of order;
    /// nobody branches on it but the read that returns it.
    last: AtomicI64,
}

impl Followed {
    /// Record `id` followed as recording `recording_id`'s counter of type `type_id`:
    /// its value read and then checked to be the recording's, or, when the record is
    /// not, the stop position `stop` holds. `None` when the record is not the
    /// recording's and no stop has arrived: nothing this could return would be the
    /// recording's.
    fn new(
        counters: &CountersReader,
        id: u32,
        type_id: CounterId,
        recording_id: i64,
        stop: &RecordingStop,
    ) -> Option<Self> {
        if let Ok(value) = counters.value(id) {
            let live = value.load_acquire();
            if is_recordings(counters, id, type_id, recording_id) {
                return Some(Self {
                    value: Some(value),
                    type_id,
                    last: AtomicI64::new(live),
                });
            }
        }
        stop.get().map(|stopped| Self {
            value: None,
            type_id,
            last: AtomicI64::new(stopped),
        })
    }
}

/// A recording the driver started for one publication: its id in the catalog, the
/// session recorded, and its two positions as the driver stores them
/// (`spec/client-protocol.md`, "Events", `recording ready`). The client follows the
/// stop of every recording it started while the handle lives: once the recording has
/// stopped, both positions are the stop position, a position a sync made durable, and no
/// counter is read. A position is never another counter's value: each read checks that
/// the record it loaded is still this recording's.
pub struct Recording {
    recording_id: i64,
    session_id: i32,
    stream_id: i32,
    channel: String,
    counters: Arc<CountersReader>,
    position: Followed,
    synced_position: Followed,
    stop: Arc<RecordingStop>,
}

impl Recording {
    /// The handle on recording `recording_id`, whose counters are records
    /// `position_counter_id` and `synced_position_counter_id` of `counters`, or `None`
    /// when a record is not this recording's and `stop` holds no stop. A record that is
    /// not the recording's is a recording that has stopped, so the caller waits for the
    /// stop and asks again (`Cavalry::start_recording`).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        recording_id: i64,
        session_id: i32,
        stream_id: i32,
        channel: &str,
        counters: &Arc<CountersReader>,
        position_counter_id: u32,
        synced_position_counter_id: u32,
        stop: &Arc<RecordingStop>,
    ) -> Option<Self> {
        let position = Followed::new(
            counters,
            position_counter_id,
            RECORDING_POSITION,
            recording_id,
            stop,
        )?;
        let synced_position = Followed::new(
            counters,
            synced_position_counter_id,
            RECORDING_SYNCED_POSITION,
            recording_id,
            stop,
        )?;
        Some(Self {
            recording_id,
            session_id,
            stream_id,
            channel: channel.to_string(),
            counters: Arc::clone(counters),
            position,
            synced_position,
            stop: Arc::clone(stop),
        })
    }

    /// The live value of `followed`, or the stop position once the recording has
    /// stopped. The stop is looked at first, so a stopped recording's counter is never
    /// read once this client has read `recording stopped`. Before that, the value is
    /// loaded and then the record is checked: a record that is this recording's after
    /// the load was this recording's at the load, since the driver allocates a
    /// recording's counters once and a recording id is never reused. A record that is
    /// not, freed or another counter's by now, is a recording that has stopped, whose
    /// stop the conductor has not stored yet: the read returns the stop if it arrived
    /// meanwhile, and otherwise the last value read from the recording's own record,
    /// never another counter's value. That value is at or below the stop position, but
    /// for the position counter of a stop whose own writes failed, which the driver
    /// announces at the last sync (ADR 0064). The check is the
    /// counters file's reader protocol (`spec/counters-file.md`, "Metadata record"),
    /// with that protocol's own bound: it completes within the counter reuse timeout.
    fn live_or_stopped(&self, followed: &Followed) -> i64 {
        if let Some(stopped) = self.stop.get() {
            return stopped;
        }
        if let Some(value) = &followed.value {
            let live = value.load_acquire();
            if is_recordings(
                &self.counters,
                value.id(),
                followed.type_id,
                self.recording_id,
            ) {
                return followed.last.fetch_max(live, Ordering::Relaxed).max(live);
            }
        }
        self.stop
            .get()
            .unwrap_or_else(|| followed.last.load(Ordering::Relaxed))
    }

    /// The recording id: its slot in the catalog, never reused, what `stop recording`,
    /// `cavalry replay` and `cavalry doctor` name it by.
    pub fn recording_id(&self) -> i64 {
        self.recording_id
    }

    /// The session of the publication being recorded: which of a stream's publications
    /// this recording holds.
    pub fn session_id(&self) -> i32 {
        self.session_id
    }

    /// The stream recorded.
    pub fn stream_id(&self) -> i32 {
        self.stream_id
    }

    /// The channel recorded, `cavalry:ipc` in this version.
    pub fn channel(&self) -> &str {
        &self.channel
    }

    /// The position up to which the recorder has written the stream into its segment
    /// files, what the operating system holds: the `recording.position` counter, read
    /// with an acquire load. Behind the publisher by at most the publication window,
    /// since the publisher's limit respects it. Once the recording has stopped, its
    /// [`stop_position`]. That is below a value this returned before the stop when the
    /// stop's own writes or syncs failed, since the driver then announces the position
    /// last synced (ADR 0064).
    ///
    /// [`stop_position`]: Recording::stop_position
    pub fn position(&self) -> i64 {
        self.live_or_stopped(&self.position)
    }

    /// The position up to which a sync has made the recording durable on disk: the
    /// `recording.synced_position` counter, at most a second behind [`position`]
    /// (ADR 0025). Once the recording has stopped, its [`stop_position`], which a sync
    /// made durable before the driver said so: the stop's own sync, or, when the stop's
    /// own writes or syncs failed, the last sync before the stop (ADR 0064). It never
    /// moves back.
    ///
    /// [`position`]: Recording::position
    /// [`stop_position`]: Recording::stop_position
    pub fn synced_position(&self) -> i64 {
        self.live_or_stopped(&self.synced_position)
    }

    /// Where the recording stopped, once the driver has said so with `recording
    /// stopped`: by a `stop recording` from any client, at its publication's end of
    /// stream, or on a failure. `None` while it is live as far as this client has read.
    ///
    /// The position is durable on every path (`spec/client-protocol.md`, "Commands";
    /// ADR 0064). Usually it is the catalog's stop position, which the stop wrote and
    /// synced. A stop whose own writes or syncs failed, on a full disk for instance,
    /// still stops the recording, and `recording.write_failed` counts it. Then this is
    /// the position the recorder had synced before the stop. The catalog holds no stop
    /// for the recording until the driver's next start rebuilds one from the files, at
    /// or above this position.
    pub fn stop_position(&self) -> Option<i64> {
        self.stop.get()
    }

    /// Stops this recording through `client`, which need not be the client that started
    /// it: [`Cavalry::stop_recording`] with [`recording_id`], and the same answers (ADR
    /// 0025). The handle stays valid: [`stop_position`] says where it stopped once the
    /// conductor has read the driver's `recording stopped`, which follows the answer,
    /// and both positions are that position from then on.
    ///
    /// [`recording_id`]: Recording::recording_id
    /// [`stop_position`]: Recording::stop_position
    pub fn stop(&self, client: &mut Cavalry) -> Result<(), ClientError> {
        client.stop_recording(self.recording_id)
    }
}

/// `recording ready` as a handle is made from it: the recording's id and session, the
/// records of its two counters, and when the answer was read.
pub(crate) struct RecordingAnswer {
    recording_id: i64,
    session_id: i32,
    position_counter_id: u32,
    synced_position_counter_id: u32,
    read: Instant,
}

impl RecordingAnswer {
    /// The answer `response` carries; anything but a `recording ready` with two counter
    /// ids a counters file can hold is [`ClientError::Driver`].
    pub(crate) fn new(response: Response) -> Result<Self, ClientError> {
        let Response::RecordingReady {
            recording_id,
            session_id,
            position_counter_id,
            synced_position_counter_id,
            ..
        } = response
        else {
            return Err(ClientError::Driver {
                what: format!("{response:?} in answer to start recording"),
            });
        };
        let counter = |id: i32| -> Result<u32, ClientError> {
            u32::try_from(id).map_err(|_| ClientError::Driver {
                what: format!("recording counter id {id}"),
            })
        };
        Ok(Self {
            recording_id,
            session_id,
            position_counter_id: counter(position_counter_id)?,
            synced_position_counter_id: counter(synced_position_counter_id)?,
            read: Instant::now(),
        })
    }

    /// The handle, once it can be made. `Ok(None)` while a counter's record is not the
    /// recording's and `stop` holds no stop: the recording has already stopped and its
    /// counters are freed, and the conductor stores the stop within its next cycles,
    /// since `recording stopped` follows `recording ready`. No stop within the response
    /// timeout of the answer is a driver that named records that were never the
    /// recording's: [`ClientError::Driver`].
    pub(crate) fn recording(
        &self,
        channel: &str,
        stream_id: i32,
        counters: &Arc<CountersReader>,
        stop: &Arc<RecordingStop>,
    ) -> Result<Option<Recording>, ClientError> {
        if let Some(recording) = Recording::new(
            self.recording_id,
            self.session_id,
            stream_id,
            channel,
            counters,
            self.position_counter_id,
            self.synced_position_counter_id,
            stop,
        ) {
            return Ok(Some(recording));
        }
        if self.read.elapsed() < RESPONSE_TIMEOUT {
            return Ok(None);
        }
        Err(ClientError::Driver {
            what: format!(
                "recording ready for recording {} with counters {} and {}, which are not that \
                 recording's, and no recording stopped after it",
                self.recording_id, self.position_counter_id, self.synced_position_counter_id
            ),
        })
    }
}

/// A recording the driver may hold armed: what [`Cavalry::arm_recording`] returns. The
/// driver arms a recording on a `cavalry:udp` stream that has a live subscription and
/// no publication or image yet, and starts it when the stream's next image arrives, at
/// the image's join position, so that every frame of the image is in the recording
/// (`spec/client-protocol.md`, "Commands", start recording; ADR 0059). The arming
/// command is answered twice, and this handle holds the place of the second answer:
/// [`ArmedRecording::poll`] returns the [`Recording`] once the driver has started it.
///
/// The recording is the driver's, armed or started: dropping this handle disarms
/// nothing, since the protocol has no command for that, and the recording starts with
/// the image all the same. The driver drops an armed recording when the stream's last
/// subscription on the endpoint goes, and `poll` then returns the driver's error.
pub struct ArmedRecording {
    stream_id: i32,
    channel: String,
    counters: Arc<CountersReader>,
    shared: Arc<Shared>,
    stop: Arc<RecordingStop>,
    answers: spsc::Consumer<Response>,
    ready: Option<RecordingAnswer>,
    done: bool,
}

impl ArmedRecording {
    pub(crate) fn new(
        stream_id: i32,
        channel: &str,
        counters: Arc<CountersReader>,
        shared: Arc<Shared>,
        stop: Arc<RecordingStop>,
        answers: spsc::Consumer<Response>,
        ready: Option<RecordingAnswer>,
    ) -> Self {
        Self {
            stream_id,
            channel: channel.to_string(),
            counters,
            shared,
            stop,
            answers,
            ready,
            done: false,
        }
    }

    /// The stream the recording is armed on.
    pub fn stream_id(&self) -> i32 {
        self.stream_id
    }

    /// The channel the recording is armed on.
    pub fn channel(&self) -> &str {
        &self.channel
    }

    /// The recording, once: `Ok(None)` while the driver holds it armed, and
    /// `Ok(Some(recording))` by the first poll after the driver has started it, with
    /// the stream's next image or, when the stream already had a publication or an
    /// image, at the command. Call it from the loop that polls the subscription; it
    /// reads one queue of one and sends nothing.
    ///
    /// The driver's second answer may be an error instead, returned once as
    /// [`ClientError::Refused`]: code 20 when the armed recording was dropped, the
    /// stream's last subscription on the endpoint gone, and code 10 or 12 when its
    /// start failed at a registry, the catalog, the counters file or its files. After
    /// the recording or an error the handle is spent, [`ArmedRecording::is_done`], and
    /// every later poll is `Ok(None)`. A closed client is [`ClientError::Closed`] at
    /// every poll while the recording is still armed.
    pub fn poll(&mut self) -> Result<Option<Recording>, ClientError> {
        if self.done {
            return Ok(None);
        }
        if self.ready.is_none() {
            match self.answers.poll() {
                Some(Response::Error { code, message, .. }) => {
                    self.done = true;
                    return Err(ClientError::Refused {
                        code: ErrorCode::from_code(code),
                        raw_code: code,
                        message,
                    });
                }
                Some(answer) => match RecordingAnswer::new(answer) {
                    Ok(ready) => self.ready = Some(ready),
                    Err(error) => {
                        self.done = true;
                        return Err(error);
                    }
                },
                None => {
                    return match self.shared.closed() {
                        Some(reason) => Err(ClientError::Closed(reason)),
                        None => Ok(None),
                    };
                }
            }
        }
        let Some(ready) = &self.ready else {
            return Ok(None);
        };
        match ready.recording(&self.channel, self.stream_id, &self.counters, &self.stop) {
            Ok(Some(recording)) => {
                self.done = true;
                Ok(Some(recording))
            }
            Ok(None) => Ok(None),
            Err(error) => {
                self.done = true;
                Err(error)
            }
        }
    }

    /// Whether [`ArmedRecording::poll`] has handed over the recording or returned the
    /// armed recording's error: nothing more comes from this handle.
    pub fn is_done(&self) -> bool {
        self.done
    }
}

impl std::fmt::Debug for ArmedRecording {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArmedRecording")
            .field("stream_id", &self.stream_id)
            .field("channel", &self.channel)
            .field("done", &self.done)
            .finish()
    }
}

/// Whether record `id` of `counters` is, now, the counter of type `type_id` of recording
/// `recording_id`: allocated, of that type, and keyed by that id, the first eight bytes
/// of a recording counter's key (`counters.toml`, `recording.position`). A record that
/// cannot be read is not.
fn is_recordings(
    counters: &CountersReader,
    id: u32,
    type_id: CounterId,
    recording_id: i64,
) -> bool {
    matches!(
        counters.record(id),
        Ok(Some(record))
            if record.type_id() == type_id.0
                && record.key().starts_with(&recording_id.to_le_bytes())
    )
}

impl std::fmt::Debug for Recording {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Recording")
            .field("recording_id", &self.recording_id)
            .field("session_id", &self.session_id)
            .field("stream_id", &self.stream_id)
            .field("channel", &self.channel)
            .field("position", &self.position())
            .field("synced_position", &self.synced_position())
            .field("stop_position", &self.stop_position())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use cavalry_shm::counters::CountersFile;

    use super::*;

    /// A counters file of its own, in a directory this test alone uses.
    fn counters_dir(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("cavalry-recording-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create the directory");
        dir
    }

    /// The hazard the handle closes: the driver stops a recording and frees its two
    /// counters, the client's conductor is late with `recording stopped`, and the
    /// records are other counters' by the time the handle reads. The driver's side is
    /// played on a counters file directly, with a reclaim deadline of 0 so that the
    /// reuse needs no wait, and the conductor's by a stop this test stores last.
    #[test]
    fn a_position_is_never_another_counters_value_while_the_stop_is_late() {
        let dir = counters_dir("late-stop");
        let mut file = CountersFile::create(&dir, 1024).expect("create the counters file");
        let key = 7i64.to_le_bytes();
        let position = file
            .allocate(RECORDING_POSITION, &key, "recording.position 7 1 10")
            .expect("allocate");
        let synced = file
            .allocate(
                RECORDING_SYNCED_POSITION,
                &key,
                "recording.synced_position 7 1 10",
            )
            .expect("allocate");
        position.store_release(4096);
        synced.store_release(2048);
        let counters = Arc::new(CountersReader::open(&dir).expect("open the counters file"));
        let stop = Arc::new(RecordingStop::new());
        let handle = |stop: &Arc<RecordingStop>| {
            Recording::new(
                7,
                1,
                10,
                "cavalry:ipc",
                &counters,
                position.id(),
                synced.id(),
                stop,
            )
        };
        let recording = handle(&stop).expect("a live recording's handle");
        assert_eq!(
            (recording.position(), recording.synced_position()),
            (4096, 2048)
        );
        position.store_release(8192);
        assert_eq!(
            recording.position(),
            8192,
            "the live value while the record is its own"
        );

        // The stop, on the driver's side alone: the records are freed, and no stop
        // reaches the handle. The slots still hold the recorder's last values, but a
        // freed record proves nothing, so the handle returns what it read from its own.
        file.free(position.id(), 0).expect("free");
        file.free(synced.id(), 0).expect("free");
        assert_eq!(
            (recording.position(), recording.synced_position()),
            (8192, 2048)
        );
        assert_eq!(recording.stop_position(), None);

        // The records become another recording's counters, far ahead of the first.
        let other_key = 8i64.to_le_bytes();
        let other = file
            .allocate(RECORDING_POSITION, &other_key, "recording.position 8 2 10")
            .expect("allocate");
        let other_synced = file
            .allocate(
                RECORDING_SYNCED_POSITION,
                &other_key,
                "recording.synced_position 8 2 10",
            )
            .expect("allocate");
        assert_eq!(
            (other.id(), other_synced.id()),
            (position.id(), synced.id())
        );
        other.store_release(1 << 40);
        other_synced.store_release(1 << 40);
        assert_eq!(
            (recording.position(), recording.synced_position()),
            (8192, 2048),
            "another recording's values, in the records the handle holds, are not returned"
        );
        // And no handle is made on records that are another recording's while nothing
        // says where this one stopped.
        assert!(handle(&Arc::new(RecordingStop::new())).is_none());

        // The conductor catches up: the stop is the answer from here on, for this
        // handle and for one made after it, which reads no counter at all.
        stop.set(8192);
        assert_eq!(
            (recording.position(), recording.synced_position()),
            (8192, 8192)
        );
        assert_eq!(recording.stop_position(), Some(8192));
        let late = handle(&stop).expect("a stopped recording's handle");
        assert_eq!((late.position(), late.synced_position()), (8192, 8192));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
