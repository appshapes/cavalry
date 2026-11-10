//! A cluster's committed log as a service reads it (`spec/cluster.md`, "The service";
//! ADR 0066): a replay that follows the recording of the log that the cluster member on
//! this driver keeps, polled no further than the member's commit position.

use std::sync::Arc;

use cavalry_proto::client_protocol::FOLLOW_LENGTH;
use cavalry_proto::cluster::SERVICE_LOG_STREAM_ID;
use cavalry_proto::frame::DataFrameHeader;
use cavalry_shm::counters::generated::{CLUSTER_COMMIT_POSITION, CLUSTER_LOG_RECORDING_ID};
use cavalry_shm::counters::{CounterId, CounterValue, CountersReader};
use cavalry_shm::log::Positions;

use crate::client::Cavalry;
use crate::error::ClientError;
use crate::replay::Replay;
use crate::subscription::{Fault, Image, Subscription};

/// The channel of that stream: the service and its member's driver are on one host.
const CHANNEL: &str = "cavalry:ipc";

/// How many fragments of other sessions on the stream one poll reads past: another
/// service's replay on the same driver, whose publication this subscription would
/// otherwise hold at its own position.
const DISCARD_LIMIT: usize = 64;

/// The member id at the start of a cluster counter's key, little-endian
/// (`counters.toml`, `cluster.commit_position`).
type MemberKey = [u8; 4];

/// Whether record `id` of `counters` is, now, the counter of type `type_id` of the
/// member whose key is `member`: allocated, of that type, and keyed by that member id.
/// A record that cannot be read is not.
fn is_members(counters: &CountersReader, id: u32, type_id: CounterId, member: MemberKey) -> bool {
    matches!(
        counters.record(id),
        Ok(Some(record))
            if record.type_id() == type_id.0 && record.key().starts_with(&member)
    )
}

/// The first allocated record of type `type_id`, of `member` when one is given, with
/// the member id its key begins with.
fn find(
    counters: &CountersReader,
    type_id: CounterId,
    member: Option<MemberKey>,
) -> Option<(u32, MemberKey)> {
    (0..counters.capacity()).find_map(|id| {
        let record = counters.record(id).ok().flatten()?;
        if record.type_id() != type_id.0 {
            return None;
        }
        let key = MemberKey::try_from(record.key().get(..size_of::<MemberKey>())?).ok()?;
        (member.is_none() || member == Some(key)).then_some((id, key))
    })
}

/// The bound of a service's reading: its member's commit position, taken only while the
/// member still names the recording the service replays (`spec/cluster.md`, "The
/// service").
struct Bound {
    counters: Arc<CountersReader>,
    member: MemberKey,
    commit: CounterValue,
    recording: CounterValue,
    recording_id: i64,
    /// The highest commit position taken from the member's counter.
    commit_position: i64,
    /// The member no longer names the recording, or a record is no longer the
    /// member's: the counters are read no more.
    lost: bool,
}

impl Bound {
    /// The bound over the first member found on `counters` that names a recording of
    /// its log, or `None` when there is none.
    ///
    /// The member's commit position is looked for first and its recording's id read
    /// only then: a member stores −1 as its recording's id before it adds its commit
    /// position, so the id read here is never the 0 a counter holds between its
    /// allocation and its first store.
    fn find(counters: &Arc<CountersReader>) -> Option<Self> {
        let (commit_id, member) = find(counters, CLUSTER_COMMIT_POSITION, None)?;
        let (recording_counter, _) = find(counters, CLUSTER_LOG_RECORDING_ID, Some(member))?;
        let commit = counters.value(commit_id).ok()?;
        let recording = counters.value(recording_counter).ok()?;
        let recording_id = recording.load_acquire();
        let named = recording_id >= 0
            && is_members(
                counters,
                recording_counter,
                CLUSTER_LOG_RECORDING_ID,
                member,
            )
            && is_members(counters, commit_id, CLUSTER_COMMIT_POSITION, member);
        named.then(|| Self {
            counters: Arc::clone(counters),
            member,
            commit,
            recording,
            recording_id,
            commit_position: 0,
            lost: false,
        })
    }

    /// The member's commit position as the bound of the next read: loaded, and taken
    /// only if the counter's record is the member's after the load and the member still
    /// names this recording. The recording's id is loaded after the commit position: a
    /// member stores another id before it stores a commit position this recording does
    /// not hold, so an id that is still this recording's says the commit position
    /// loaded before it is one of this recording's. From the first load that fails the
    /// counters are read no more: a record that has gone to another counter may come
    /// back as a later member's, and that member's commit position is of another log.
    fn load(&mut self) -> i64 {
        if !self.lost {
            let commit = self.commit.load_acquire();
            let named = is_members(
                &self.counters,
                self.commit.id(),
                CLUSTER_COMMIT_POSITION,
                self.member,
            ) && self.recording.load_acquire() == self.recording_id
                && is_members(
                    &self.counters,
                    self.recording.id(),
                    CLUSTER_LOG_RECORDING_ID,
                    self.member,
                );
            if named {
                self.commit_position = self.commit_position.max(commit);
            } else {
                self.lost = true;
            }
        }
        self.commit_position
    }
}

/// The committed log of the cluster member on this client's driver: every entry of the
/// cluster's log that a quorum of members holds, in order, each once, from the position
/// the log was opened at ([`Cavalry::committed_log`]). In Aeron, this is what a
/// clustered service is given by its container's bounded log adapter.
///
/// The log is read from the member's own recording of it, through a replay that
/// follows the recording, so it is the same on the leader and on a follower, it holds
/// only what this member has made durable, and a service may open it at any time, from
/// position 0 or from the position it had reached ([`position`](Self::position)).
/// [`poll`](Self::poll) reads no further than the member's commit position.
///
/// The commit position is taken only from the member's own counter, and only while the
/// member still names the recording this handle replays: after each load the handle
/// checks that the counter's record is still the member's and that the member's
/// `cluster.log_recording_id` is still this recording's id, and from the first check
/// that fails it reads the counter no more and keeps the highest position it took. A
/// record the driver has given to another counter, or to a later member, is never a
/// bound. From then on, and once the recording has stopped, the handle
/// [is ended](Self::is_ended): it delivers what is left below its bound and nothing
/// after it, and the service opens the log again from [`position`](Self::position).
pub struct CommittedLog {
    subscription: Subscription,
    replay: Replay,
    /// The replay publication's session: the one image this handle reads.
    session_id: i32,
    /// The image's positions, once the image has joined the subscription.
    positions: Option<Positions>,
    bound: Bound,
    /// The position after the last entry delivered.
    position: i64,
    /// The replay's image failed a check and was removed: nothing more is delivered.
    faulted: bool,
}

impl CommittedLog {
    /// Opens the committed log of the member on `client`'s driver at `position`, or
    /// `None` while the driver hosts no member that has a recording of its log.
    pub(crate) fn open(client: &mut Cavalry, position: i64) -> Result<Option<Self>, ClientError> {
        let Some(bound) = Bound::find(&client.shared_counters()) else {
            return Ok(None);
        };
        // The subscription first: a replay with no subscriber does not move.
        let subscription = client.add_subscription(CHANNEL, SERVICE_LOG_STREAM_ID)?;
        let replay = client.start_replay(
            bound.recording_id,
            position,
            FOLLOW_LENGTH,
            SERVICE_LOG_STREAM_ID,
            CHANNEL,
        )?;
        Ok(Some(Self {
            subscription,
            session_id: replay.session_id(),
            replay,
            positions: None,
            bound,
            position,
            faulted: false,
        }))
    }

    /// Reads up to `fragment_limit` entries that begin below the member's commit
    /// position, each handed to `handler` as its bytes, in `spec/cluster.md`'s
    /// encoding, its frame's header, and its position in the log. Returns how many
    /// were delivered, or the [`Fault`] that stopped the poll.
    ///
    /// An entry the leader appended as one frame arrives whole, and this version's
    /// leader appends no other. A longer entry would arrive as its fragments, in order,
    /// the header's flags saying which, as from [`Subscription::poll`]. The header's
    /// session id and stream id are the replay's and not the leader's.
    ///
    /// Per poll: two acquire loads and two reads of a counter's record for the bound,
    /// and the bounded read. Nothing here allocates, locks or makes a system call.
    pub fn poll<F>(&mut self, mut handler: F, fragment_limit: usize) -> Result<usize, Fault>
    where
        F: FnMut(&[u8], &DataFrameHeader, i64),
    {
        // Another session on the stream is read past and dropped. One that fails a
        // check is removed by that read; it is not this log, so it is no fault here.
        if let Err(Fault::Closed) = self
            .subscription
            .discard_others(self.session_id, DISCARD_LIMIT)
        {
            return Err(Fault::Closed);
        }
        let positions = match self.positions {
            Some(positions) => positions,
            None => {
                let joined = self
                    .subscription
                    .images()
                    .iter()
                    .find(|image| image.session_id() == self.session_id)
                    .map(Image::positions);
                let Some(positions) = joined else {
                    return Ok(0);
                };
                self.positions = Some(positions);
                positions
            }
        };
        let bound = self.bound.load();
        let mut next = self.position;
        let polled = self.subscription.bounded_poll(
            self.session_id,
            |body, header| {
                let position = positions.position(header.term_id(), header.term_offset() as u32);
                handler(body, header, position);
                // The frame's length is the header's 32 bytes and the body, and the next
                // frame begins at the next multiple of 32 (`spec/frame.md`).
                let aligned = (i64::from(header.frame_length()) + 31) & !31;
                next = position.saturating_add(aligned);
            },
            bound,
            fragment_limit,
        );
        self.position = next;
        if let Err(Fault::Image { .. } | Fault::ImagesFull { .. }) = polled {
            self.faulted = true;
        }
        polled
    }

    /// The position after the last entry delivered, or the position the log was opened
    /// at before the first: where a service that has applied every entry it was given
    /// opens the log again.
    pub fn position(&self) -> i64 {
        self.position
    }

    /// The bound of the last read: the highest commit position taken from the member's
    /// counter, 0 before the first poll. It never falls.
    pub fn commit_position(&self) -> i64 {
        self.bound.commit_position
    }

    /// Whether this handle has come to its end: the member no longer names the
    /// recording it replays or the member's counters are gone, as the last poll found;
    /// the recording stopped and the replay ended with it; or the replay's image
    /// failed a check. An ended handle delivers what is left below
    /// [`commit_position`](Self::commit_position) and then nothing: once a poll
    /// returns 0, open the log again from [`position`](Self::position).
    pub fn is_ended(&self) -> bool {
        self.bound.lost || self.faulted || self.replay.ended().is_some()
    }

    /// The id of the member whose log this is.
    pub fn member_id(&self) -> i32 {
        i32::from_le_bytes(self.bound.member)
    }

    /// The id of the member's recording that this handle replays.
    pub fn recording_id(&self) -> i64 {
        self.bound.recording_id
    }

    /// Stops the replay, if it has not ended, and closes the subscription. Dropping
    /// the handle closes the subscription alone, and the driver then ends the replay
    /// after the replay connect timeout, 5 s, for want of a subscriber.
    pub fn close(self, client: &mut Cavalry) -> Result<(), ClientError> {
        // A replay that ended between the look and the command is one the driver no
        // longer knows, and its refusal is the state asked for.
        let stopped = match self.replay.ended() {
            Some(_) => Ok(()),
            None => match self.replay.stop(client) {
                Err(ClientError::Refused { .. }) => Ok(()),
                other => other,
            },
        };
        self.subscription.close()?;
        stopped
    }
}

impl std::fmt::Debug for CommittedLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommittedLog")
            .field("member_id", &self.member_id())
            .field("recording_id", &self.bound.recording_id)
            .field("position", &self.position)
            .field("commit_position", &self.bound.commit_position)
            .field("is_ended", &self.is_ended())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use cavalry_shm::counters::CountersFile;
    use cavalry_shm::counters::generated::RECORDING_POSITION;

    use super::*;

    /// A counters file of its own, in a directory this test alone uses.
    fn counters_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cavalry-committed-log-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create the directory");
        dir
    }

    /// The bound is the member's commit position and nothing else's. The member's and
    /// the driver's sides are played on a counters file directly, with a reclaim
    /// deadline of 0 so that a freed record is reused with no wait.
    #[test]
    fn a_bound_is_the_members_commit_position_while_it_names_the_recording() {
        let dir = counters_dir("bound");
        let mut file = CountersFile::create(&dir, 1024).expect("create the counters file");
        let counters = Arc::new(CountersReader::open(&dir).expect("open the counters file"));
        let member = 3i32.to_le_bytes();

        // No member, and then a member in the order a consensus module adds its
        // counters: the recording's id first, −1 stored, and the commit position after.
        assert!(Bound::find(&counters).is_none(), "no member");
        let recording = file
            .allocate(
                CLUSTER_LOG_RECORDING_ID,
                &member,
                "cluster.log_recording_id 3",
            )
            .expect("allocate");
        assert_eq!(recording.get(), 0, "an allocated counter holds 0");
        assert!(
            Bound::find(&counters).is_none(),
            "a recording id with no commit position is not read: it may be the allocation's 0"
        );
        recording.store_release(-1);
        let commit = file
            .allocate(
                CLUSTER_COMMIT_POSITION,
                &member,
                "cluster.commit_position 3",
            )
            .expect("allocate");
        assert!(
            Bound::find(&counters).is_none(),
            "the member has no recording"
        );

        // The member has its recording, 0, the first of a new log directory.
        recording.store_release(0);
        commit.store_release(960);
        let mut bound = Bound::find(&counters).expect("a member that names a recording");
        assert_eq!((bound.recording_id, bound.member), (0, member));
        assert_eq!(
            bound.commit_position, 0,
            "nothing taken before the first load"
        );
        assert_eq!(bound.load(), 960);
        commit.store_release(1920);
        assert_eq!(bound.load(), 1920);
        commit.store_release(1000);
        assert_eq!(bound.load(), 1920, "the bound never falls");
        assert!(!bound.lost);

        // The member leaves the recording: another id first, and only then a commit
        // position the recording does not hold. The bound stays, for good: the same id
        // stored again later does not bring the counter back.
        let mut left = Bound::find(&counters).expect("a second reader");
        assert_eq!(left.load(), 1000);
        recording.store_release(-1);
        commit.store_release(1 << 40);
        assert_eq!(left.load(), 1000, "a commit position of another recording");
        assert!(left.lost);
        recording.store_release(0);
        assert_eq!(left.load(), 1000, "lost is for good");
        assert!(
            Bound::find(&counters).is_some(),
            "a new reader takes the member at its word again"
        );
        commit.store_release(1920);

        // The member's consensus module closes and the driver frees its counters. The
        // slots still hold the last values, but a freed record proves nothing.
        file.free(recording.id(), 0).expect("free");
        let mut freed = Bound::find(&counters);
        assert!(freed.is_none(), "a member with no recording counter");
        file.free(commit.id(), 0).expect("free");
        assert_eq!(bound.load(), 1920);
        assert!(bound.lost);
        freed.take();
        assert!(Bound::find(&counters).is_none(), "no member");

        // The records go to a later member with the same id, on a log of its own whose
        // first recording is 0 too. Its recording id counter is allocated first, in the
        // first free record, and holds the allocation's 0, which is this reader's
        // recording id, until the member stores −1.
        let later_recording = file
            .allocate(
                CLUSTER_LOG_RECORDING_ID,
                &member,
                "cluster.log_recording_id 3",
            )
            .expect("allocate");
        assert_eq!(later_recording.id(), recording.id());
        assert_eq!(later_recording.get(), 0);
        assert!(Bound::find(&counters).is_none());
        later_recording.store_release(-1);
        let later_commit = file
            .allocate(
                CLUSTER_COMMIT_POSITION,
                &member,
                "cluster.commit_position 3",
            )
            .expect("allocate");
        assert_eq!(later_commit.id(), commit.id());
        later_commit.store_release(1 << 41);
        later_recording.store_release(0);
        assert_eq!(
            bound.load(),
            1920,
            "the later member's commit position, in the records this reader holds"
        );
        let mut later = Bound::find(&counters).expect("the later member's reader");
        assert_eq!(later.load(), 1 << 41);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A reader that did not load between the free and the reuse: the records it holds
    /// are a later member's when it looks again, and the later member's recording id
    /// says so, because that member stored −1 before its commit position existed.
    #[test]
    fn a_reader_that_missed_the_free_is_stopped_by_the_recording_id() {
        let dir = counters_dir("missed");
        let mut file = CountersFile::create(&dir, 1024).expect("create the counters file");
        let counters = Arc::new(CountersReader::open(&dir).expect("open the counters file"));
        let member = 0i32.to_le_bytes();
        let recording = file
            .allocate(
                CLUSTER_LOG_RECORDING_ID,
                &member,
                "cluster.log_recording_id 0",
            )
            .expect("allocate");
        recording.store_release(-1);
        let commit = file
            .allocate(
                CLUSTER_COMMIT_POSITION,
                &member,
                "cluster.commit_position 0",
            )
            .expect("allocate");
        recording.store_release(0);
        commit.store_release(96);
        let mut bound = Bound::find(&counters).expect("a member that names a recording");
        assert_eq!(bound.load(), 96);

        // Freed and reused while the reader was away. The later member is in canvass,
        // with no recording yet, and has stored a commit position.
        file.free(recording.id(), 0).expect("free");
        file.free(commit.id(), 0).expect("free");
        let later_recording = file
            .allocate(
                CLUSTER_LOG_RECORDING_ID,
                &member,
                "cluster.log_recording_id 0",
            )
            .expect("allocate");
        later_recording.store_release(-1);
        let later_commit = file
            .allocate(
                CLUSTER_COMMIT_POSITION,
                &member,
                "cluster.commit_position 0",
            )
            .expect("allocate");
        later_commit.store_release(9600);
        assert_eq!(
            (later_recording.id(), later_commit.id()),
            (recording.id(), commit.id())
        );
        assert_eq!(bound.load(), 96);
        assert!(bound.lost);

        // And a record that went to a counter of another type is no bound either.
        let dir_other = counters_dir("other-type");
        let mut file = CountersFile::create(&dir_other, 1024).expect("create the counters file");
        let counters = Arc::new(CountersReader::open(&dir_other).expect("open the counters file"));
        let recording = file
            .allocate(
                CLUSTER_LOG_RECORDING_ID,
                &member,
                "cluster.log_recording_id 0",
            )
            .expect("allocate");
        recording.store_release(-1);
        let commit = file
            .allocate(
                CLUSTER_COMMIT_POSITION,
                &member,
                "cluster.commit_position 0",
            )
            .expect("allocate");
        recording.store_release(0);
        commit.store_release(96);
        let mut bound = Bound::find(&counters).expect("a member that names a recording");
        assert_eq!(bound.load(), 96);
        file.free(recording.id(), 0).expect("free");
        file.free(commit.id(), 0).expect("free");
        let other = file
            .allocate(
                RECORDING_POSITION,
                &0i64.to_le_bytes(),
                "recording.position 0 1 10",
            )
            .expect("allocate");
        let other_too = file
            .allocate(
                RECORDING_POSITION,
                &1i64.to_le_bytes(),
                "recording.position 1 1 10",
            )
            .expect("allocate");
        other.store_release(0);
        other_too.store_release(1 << 40);
        assert_eq!(bound.load(), 96);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir_other);
    }
}
