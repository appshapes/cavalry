//! A cluster's committed log as a service reads it (`spec/cluster.md`, "The service";
//! ADR 0066): a replay that follows the recording of the log that the cluster member on
//! this driver keeps, polled no further than the member's commit position.

use std::sync::Arc;

use cavalry_proto::client_protocol::{FOLLOW_LENGTH, ReplayStopReason};
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
/// bound. From then on the handle delivers what is left below its bound, as the replay
/// plays it, and nothing after it. Once it has delivered all of that, or all the replay
/// played up to the recording's stop when that is lower, the handle
/// [is ended](Self::is_ended), and the service opens the log again from
/// [`position`](Self::position).
///
/// Two ends are short of that, and each has its own report, read once the handle is
/// ended. An ended handle has delivered every entry it was owed only when both are
/// `None`.
///
/// The driver's replay of the recording can fail: a segment file it cannot read, or a
/// frame that fails a check. The replay ends there, with the member still running and
/// its commit position above it, and the handle is ended once it has read what the
/// replay played, or once the driver has withdrawn the image, as the next paragraph
/// says, whichever comes first. [`failed_at`](Self::failed_at) is the position the
/// replay failed at, and the entries from it on were not delivered.
///
/// The driver keeps the replay's image for the publication linger, 5 s, after the
/// replay ends, however it ended, and then withdraws it, read or not
/// (`spec/client-protocol.md`, "Events"). A service that has not read the entries
/// below its bound by then is ended short of them, and
/// [`withdrawn_at`](Self::withdrawn_at) says where the entries it was not given
/// begin. A replay that failed is withdrawn so too: both reports are then positions,
/// `withdrawn_at` the lower, and the entries from that one on were not delivered.
///
/// In every case what the handle delivered ends at [`position`](Self::position), with
/// no entry left out before it, and that is where the service opens the log again.
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
    /// The image's position after the last poll that held it: where the next frame it
    /// reads begins, which is past [`position`](Self::position) by a padding frame
    /// when one follows the last entry. The open position until the image joins.
    read: i64,
    /// The image is gone: the driver withdrew it, or it failed a check, or the replay
    /// ended before it joined this subscription. Nothing more arrives through it.
    gone: bool,
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
            read: position,
            gone: false,
            faulted: false,
        }))
    }

    /// Whether `fault` is this log's own: its image failed a check or could not join.
    /// Another session's is a fault of another service's replay, not of this log.
    fn is_own(&self, fault: &Fault) -> bool {
        matches!(
            fault,
            Fault::Image { session_id, .. } | Fault::ImagesFull { session_id }
                if *session_id == self.session_id
        )
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
    /// the bounded read, and a scan of at most the subscription's images for the
    /// image's position after it. Nothing here allocates, locks or makes a system call.
    pub fn poll<F>(&mut self, mut handler: F, fragment_limit: usize) -> Result<usize, Fault>
    where
        F: FnMut(&[u8], &DataFrameHeader, i64),
    {
        // Before the image has joined: whether the replay had ended before this poll
        // applies the driver's events. The driver announces a replay's image before the
        // replay can end, and the client's conductor hands the two over in that order,
        // so an image that is still not held once every event is applied came and went
        // before this handle saw it, or never came. One acquire load, and none once the
        // image has joined.
        let ended_before_join = self.positions.is_none() && self.replay.ended().is_some();
        // Another session on the stream is read past and dropped. One that fails a
        // check is removed by that read; it is not this log, so it is no fault here.
        // A fault of this log's own session is: its image could not join, and nothing
        // of the log will be delivered through this handle.
        let applied = match self
            .subscription
            .discard_others(self.session_id, DISCARD_LIMIT)
        {
            Ok(_) => true,
            Err(Fault::Closed) => return Err(Fault::Closed),
            Err(fault) if self.is_own(&fault) => {
                self.faulted = true;
                self.gone = true;
                return Err(fault);
            }
            // The events after another session's fault may not have been applied yet.
            Err(_) => false,
        };
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
                    if ended_before_join && applied {
                        self.gone = true;
                    }
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
        // The image's position after the read, which `is_ended` and `withdrawn_at`
        // compare. An image that joined and is no longer held has gone: the driver
        // withdrew it, at its end or at the linger's, or a frame of it failed a check.
        match self
            .subscription
            .images()
            .iter()
            .find(|image| image.session_id() == self.session_id)
        {
            Some(image) => self.read = image.position(),
            None => self.gone = true,
        }
        match polled {
            Err(fault) if self.is_own(&fault) => {
                self.faulted = true;
                Err(fault)
            }
            // Another session's image, announced since `discard_others` and refused as
            // it joined, before this log's image was read: not this log's fault, and
            // the next poll goes on.
            Err(Fault::Image { .. } | Fault::ImagesFull { .. }) => Ok(0),
            other => other,
        }
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

    /// Whether this handle has come to its end: no poll will deliver another entry.
    /// That is so once the polls have read the replay's image to
    /// [`commit_position`](Self::commit_position) after the member stopped naming the
    /// recording this handle replays, or its counters went; once the replay has ended,
    /// because its recording stopped or because the driver could not play the
    /// recording on, and the image has been read to where the replay ended; once the
    /// replay's image failed a check; or once the driver has withdrawn the image.
    ///
    /// What an ended handle has delivered is said by two reports, read after this
    /// returns true. With [`failed_at`](Self::failed_at) and
    /// [`withdrawn_at`](Self::withdrawn_at) both `None`, and an image that passed
    /// every check, it has delivered every entry below the bound it took, or below the
    /// recording's stop when that is lower: open the log again from
    /// [`position`](Self::position) when the member names a recording again. With
    /// `failed_at` a position, the driver's replay failed there, and the entries from
    /// it on were not delivered, though the member is still running and may have
    /// committed them. With `withdrawn_at` a position, the entries from it on were
    /// played and not delivered. Both can be positions: the driver withdraws a replay
    /// that failed as it does any other, and `withdrawn_at` is then the lower, so the
    /// entries from it on, those below `failed_at` among them, were not delivered. In
    /// every case what the handle delivered ends at [`position`](Self::position).
    ///
    /// Until then a poll that returns 0 says only that nothing below the bound has
    /// arrived yet. A replay plays no further than what the member's recorder has
    /// synced, once a second, and a follower's commit position comes from its leader,
    /// so the bound may be ahead of the replay by a second's entries; and when the
    /// member leaves the log or closes, the replay plays on to the recording's stop.
    ///
    /// The image's position is the one compared, not [`position`](Self::position): a
    /// padding frame at a term's end moves it and is handed to no handler. The image's
    /// position is the one the last poll left, so a call reads no shared memory but
    /// the replay's end, one acquire load.
    pub fn is_ended(&self) -> bool {
        self.faulted
            || self.gone
            || (self.bound.lost && self.read >= self.bound.commit_position)
            || self.replay.ended().is_some_and(|(end, _)| self.read >= end)
    }

    /// Where the entries this handle was not given begin, when the driver withdrew the
    /// replay's image before the polls had read it to its end, or to the bound once the
    /// member stopped naming the recording, whichever is lower; `None` otherwise. The
    /// driver keeps a replay's image for the publication linger, 5 s, after the replay
    /// ends, and then withdraws it whether or not its subscriber has read it
    /// (`spec/client-protocol.md`, "Events"): a service that did not poll for that long
    /// after its member closed or left the log, or whose handler took that long, ends
    /// so. [`is_ended`](Self::is_ended) is true by then, and the position is
    /// [`position`](Self::position), or past it by a padding frame, which holds no
    /// entry.
    ///
    /// The entries from there on were played and not delivered, and nothing else
    /// delivers them. Open the log again from [`position`](Self::position): a member
    /// that still names the same recording plays them again. A member that left the
    /// log and joined it again names a new recording, which begins above them, and the
    /// open is refused with code 23 ([`Cavalry::committed_log`]); they are in the
    /// recording this handle replayed, [`recording_id`](Self::recording_id), and on
    /// every member that has not left the log.
    pub fn withdrawn_at(&self) -> Option<i64> {
        if !self.gone || self.faulted {
            return None;
        }
        // The replay's end is known by the time its image goes: the driver sends
        // `replay stopped` first. An end not known is taken as no end at all, so that
        // a short end is never taken for a whole one.
        let mut end = self.replay.ended().map_or(i64::MAX, |(end, _)| end);
        if self.bound.lost {
            end = end.min(self.bound.commit_position);
        }
        (self.read < end).then_some(self.read)
    }

    /// Where the driver's replay of the member's recording failed, when it did: a
    /// segment file could not be read or a frame failed a check, after the frame that
    /// ends at this position (`spec/client-protocol.md`, "Events", `replay stopped`,
    /// reason 3). `None` for a log that is live or that ended because its member
    /// closed or left the recording. A log opened at a position where no entry begins
    /// ends so at once, at that position, with nothing delivered, since the bytes
    /// there are no frame; a log that read entries before it failed is a recording
    /// whose files are damaged, which `cavalry doctor` names. Either way no entry from
    /// the position on is delivered. The entries below it may still be on their way
    /// when this first says so: read it once [`is_ended`](Self::is_ended) does. They
    /// have all been delivered by then unless the driver withdrew the replay's image
    /// first, which it does 5 s after a replay ends, one that failed included:
    /// [`withdrawn_at`](Self::withdrawn_at) is then a position below this one, and the
    /// entries from there on were not delivered either. What the handle delivered
    /// ends at [`position`](Self::position), with no entry left out before it.
    pub fn failed_at(&self) -> Option<i64> {
        match self.replay.ended() {
            Some((position, Some(ReplayStopReason::Failed))) => Some(position),
            _ => None,
        }
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
            .field("failed_at", &self.failed_at())
            .field("withdrawn_at", &self.withdrawn_at())
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
