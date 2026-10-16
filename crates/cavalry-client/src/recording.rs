//! A recording the driver writes for a client's stream: `start recording` and `stop
//! recording` of `spec/client-protocol.md` (ADR 0025). The recording belongs to the
//! driver, not to the client that started it: it outlives the client, any client may stop
//! it, and dropping a [`Recording`] stops nothing. The handle reads the recording's two
//! positions from the counters file, as `cavalry doctor` does.

use cavalry_shm::counters::CounterValue;

use crate::{Cavalry, ClientError};

/// A recording the driver started for one publication: its id in the catalog, the
/// session recorded, and its two positions as the driver stores them
/// (`spec/client-protocol.md`, "Events", `recording ready`).
pub struct Recording {
    recording_id: i64,
    session_id: i32,
    stream_id: i32,
    channel: String,
    position: CounterValue,
    synced_position: CounterValue,
}

impl Recording {
    pub(crate) fn new(
        recording_id: i64,
        session_id: i32,
        stream_id: i32,
        channel: String,
        position: CounterValue,
        synced_position: CounterValue,
    ) -> Self {
        Self {
            recording_id,
            session_id,
            stream_id,
            channel,
            position,
            synced_position,
        }
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
    /// since the publisher's limit respects it.
    pub fn position(&self) -> i64 {
        self.position.load_acquire()
    }

    /// The position up to which a sync has made the recording durable on disk: the
    /// `recording.synced_position` counter, at most a second behind [`position`]
    /// (ADR 0025).
    ///
    /// [`position`]: Recording::position
    pub fn synced_position(&self) -> i64 {
        self.synced_position.load_acquire()
    }

    /// Stops this recording through `client`, which need not be the client that started
    /// it: [`Cavalry::stop_recording`] with [`recording_id`], and the same answers (ADR
    /// 0025). The handle stays valid and its two positions stay readable at the stop.
    ///
    /// [`recording_id`]: Recording::recording_id
    pub fn stop(&self, client: &mut Cavalry) -> Result<(), ClientError> {
        client.stop_recording(self.recording_id)
    }
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
            .finish()
    }
}
