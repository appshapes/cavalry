//! A cluster in memory: every member's [`Consensus`] with its I/O played by one
//! struct, on a clock the caller steps a millisecond at a time. No driver, no socket
//! and no file: the consensus stream is a queue, a leader's log publication is a list
//! of entries, a member's recording is the entries that have reached it, a log of its
//! own, and its sync is a flag. It is what the crate's example and its tests run, and
//! what a test of a service can run before it runs on three drivers.
//!
//! It is a test double, not a transport: it allocates, and nothing of it is on a
//! message's path in a running member. What it plays faithfully is what the core
//! depends on: a consensus message arrives whole, late or never; a member started
//! again, on an empty log directory or on the one it kept, reads none that was in
//! flight to it before, so none that a peer sent before that start (`spec/cluster.md`,
//! "Taking a leader"; ADR 0145); a follower's image of
//! the log joins at the position the leader's publication is at when the follower
//! subscribes; a member's append position is what its recording has synced; the
//! leader's own append position rises by its own sync, like any member's; and every
//! member holds the bytes that reached it and no others, so two members can hold
//! different bytes at one position, which a test of the cluster's safety must be able
//! to find. A leader's replay of its own log reaches a member that subscribed on its
//! catch-up endpoint, an entry after another from the position asked and no further
//! than the leader's recording has synced, and ends by itself at its end, when it is
//! stopped, or when it has had no subscriber for the replay connect timeout.
//!
//! A member's truncation cuts its log in a driver's order, the recordings from the
//! last back and then its record of terms, and [`Cluster::fault_truncation`] makes one
//! fault part way: the log and the record of terms are then cut as far as its steps
//! went, and the end the member reports is as it was, as a driver leaves them (ADR
//! 0148). A member started again on the log it kept ends its log where its
//! entries end, as a start that finishes such a truncation does (ADR 0108).
//!
//! Its bounds, and what it does at each:
//!
//! - The consensus stream holds at most [`IN_FLIGHT_CAPACITY`] messages in flight,
//!   allocated when the cluster is made. A message sent while that many are in flight
//!   is lost, as a datagram is when the socket buffer has no room for it, and counted
//!   in [`Cluster::full_queue_losses`]. Every consensus message is sent again at its
//!   interval, so a loss delays the cluster and never stops it.
//! - A message is late by at most [`MAX_DELAY_MS`], the leader heartbeat timeout;
//!   [`Cluster::set_delivery`] holds a longer delay to it.
//! - A member runs at most one replay to each other member: one started to a member
//!   that has one takes its place, and one that stopped is dropped when the member that
//!   started it asks how it ended. So eight members hold 56 replays at most.
//! - The logs and the records of terms have no bound: a log publication holds every
//!   entry appended to it for the cluster's life, each member's log holds every entry
//!   that reached it, and a member's record of terms holds every term it records. A
//!   test that offers a million entries to three members holds four million.

use std::collections::VecDeque;

use cavalry_proto::cluster::{LEADER_HEARTBEAT_TIMEOUT_NS, Message, Stream, entry_position};

use crate::consensus::{
    Clock, Consensus, Counts, ElectionState, Io, Log, MembersRefused, Refused, ReplayStopped, Role,
    base_above, term_at_or_below, terms_above,
};

/// The MTU length the cluster's log positions are computed with: the UDP transport's
/// (`spec/udp.md`).
const MTU: usize = 1408;

/// How long a consensus message takes with no delay asked: one step.
const STEP_NS: u64 = 1_000_000;

/// The most consensus messages in flight at once, between every pair of members. Eight
/// members send at most 112 messages in a step, one append position or canvass
/// position and one commit position or new leadership term each way between each of
/// the 28 pairs, so the queue holds more than 36 steps of the most they send, and every
/// message of the crate's tests with the delays they set. A message sent while it is
/// full is lost and counted ([`Cluster::full_queue_losses`]).
pub const IN_FLIGHT_CAPACITY: usize = 4_096;

/// The most a consensus message is late, in milliseconds: the leader heartbeat timeout
/// (`spec/cluster.md`, "Values"). A message later than that is one the member that
/// waits for it has already given up on, which loss plays as well.
pub const MAX_DELAY_MS: u64 = LEADER_HEARTBEAT_TIMEOUT_NS / STEP_NS;

/// How long an image outlives the last datagram of its stream: the transport's image
/// liveness timeout (`spec/udp.md`).
const IMAGE_LIVENESS_TIMEOUT_NS: u64 = 10_000_000_000;

/// How long a replay runs with no subscriber before it ends: the driver's replay
/// connect timeout (`spec/client-protocol.md`, "Values").
const REPLAY_CONNECT_TIMEOUT_NS: u64 = 5_000_000_000;

/// The most new leadership terms the in-memory cluster holds back at once, and the
/// most pairs it holds them back for: one for each ordered pair of the most members a
/// cluster has would be more than a test needs.
pub const MAX_HELD_BACK: usize = 8;

/// One consensus message between two members.
struct Flight {
    deliver_at_ns: u64,
    to: usize,
    bytes: Vec<u8>,
}

/// One entry of a log: its bytes and the positions it lies between.
#[derive(Clone)]
struct Entry {
    position: i64,
    position_after: i64,
    bytes: Vec<u8>,
}

/// A leader's log publication, one for each term a member leads: what
/// [`Io::add_log`] makes.
struct Publication {
    session_id: i32,
    /// The member that added it, by its place among the members.
    leader: usize,
    /// The position after its last entry: the term's base before the first.
    position: i64,
    /// Every entry appended to it, for the cluster's life.
    entries: Vec<Entry>,
    /// Gone: the member that added it was started again, on an empty log directory
    /// ([`Cluster::wipe`]) or on the one it kept ([`Cluster::restart`]), or gave its log
    /// up to hold an election ([`Io::remove_log`]).
    closed: bool,
}

/// A member's image of a log publication.
#[derive(Clone, Copy)]
struct Image {
    /// The publication's place in the world's list.
    publication: usize,
    /// Where the image joined: the publication's position when the member first saw it.
    join_position: i64,
    /// The next of the publication's entries to reach the member.
    next: usize,
}

/// A replay of a member's own log to another member's catch-up subscription: what
/// [`Io::start_replay`] makes. One at most from one member to another.
struct Replay {
    /// The member that replays and the one it replays to, by their places.
    from: usize,
    to: usize,
    /// Where it began, the join position of its image.
    start: i64,
    /// Where the next entry it sends begins, and where it ends: a replication's
    /// replay at its end, a catch-up's, with none, when it is stopped or the member
    /// it replays gives its log up.
    position: i64,
    end: Option<i64>,
    /// Since when it has had no subscriber it reaches.
    unheard_since_ns: Option<u64>,
    /// Whether its image has reached the member's subscription.
    joined: bool,
    /// Whether the member records its image, `None` until the image has reached the
    /// subscription the member holds now: the image that reaches a subscription first
    /// takes the recording armed on it, and one that comes after finds none and
    /// brings the member's log nothing, as over a driver, which arms a recording only
    /// on a stream with no image (`spec/client-protocol.md`, "Commands", start
    /// recording; ADR 0092).
    recorded: Option<bool>,
    /// Asked to stop: it stops on the next step.
    stopping: bool,
    /// How it ended, until the member that started it has asked.
    stopped: Option<ReplayStopped>,
}

/// A partition between two members, by their places among the members.
struct Cut {
    a: usize,
    b: usize,
    since_ns: u64,
    /// Whether it cuts one way: nothing `a` sends reaches `b`, and what `b` sends
    /// still reaches `a`.
    one_way: bool,
}

/// What the world holds for one member: what its `terms` file and its recording would.
#[derive(Clone)]
struct Disk {
    id: i32,
    candidate_term: i64,
    /// The vote floor its `terms` file holds, −1 for none or once cleared (ADR 0145, ADR
    /// 0161).
    vote_floor: i64,
    terms: Vec<(i64, i64)>,
    /// Subscribed to the log, with a recording armed.
    joined: bool,
    /// Its image of the leader's log publication, once it has one.
    image: Option<Image>,
    /// The log it holds: every entry that has reached its recordings, in position
    /// order.
    log: Vec<Entry>,
    /// The position up to which the log has reached its recording: the position
    /// after the last entry of `log`.
    received: i64,
    /// The position up to which its recording is synced: its append position.
    synced: i64,
    sync_paused: bool,
    /// Where one of its recordings of the log ends and the next begins, below where
    /// its log ends: what [`Cluster::end_recording`] marked. A replication's replay
    /// of its log ends at the first of these above where the replay began.
    recording_ends: Vec<i64>,
    /// When its cut began, for the image liveness timeout.
    cut_since_ns: Option<u64>,
    /// Its image of the log is gone, until it leaves the log and joins again.
    image_lost: bool,
    halted: bool,
    /// Its consensus module stands still while its driver runs ([`Cluster::stall`]).
    stalled: bool,
    /// Subscribed on its catch-up endpoint.
    catchup: bool,
    /// Whether the recording armed on that subscription still waits for an image:
    /// from [`Io::add_catchup`] until the first image reaches the subscription.
    catchup_armed: bool,
    /// The join position of the replay's image the member records on that
    /// subscription.
    catchup_image: Option<i64>,
    /// Where its next truncations fault, and for how many of them in a row
    /// ([`Cluster::fault_truncation`]).
    truncation_fault: Option<(TruncationFault, u32)>,
    /// A truncation of its log faulted and none has finished since: its log is cut
    /// short of the end it reports (ADR 0148).
    cut_short: bool,
    /// It added a log, joined one or subscribed for a replay while `cut_short`, which
    /// ADR 0148 says a member never does: what a test of it reads.
    took_a_log_cut_short: bool,
}

impl Disk {
    /// A new log directory: no term, no vote, no recording.
    fn empty(id: i32) -> Self {
        Self {
            id,
            candidate_term: -1,
            vote_floor: -1,
            terms: Vec::new(),
            joined: false,
            image: None,
            log: Vec::new(),
            received: 0,
            synced: 0,
            sync_paused: false,
            recording_ends: Vec::new(),
            cut_since_ns: None,
            image_lost: false,
            halted: false,
            stalled: false,
            catchup: false,
            catchup_armed: false,
            catchup_image: None,
            truncation_fault: None,
            cut_short: false,
            took_a_log_cut_short: false,
        }
    }
}

/// Where a member's truncation faults ([`Cluster::fault_truncation`]): one of the
/// places at which a driver's command or a file write can fail part way through
/// (`spec/cluster.md`, "Messages during an election"; ADR 0095, ADR 0148). The
/// truncation is a list of steps: a purge of each recording that begins above the
/// position, from the last back, where a recording begins at 0, at each term's base
/// and at each end [`Cluster::end_recording`] marked; then the cut of the one that
/// holds the position, when anything above it is left; then the member's log
/// recordings file; then its record of terms.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TruncationFault {
    /// At the second step, the first done: the purge of the last recording, or with
    /// none to purge the cut, or with nothing to cut the file.
    SecondStep,
    /// At the file, every command done.
    List,
    /// At the record of terms, every command and the file done, and the highest of
    /// the terms based above the position deleted, where there is one.
    Terms,
}

/// A step of the I/O that faulted: a truncation [`Cluster::fault_truncation`] made fail.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Faulted;

/// Everything outside the cores.
struct World {
    clock: Clock,
    disks: Vec<Disk>,
    /// Every consensus message in flight, at most [`IN_FLIGHT_CAPACITY`], in the order
    /// sent.
    net: VecDeque<Flight>,
    /// The messages lost because `net` was full.
    full_queue_losses: u64,
    /// The pairs, `(from, to)` by member index, whose next new leadership term is held
    /// back, which [`Cluster::hold_back_new_leadership_term`] arms: at most
    /// [`MAX_HELD_BACK`].
    hold_back: Vec<(usize, usize)>,
    /// The new leadership terms held back, each written by its sender and kept out of
    /// `net` until [`Cluster::release_held_back`]: at most [`MAX_HELD_BACK`].
    held_back: Vec<Flight>,
    /// The most entries a replay brings in one step, which [`Cluster::set_replay_rate`]
    /// sets: `usize::MAX`, all it can reach, unless a test asks for fewer.
    replay_rate: usize,
    /// Every log publication a member added, for the cluster's life, in the order
    /// added.
    publications: Vec<Publication>,
    /// The partitions between two members that are set now.
    cuts: Vec<Cut>,
    /// The replays started and not yet asked about after they stopped: one at most
    /// from one member to another.
    replays: Vec<Replay>,
    back_pressured: bool,
    loss_one_in: u64,
    max_delay_ms: u64,
    random: u64,
}

impl World {
    /// Xorshift64: the same faults at every run of a test.
    fn next_random(&mut self) -> u64 {
        self.random ^= self.random << 13;
        self.random ^= self.random >> 7;
        self.random ^= self.random << 17;
        self.random
    }

    /// The log publication the member at `leader` appends to: the last it added that
    /// is not gone.
    fn publication_of(&self, leader: usize) -> Option<usize> {
        self.publications
            .iter()
            .rposition(|publication| publication.leader == leader && !publication.closed)
    }
}

/// Since when nothing the member at `from` sends reaches the one at `to`: the
/// earliest of the cuts set now that stop it, a cut of either from everyone, a
/// partition between the two, or one that cuts one way from `from` to `to`. `None`
/// while `to` hears `from`.
fn unheard_since(disks: &[Disk], cuts: &[Cut], from: usize, to: usize) -> Option<u64> {
    let own = [from, to]
        .into_iter()
        .filter_map(|index| disks.get(index).and_then(|disk| disk.cut_since_ns));
    let between = cuts
        .iter()
        .filter(|cut| {
            (cut.a == from && cut.b == to) || (!cut.one_way && cut.a == to && cut.b == from)
        })
        .map(|cut| cut.since_ns);
    own.chain(between).min()
}

/// One member's [`Io`], for the length of one call into its core.
struct MemberIo<'a> {
    world: &'a mut World,
    own: usize,
}

impl Io for MemberIo<'_> {
    type Fault = Faulted;

    fn send(&mut self, member: i32, message: &Message<'_>) -> bool {
        let Some(to) = self.world.disks.iter().position(|disk| disk.id == member) else {
            return false;
        };
        let mut buffer = [0u8; 256];
        let Ok(length) = message.write(&mut buffer) else {
            return false;
        };
        // The new leadership term a test holds back is written and not sent, as a
        // sender holds a frame back while its receiver's image is a receiver window
        // behind, and it reaches the member when the test releases it.
        if matches!(message, Message::NewLeadershipTerm { .. })
            && let Some(armed) = self
                .world
                .hold_back
                .iter()
                .position(|&pair| pair == (self.own, to))
            && self.world.held_back.len() < MAX_HELD_BACK
        {
            self.world.hold_back.swap_remove(armed);
            self.world.held_back.push(Flight {
                deliver_at_ns: 0,
                to,
                bytes: buffer.get(..length).unwrap_or(&[]).to_vec(),
            });
            return true;
        }
        // A cut or halted member's messages leave it and arrive nowhere, as a
        // datagram's do, and so do the ones between two members a partition parts
        // and the ones the delivery setting loses.
        let halted = [self.own, to]
            .into_iter()
            .any(|index| self.world.disks.get(index).is_none_or(|disk| disk.halted));
        let cut =
            halted || unheard_since(&self.world.disks, &self.world.cuts, self.own, to).is_some();
        let lost = self.world.loss_one_in > 0
            && self
                .world
                .next_random()
                .is_multiple_of(self.world.loss_one_in);
        if cut || lost {
            return true;
        }
        if self.world.net.len() >= IN_FLIGHT_CAPACITY {
            // Full: lost, as a datagram the socket buffer has no room for is, and
            // counted. The core sends it again at its interval.
            self.world.full_queue_losses = self.world.full_queue_losses.saturating_add(1);
            return true;
        }
        // `set_delivery` holds the delay to MAX_DELAY_MS, so the sum below cannot
        // wrap; it saturates all the same.
        let delay_ms = if self.world.max_delay_ms == 0 {
            0
        } else {
            self.world.next_random() % self.world.max_delay_ms.saturating_add(1)
        };
        self.world.net.push_back(Flight {
            deliver_at_ns: self
                .world
                .clock
                .now_ns
                .saturating_add(STEP_NS.saturating_mul(delay_ms.saturating_add(1))),
            to,
            bytes: buffer.get(..length).unwrap_or(&[]).to_vec(),
        });
        true
    }

    fn store_candidate_term(&mut self, term: i64) -> Result<(), Self::Fault> {
        if let Some(disk) = self.world.disks.get_mut(self.own) {
            disk.candidate_term = term;
        }
        Ok(())
    }

    fn record_term(&mut self, term: i64, base: i64) -> Result<(), Self::Fault> {
        if let Some(disk) = self.world.disks.get_mut(self.own) {
            disk.terms.push((term, base));
        }
        Ok(())
    }

    fn candidate_term(&mut self) -> i64 {
        self.world
            .disks
            .get(self.own)
            .map_or(-1, |disk| disk.candidate_term)
    }

    fn log_leadership_term_id(&mut self) -> i64 {
        self.world
            .disks
            .get(self.own)
            .and_then(|disk| disk.terms.iter().map(|&(term, _)| term).max())
            .unwrap_or(-1)
    }

    fn vote_floor(&mut self) -> i64 {
        self.world
            .disks
            .get(self.own)
            .map_or(-1, |disk| disk.vote_floor)
    }

    fn store_vote_floor(&mut self, position: i64) -> Result<(), Self::Fault> {
        if let Some(disk) = self.world.disks.get_mut(self.own) {
            disk.vote_floor = position;
        }
        Ok(())
    }

    fn clear_vote_floor(&mut self) -> Result<(), Self::Fault> {
        if let Some(disk) = self.world.disks.get_mut(self.own) {
            disk.vote_floor = -1;
        }
        Ok(())
    }

    fn add_log(&mut self, term: i64, base: i64) -> Result<Option<Log>, Self::Fault> {
        if let Some(disk) = self.world.disks.get_mut(self.own) {
            disk.took_a_log_cut_short |= disk.cut_short;
        }
        let session_id = 1_000 + i32::try_from(term).unwrap_or(0);
        self.world.publications.push(Publication {
            session_id,
            leader: self.own,
            position: base,
            entries: Vec::new(),
            closed: false,
        });
        Ok(Some(Log {
            session_id,
            recording_id: 0,
        }))
    }

    fn join_log(&mut self) -> Result<(), Self::Fault> {
        if let Some(disk) = self.world.disks.get_mut(self.own) {
            disk.took_a_log_cut_short |= disk.cut_short;
            disk.joined = true;
        }
        Ok(())
    }

    fn log_image(&mut self, session_id: i32) -> Result<Option<i64>, Self::Fault> {
        let now_ns = self.world.clock.now_ns;
        // The publication with this session id that is not gone, the last added.
        let Some((index, publication)) = self
            .world
            .publications
            .iter()
            .enumerate()
            .rfind(|(_, publication)| publication.session_id == session_id && !publication.closed)
        else {
            return Ok(None);
        };
        let (leader, position, entries) = (
            publication.leader,
            publication.position,
            publication.entries.len(),
        );
        let parted = unheard_since(&self.world.disks, &self.world.cuts, leader, self.own);
        let Some(disk) = self.world.disks.get_mut(self.own) else {
            return Ok(None);
        };
        if !disk.joined || disk.image_lost {
            return Ok(None);
        }
        let image = disk.image.filter(|image| image.publication == index);
        if let Some(since_ns) = parted {
            // Nothing of the log arrives: no image comes, and one already there is
            // gone when the image liveness timeout has passed.
            if now_ns.saturating_sub(since_ns) >= IMAGE_LIVENESS_TIMEOUT_NS {
                disk.image_lost = image.is_some();
                return Ok(None);
            }
            return Ok(image.map(|image| image.join_position));
        }
        // The image joins where the publication is when the subscriber first sees it,
        // and the entries appended from then on are the ones that reach it.
        let image = image.unwrap_or(Image {
            publication: index,
            join_position: position,
            next: entries,
        });
        disk.image = Some(image);
        Ok(Some(image.join_position))
    }

    // A recording in memory stops where it stands, so each of the three calls that
    // stop one is done when it returns.
    fn leave_log(&mut self) -> Result<bool, Self::Fault> {
        if let Some(disk) = self.world.disks.get_mut(self.own) {
            disk.joined = false;
            disk.image = None;
            disk.image_lost = false;
        }
        Ok(true)
    }

    fn remove_log(&mut self) -> Result<bool, Self::Fault> {
        // The member's log publication is gone, and with it every follower's image of
        // it: nothing more of it reaches a member, and `log_image` finds no
        // publication with its session id from this step on, where a transport takes
        // the image liveness timeout at most. What the member appended stays in its
        // own log.
        for publication in &mut self.world.publications {
            if publication.leader == self.own {
                publication.closed = true;
            }
        }
        Ok(true)
    }

    fn truncate_log(&mut self, position: i64) -> Result<Option<i64>, Self::Fault> {
        let Some(disk) = self.world.disks.get_mut(self.own) else {
            return Ok(Some(-1));
        };
        // A driver's order: a purge of each recording that begins above the position,
        // the last first, then the cut of the one that holds it, then the file, then
        // the record of terms (ADR 0095, ADR 0108). A recording begins at 0, at each
        // term's base and at each end `end_recording` marked.
        let mut starts: Vec<i64> = disk
            .terms
            .iter()
            .map(|&(_, base)| base)
            .chain(disk.recording_ends.iter().copied())
            .filter(|&start| start > position && start < disk.received)
            .collect();
        starts.sort_unstable_by(|a, b| b.cmp(a));
        starts.dedup();
        let armed = disk
            .truncation_fault
            .filter(|&(_, times)| times > 0)
            .map(|(at, _)| at);
        // A fault at the second step comes once one step is done, whichever step is
        // next.
        let second = armed == Some(TruncationFault::SecondStep);
        let mut done = 0_usize;
        let mut faulted = false;
        for start in starts {
            if second && done == 1 {
                faulted = true;
                break;
            }
            disk.log.retain(|entry| entry.position < start);
            done += 1;
        }
        if !faulted && disk.log.iter().any(|entry| entry.position_after > position) {
            if second && done == 1 {
                faulted = true;
            } else {
                disk.log.retain(|entry| entry.position_after <= position);
                done += 1;
            }
        }
        // The file, which this model has no copy of: its step is a place to fault.
        if !faulted {
            if (second && done == 1) || armed == Some(TruncationFault::List) {
                faulted = true;
            } else {
                done += 1;
            }
        }
        // The record of terms, from the highest term down.
        if !faulted && ((second && done == 1) || armed == Some(TruncationFault::Terms)) {
            if armed == Some(TruncationFault::Terms)
                && let Some(highest) = disk
                    .terms
                    .iter()
                    .enumerate()
                    .filter(|(_, (_, base))| *base > position)
                    .max_by_key(|(_, (term, _))| *term)
                    .map(|(index, _)| index)
            {
                disk.terms.remove(highest);
            }
            faulted = true;
        }
        if faulted {
            // The log and the record of terms are cut as far as the steps went; the
            // end the member reports and what it received are as they were.
            if let Some((at, times)) = disk.truncation_fault {
                disk.truncation_fault = (times > 1).then_some((at, times - 1));
            }
            disk.cut_short = true;
            return Err(Faulted);
        }
        // Every entry that ends above the position is gone, read or synced or
        // neither, and every term recorded at a base above it.
        disk.log.retain(|entry| entry.position_after <= position);
        disk.received = disk.received.min(position);
        disk.synced = disk.synced.min(position);
        disk.terms.retain(|&(_, base)| base <= position);
        disk.recording_ends.retain(|&end| end < position);
        disk.cut_short = false;
        Ok(Some(
            disk.terms.iter().map(|&(term, _)| term).max().unwrap_or(-1),
        ))
    }

    fn append_position(&mut self) -> i64 {
        self.world.disks.get(self.own).map_or(0, |disk| disk.synced)
    }

    fn terms_after(&mut self, term: i64) -> [Option<(i64, i64)>; 2] {
        let record = self.world.disks.get(self.own).map(|disk| &disk.terms);
        terms_above(record.into_iter().flatten().copied(), term)
    }

    fn term_base_above(&mut self, position: i64) -> Option<i64> {
        let record = self.world.disks.get(self.own).map(|disk| &disk.terms);
        base_above(record.into_iter().flatten().copied(), position)
    }

    fn term_at_or_below(&mut self, term: i64) -> i64 {
        let record = self.world.disks.get(self.own).map(|disk| &disk.terms);
        term_at_or_below(record.into_iter().flatten().copied(), term)
    }

    fn drop_terms_above(&mut self, term: i64) -> Result<i64, Self::Fault> {
        let Some(disk) = self.world.disks.get_mut(self.own) else {
            return Ok(-1);
        };
        disk.terms.retain(|&(recorded, _)| recorded <= term);
        Ok(disk
            .terms
            .iter()
            .map(|&(recorded, _)| recorded)
            .max()
            .unwrap_or(-1))
    }

    fn start_replay(
        &mut self,
        member: i32,
        position: i64,
        end: Option<i64>,
    ) -> Result<bool, Self::Fault> {
        let Some(to) = self.world.disks.iter().position(|disk| disk.id == member) else {
            return Ok(false);
        };
        // The member's log holds the position when an entry of it begins there, and
        // a replication's replay needs that entry to end at or below its end. A
        // catch-up's replay, with no end, follows the log as it grows, so it may
        // begin where the log ends now. A log that holds neither is one whose
        // recordings were purged, or one that differs from the asker's below the
        // position: nothing is started.
        let held = self.world.disks.get(self.own).is_some_and(|disk| {
            let begins = disk.log.iter().any(|entry| {
                entry.position == position && end.is_none_or(|end| entry.position_after <= end)
            });
            begins || (end.is_none() && position == disk.received)
        });
        if !held {
            return Ok(false);
        }
        // How an earlier replay to the member ended is forgotten.
        let from = self.own;
        self.world
            .replays
            .retain(|replay| !(replay.from == from && replay.to == to));
        self.world.replays.push(Replay {
            from,
            to,
            start: position,
            position,
            end,
            unheard_since_ns: None,
            joined: false,
            recorded: None,
            stopping: false,
            stopped: None,
        });
        Ok(true)
    }

    fn stop_replay(&mut self, member: i32) -> Result<(), Self::Fault> {
        let to = self.world.disks.iter().position(|disk| disk.id == member);
        for replay in &mut self.world.replays {
            if replay.from == self.own && Some(replay.to) == to && replay.stopped.is_none() {
                replay.stopping = true;
            }
        }
        Ok(())
    }

    fn replay_stopped(&mut self, member: i32) -> Result<Option<ReplayStopped>, Self::Fault> {
        let to = self.world.disks.iter().position(|disk| disk.id == member);
        let place = self.world.replays.iter().position(|replay| {
            replay.from == self.own && Some(replay.to) == to && replay.stopped.is_some()
        });
        Ok(place.and_then(|place| self.world.replays.swap_remove(place).stopped))
    }

    fn add_catchup(&mut self) -> Result<(), Self::Fault> {
        // A new subscription: an image a replay brings to it is new to it.
        for replay in &mut self.world.replays {
            if replay.to == self.own {
                replay.recorded = None;
            }
        }
        if let Some(disk) = self.world.disks.get_mut(self.own) {
            disk.took_a_log_cut_short |= disk.cut_short;
            disk.catchup = true;
            disk.catchup_armed = true;
            disk.catchup_image = None;
            // The recording armed here begins where the member's log ends, its append
            // position. What it received above that and never synced, which needs a
            // sync that was paused, it never reported and no leader knows of: it is
            // not in its log from here (ADR 0086). A member over a driver holds no
            // such bytes: its recording's stop is where its log ends.
            let synced = disk.synced;
            disk.log.retain(|entry| entry.position_after <= synced);
            disk.received = synced;
        }
        Ok(())
    }

    fn send_catchup_position(&mut self, leader: i32, term: i64, position: i64) -> bool {
        let follower_member_id = self.world.disks.get(self.own).map_or(-1, |disk| disk.id);
        // This cluster has no endpoints: a replay reaches the member by its place.
        self.send(
            leader,
            &Message::CatchupPosition {
                leadership_term_id: term,
                log_position: position,
                follower_member_id,
                catchup_endpoint: b"memory:0",
            },
        )
    }

    fn catchup_image(&mut self) -> Result<Option<i64>, Self::Fault> {
        Ok(self
            .world
            .disks
            .get(self.own)
            .and_then(|disk| disk.catchup_image))
    }

    fn remove_catchup(&mut self) -> Result<bool, Self::Fault> {
        // The subscription goes, with the recording armed on it and every image.
        for replay in &mut self.world.replays {
            if replay.to == self.own {
                replay.recorded = None;
            }
        }
        if let Some(disk) = self.world.disks.get_mut(self.own) {
            disk.catchup = false;
            disk.catchup_armed = false;
            disk.catchup_image = None;
        }
        Ok(true)
    }

    fn append(&mut self, entry: &Message<'_>) -> Result<bool, Self::Fault> {
        if self.world.back_pressured {
            return Ok(false);
        }
        let mut bytes = vec![0u8; entry.encoded_length()];
        let Ok(length) = entry.write(&mut bytes) else {
            return Ok(false);
        };
        // The leader's own publication, and its own recording of it.
        let Some(publication) = self
            .world
            .publication_of(self.own)
            .and_then(|index| self.world.publications.get_mut(index))
        else {
            return Ok(false);
        };
        let Some(after) = entry_position(publication.position, length, MTU) else {
            return Ok(false);
        };
        let entry = Entry {
            position: publication.position,
            position_after: after.position_after,
            bytes,
        };
        publication.position = after.position_after;
        publication.entries.push(entry.clone());
        if let Some(disk) = self.world.disks.get_mut(self.own) {
            disk.received = after.position_after;
            disk.log.push(entry);
        }
        Ok(true)
    }
}

/// A cluster of members in one process, each a [`Consensus`], with the world between
/// them in memory. Members are named by their member ids. A getter for an id no member
/// has returns `None`, and a fault set on one does nothing.
pub struct Cluster {
    cores: Vec<Consensus>,
    world: World,
    /// The seed each member's own is made from.
    seed: u64,
    /// The members that each member's own members string marks a standby, by the
    /// member's place: one string for all when the cluster is made, and a member's
    /// own from [`Cluster::restart_with`] on (ADR 0134).
    standbys: Vec<Vec<i32>>,
}

impl Cluster {
    /// A cluster of the members with `member_ids` that elect their leader
    /// ([`Consensus::with_election`]; ADR 0077), every member started, at time 0.
    /// Each member's nomination delays come from `seed` and its place among the
    /// members, so that no two members draw the same delays and a seed gives the same
    /// elections at every run. Which member leads depends on the seed:
    /// [`Cluster::leader`] says.
    pub fn with_election(member_ids: &[i32], seed: u64) -> Result<Self, MembersRefused> {
        Self::with_standbys(member_ids, &[], seed)
    }

    /// A cluster of the members with `member_ids`, of which those with `standby_ids`
    /// are standbys in every member's members string ([`Consensus::with_standbys`];
    /// `spec/cluster.md`, "Standbys"; ADR 0134), otherwise as
    /// [`Cluster::with_election`] makes it. An id of `standby_ids` that is no
    /// member's marks nothing.
    ///
    /// ```
    /// use cavalry_raft::memory::Cluster;
    ///
    /// // Three voters and a standby: the standby follows, and never leads.
    /// let mut cluster = Cluster::with_standbys(&[0, 1, 2, 3], &[3], 1).expect("four members");
    /// cluster.run_ms(2_000);
    /// let leader = cluster.leader().expect("the voters elect a leader");
    /// assert_ne!(leader, 3);
    /// assert!(cluster.member(3).expect("a member").is_standby());
    /// ```
    pub fn with_standbys(
        member_ids: &[i32],
        standby_ids: &[i32],
        seed: u64,
    ) -> Result<Self, MembersRefused> {
        let cores = (0..member_ids.len())
            .map(|index| Self::core(member_ids, standby_ids, index, seed))
            .collect::<Result<Vec<_>, _>>()?;
        let disks = member_ids.iter().map(|&id| Disk::empty(id)).collect();
        Ok(Self {
            cores,
            seed,
            standbys: vec![standby_ids.to_vec(); member_ids.len()],
            world: World {
                clock: Clock {
                    now_ns: 0,
                    time_ms: 1_700_000_000_000,
                },
                disks,
                net: VecDeque::with_capacity(IN_FLIGHT_CAPACITY),
                full_queue_losses: 0,
                hold_back: Vec::with_capacity(MAX_HELD_BACK),
                held_back: Vec::with_capacity(MAX_HELD_BACK),
                replay_rate: usize::MAX,
                publications: Vec::new(),
                cuts: Vec::new(),
                replays: Vec::new(),
                back_pressured: false,
                loss_one_in: 0,
                max_delay_ms: 0,
                random: 0x9E37_79B9_7F4A_7C15,
            },
        })
    }

    /// The core of the member at `index` among `member_ids`, whose own members string
    /// marks those of `standby_ids` standbys.
    fn core(
        member_ids: &[i32],
        standby_ids: &[i32],
        index: usize,
        seed: u64,
    ) -> Result<Consensus, MembersRefused> {
        let id = member_ids.get(index).copied().unwrap_or(-1);
        let (standbys, voters): (Vec<i32>, Vec<i32>) = member_ids
            .iter()
            .partition(|&&member| standby_ids.contains(&member));
        // One seed for each member: the cluster's, moved on by the member's place.
        Consensus::with_standbys(
            &voters,
            &standbys,
            id,
            seed.wrapping_add(1)
                .wrapping_mul(0x9E37_79B9_7F4A_7C15)
                .wrapping_add((index as u64).wrapping_mul(0xD1B5_4A32_D192_ED03)),
        )
    }

    fn index(&self, member: i32) -> Option<usize> {
        self.world.disks.iter().position(|disk| disk.id == member)
    }

    fn disk_mut(&mut self, member: i32) -> Option<&mut Disk> {
        self.world.disks.iter_mut().find(|disk| disk.id == member)
    }

    /// One millisecond: the log reaches every follower on the leader's image, each
    /// recording syncs, the consensus messages due arrive, and every member runs one
    /// duty cycle.
    pub fn step(&mut self) {
        self.world.clock.now_ns = self.world.clock.now_ns.saturating_add(STEP_NS);
        self.world.clock.time_ms = self.world.clock.time_ms.saturating_add(1);
        self.replay();
        for index in 0..self.world.disks.len() {
            // The image a recording extends the member's log from. A recording holds
            // the image from its join position: only one that joined at or below the
            // end of what the member held extends its contiguous log.
            let image = self.world.disks.get(index).and_then(|disk| {
                disk.image.filter(|image| {
                    disk.joined
                        && !disk.halted
                        && !disk.image_lost
                        && image.join_position <= disk.received
                })
            });
            let reaches = image.and_then(|image| {
                let publication = self.world.publications.get(image.publication)?;
                let parted = unheard_since(
                    &self.world.disks,
                    &self.world.cuts,
                    publication.leader,
                    index,
                );
                (!publication.closed && parted.is_none()).then_some(image)
            });
            if let Some(mut image) = reaches
                && let Some(publication) = self.world.publications.get(image.publication)
                && let Some(disk) = self.world.disks.get_mut(index)
            {
                // Every entry appended since the last step, each one at the end of what
                // the member holds; an entry that begins below it is one the member
                // holds already, in whatever bytes it has there. An entry that begins
                // below what the member holds and ends above it, which needs a member
                // that received past what it synced and then joined a publication whose
                // entries end elsewhere, is skipped too, and the next is taken after a
                // gap: the member's log is then not the publication's, which is what
                // the safety check of a test finds. The election's truncation cuts the
                // member's log to where it joins.
                for entry in publication.entries.iter().skip(image.next) {
                    if entry.position >= disk.received {
                        disk.received = entry.position_after;
                        disk.log.push(entry.clone());
                    }
                }
                image.next = publication.entries.len();
                disk.image = Some(image);
            }
            if let Some(disk) = self.world.disks.get_mut(index)
                && !disk.sync_paused
                && !disk.halted
            {
                disk.synced = disk.received;
            }
        }
        let now_ns = self.world.clock.now_ns;
        // The messages due are taken out where they stand, so that the queue's length
        // is every message in flight, the bound `send` holds, and the ones still
        // waiting stay ahead of any sent while these arrive. A message sent now is due
        // at the next step at the soonest, so the walk ends.
        // The members whose message faulted this step: a member over a driver runs no
        // more of that duty cycle, its core's own work included (`ConsensusModule::
        // do_work`), so neither does one here.
        let mut faulted = vec![false; self.world.disks.len()];
        let mut index = 0;
        while index < self.world.net.len() {
            let due = self
                .world
                .net
                .get(index)
                .is_some_and(|flight| flight.deliver_at_ns <= now_ns);
            // A message for a member whose module stands still waits where it is, as
            // it does in the image the member's driver holds for it, and is read in
            // the order sent when the module runs again.
            let waits = self.world.net.get(index).is_some_and(|flight| {
                self.world
                    .disks
                    .get(flight.to)
                    .is_some_and(|disk| disk.stalled)
            });
            if !due || waits {
                index += 1;
                continue;
            }
            let Some(flight) = self.world.net.remove(index) else {
                break;
            };
            let halted = self
                .world
                .disks
                .get(flight.to)
                .is_none_or(|disk| disk.halted);
            if halted {
                continue;
            }
            if let (Some(core), Ok(message)) = (
                self.cores.get_mut(flight.to),
                Message::parse(Stream::Consensus, &flight.bytes),
            ) {
                let clock = self.world.clock;
                let mut io = MemberIo {
                    world: &mut self.world,
                    own: flight.to,
                };
                // A fault, of a truncation this test made fail, leaves the member as
                // the I/O left it. The rest of the step's messages are read, as the rest
                // of a poll's batch is, and the member's own work waits for the next
                // step, as `cavalry run` runs its next cycle.
                if core.on_message(clock, &message, &mut io).is_err()
                    && let Some(slot) = faulted.get_mut(flight.to)
                {
                    *slot = true;
                }
            }
        }
        for (own, core) in self.cores.iter_mut().enumerate() {
            if self
                .world
                .disks
                .get(own)
                .is_none_or(|disk| disk.halted || disk.stalled)
                || faulted.get(own).copied().unwrap_or(false)
            {
                continue;
            }
            let clock = self.world.clock;
            let mut io = MemberIo {
                world: &mut self.world,
                own,
            };
            // A fault, as above: the member's next cycle tries again.
            if core.do_work(clock, &mut io).is_err() {
                continue;
            }
        }
    }

    /// One step of every replay: a replay asked to stop stops; one whose member is
    /// subscribed on its catch-up endpoint and hears the replaying member brings it
    /// every entry the replaying member's recording has synced, from where it stands
    /// to its end, and ends by itself on the step after; a catch-up's, with no end,
    /// goes on bringing what the replaying member appends and syncs, for as long as
    /// that member holds its log publication: a replay of a live recording reads to
    /// the synced position and waits for it to move (`spec/client-protocol.md`,
    /// "Commands"), so what a leader has appended and not yet synced reaches a
    /// member by the live log alone; one with no
    /// such subscriber for the replay connect timeout ends where it stands. A replay
    /// from a halted member stands still, as its host does.
    fn replay(&mut self) {
        let now_ns = self.world.clock.now_ns;
        let World {
            disks,
            cuts,
            replays,
            publications,
            replay_rate,
            ..
        } = &mut self.world;
        let rate = *replay_rate;
        for replay in replays.iter_mut() {
            if replay.stopped.is_some() || disks.get(replay.from).is_none_or(|disk| disk.halted) {
                continue;
            }
            let heard = unheard_since(disks, cuts, replay.from, replay.to).is_none();
            let subscribed = heard
                && disks
                    .get(replay.to)
                    .is_some_and(|disk| disk.catchup && !disk.halted);
            let mut ended = None;
            if replay.stopping {
                ended = Some(false);
            } else if replay.joined && replay.end.is_some_and(|end| replay.position >= end) {
                // At its end, a step after its last entry, so that the member sees
                // the image it came on; with or without its subscriber by now.
                ended = Some(true);
            } else if !subscribed {
                let since = *replay.unheard_since_ns.get_or_insert(now_ns);
                if now_ns.saturating_sub(since) >= REPLAY_CONNECT_TIMEOUT_NS {
                    ended = Some(false);
                }
            } else {
                replay.unheard_since_ns = None;
                // The entries of the replaying member's log from where the replay
                // stands to its end. The image joined at the replay's start, and its
                // recording extends the member's log only where it is contiguous
                // with it: an entry that begins where the member's log ends.
                let entries: Vec<Entry> = disks
                    .get(replay.from)
                    .map(|disk| {
                        // A replication's replay is of one recording: it ends where
                        // the recording it began in ends, when that is below its end.
                        let recording_end = disk
                            .recording_ends
                            .iter()
                            .copied()
                            .filter(|&end| replay.end.is_some() && end > replay.start)
                            .min();
                        disk.log
                            .iter()
                            .filter(|entry| {
                                entry.position >= replay.position
                                    && entry.position_after <= disk.synced
                                    && replay.end.is_none_or(|end| entry.position_after <= end)
                                    && recording_end.is_none_or(|end| entry.position_after <= end)
                            })
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default();
                // A catch-up's replay follows the live recording for as long as the
                // replaying member holds its log publication.
                let follows = replay.end.is_none()
                    && publications.iter().any(|publication| {
                        publication.leader == replay.from && !publication.closed
                    });
                if replay.joined && entries.is_empty() && !follows {
                    // The end of what the replaying member holds below the replay's
                    // end, or of a live recording that stopped: its recording's end.
                    ended = Some(true);
                } else if let Some(disk) = disks.get_mut(replay.to) {
                    replay.joined = true;
                    // The image takes the recording armed on the subscription, when
                    // one still waits; an image that finds none is not recorded.
                    let recorded = *replay
                        .recorded
                        .get_or_insert_with(|| std::mem::take(&mut disk.catchup_armed));
                    if recorded {
                        disk.catchup_image = Some(replay.start);
                    }
                    for entry in entries.into_iter().take(rate) {
                        replay.position = entry.position_after;
                        if recorded && entry.position == disk.received {
                            disk.received = entry.position_after;
                            disk.log.push(entry);
                        }
                    }
                }
            }
            if let Some(ended) = ended {
                replay.stopped = Some(ReplayStopped {
                    position: replay.position,
                    ended,
                });
                // The replay's image goes with it.
                if replay.recorded == Some(true)
                    && let Some(disk) = disks.get_mut(replay.to)
                    && disk.catchup_image == Some(replay.start)
                {
                    disk.catchup_image = None;
                }
            }
        }
    }

    /// `milliseconds` steps.
    pub fn run_ms(&mut self, milliseconds: u64) {
        for _ in 0..milliseconds {
            self.step();
        }
    }

    /// The member's core, for everything its getters say.
    pub fn member(&self, member: i32) -> Option<&Consensus> {
        self.index(member).and_then(|index| self.cores.get(index))
    }

    /// The member's role.
    pub fn role(&self, member: i32) -> Option<Role> {
        self.member(member).map(Consensus::role)
    }

    /// The state of the member's election.
    pub fn election_state(&self, member: i32) -> Option<ElectionState> {
        self.member(member).map(Consensus::election_state)
    }

    /// The commit position the member knows.
    pub fn commit_position(&self, member: i32) -> Option<i64> {
        self.member(member).map(Consensus::commit_position)
    }

    /// The member's append position: what its recording has synced.
    pub fn append_position(&self, member: i32) -> Option<i64> {
        self.index(member)
            .and_then(|index| self.world.disks.get(index))
            .map(|disk| disk.synced)
    }

    /// What the member has counted.
    pub fn counts(&self, member: i32) -> Option<Counts> {
        self.member(member).map(Consensus::counts)
    }

    /// The terms in the member's record, each with its term base log position, and its
    /// stored candidate term.
    pub fn record_of_terms(&self, member: i32) -> Option<(Vec<(i64, i64)>, i64)> {
        self.index(member)
            .and_then(|index| self.world.disks.get(index))
            .map(|disk| (disk.terms.clone(), disk.candidate_term))
    }

    /// The bytes of the log the member's recordings hold: every entry that has reached
    /// the member, encoded as `spec/cluster.md` gives it, one after another in position
    /// order. They are the member's own: two members that were sent different entries
    /// at one position return different bytes. Empty for a member that holds none.
    pub fn log(&self, member: i32) -> Vec<u8> {
        let mut bytes = Vec::new();
        if let Some(disk) = self
            .index(member)
            .and_then(|index| self.world.disks.get(index))
        {
            for entry in &disk.log {
                bytes.extend_from_slice(&entry.bytes);
            }
        }
        bytes
    }

    /// The bytes of the member's log up to `position`: every entry it holds that ends
    /// at or below that position, as [`Cluster::log`] gives them. With a commit
    /// position, the entries the member holds that are committed.
    pub fn log_to(&self, member: i32, position: i64) -> Vec<u8> {
        let mut bytes = Vec::new();
        if let Some(disk) = self
            .index(member)
            .and_then(|index| self.world.disks.get(index))
        {
            for entry in disk
                .log
                .iter()
                .take_while(|entry| entry.position_after <= position)
            {
                bytes.extend_from_slice(&entry.bytes);
            }
        }
        bytes
    }

    /// The member that leads: the running member whose role is leader, never one that
    /// is halted or stalled. When two are, a leader that was cut off or stood still
    /// and has not yet given up the lead beside the one elected after it, it is the
    /// one of the higher term. `None` while no running member leads, during an
    /// election.
    pub fn leader(&self) -> Option<i32> {
        self.cores
            .iter()
            .zip(&self.world.disks)
            .filter(|(core, disk)| core.role() == Role::Leader && !disk.halted && !disk.stalled)
            .max_by_key(|(core, _)| core.leadership_term_id())
            .map(|(core, _)| core.member_id())
    }

    /// Offers `payload` to the leader as its own process does, and returns the
    /// position after the entry ([`Consensus::offer`]). The leader is
    /// [`Cluster::leader`]: an offer made while no member leads is refused as not the
    /// leader's.
    pub fn offer(&mut self, payload: &[u8]) -> Result<i64, Refused> {
        let leader = self.leader().unwrap_or(-1);
        let Some(own) = self.index(leader) else {
            return Err(Refused::NotLeader);
        };
        let Some(core) = self.cores.get_mut(own) else {
            return Err(Refused::NotLeader);
        };
        let clock = self.world.clock;
        let mut io = MemberIo {
            world: &mut self.world,
            own,
        };
        // An offer truncates nothing, so no step of it faults here; one that did
        // would leave the entry unoffered, which back-pressure says too.
        let Ok(outcome) = core.offer(clock, payload, &mut io) else {
            return Err(Refused::BackPressured);
        };
        outcome.map(|()| {
            self.world
                .publication_of(own)
                .and_then(|index| self.world.publications.get(index))
                .map_or(0, |publication| publication.position)
        })
    }

    /// Has `member` append a timer event for the timer `correlation_id`, as a member
    /// does for a timer it holds whose deadline its clock has reached, with the
    /// cluster's time `skew_ms` after the cluster's clock, which stands for a
    /// member's own wall clock; and returns the position at which the entry begins
    /// ([`Consensus::timer_event`]). `member` need not be [`Cluster::leader`]: a
    /// member that led, was cut off and has not yet given up the lead appends to its
    /// own log, which no other member holds. Refused as [`Consensus::offer`] refuses,
    /// and as not the leader's for a member that is halted or stalled.
    pub fn timer_event(
        &mut self,
        member: i32,
        correlation_id: i64,
        skew_ms: i64,
    ) -> Result<i64, Refused> {
        self.append_from(member, |core, mut clock, io| {
            clock.time_ms = clock.time_ms.saturating_add(skew_ms);
            core.timer_event(clock, correlation_id, io)
        })
    }

    fn append_from(
        &mut self,
        member: i32,
        append: impl FnOnce(
            &mut Consensus,
            Clock,
            &mut MemberIo<'_>,
        ) -> Result<Result<(), Refused>, Faulted>,
    ) -> Result<i64, Refused> {
        let Some(own) = self.index(member) else {
            return Err(Refused::NotLeader);
        };
        if self
            .world
            .disks
            .get(own)
            .is_none_or(|disk| disk.halted || disk.stalled)
        {
            return Err(Refused::NotLeader);
        }
        let Some(core) = self.cores.get_mut(own) else {
            return Err(Refused::NotLeader);
        };
        let begins = self
            .world
            .publication_of(own)
            .and_then(|index| self.world.publications.get(index))
            .map_or(0, |publication| publication.position);
        let clock = self.world.clock;
        let mut io = MemberIo {
            world: &mut self.world,
            own,
        };
        // An append truncates nothing, so no step of it faults here; one that did
        // would leave the entry unappended, which back-pressure says too.
        let Ok(outcome) = append(core, clock, &mut io) else {
            return Err(Refused::BackPressured);
        };
        outcome.map(|()| begins)
    }

    /// The entries of the member's log that begin at or above `from` and end at or
    /// below `to`, each the position at which it begins, the position after it and
    /// its bytes, encoded as `spec/cluster.md` gives it. With a commit position as
    /// `to`, the committed entries a service beside the member reads from its own
    /// position.
    pub fn entries(&self, member: i32, from: i64, to: i64) -> Vec<(i64, i64, Vec<u8>)> {
        self.index(member)
            .and_then(|index| self.world.disks.get(index))
            .map(|disk| {
                disk.log
                    .iter()
                    .filter(|entry| entry.position >= from && entry.position_after <= to)
                    .map(|entry| (entry.position, entry.position_after, entry.bytes.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The position after the last entry the member's log holds, 0 for an empty log:
    /// where a cut left it.
    pub fn log_end(&self, member: i32) -> i64 {
        self.index(member)
            .and_then(|index| self.world.disks.get(index))
            .and_then(|disk| disk.log.last())
            .map_or(0, |entry| entry.position_after)
    }

    /// The cluster's time on the cluster's one clock, in milliseconds.
    pub fn time_ms(&self) -> i64 {
        self.world.clock.time_ms
    }

    /// Cuts the member off: its consensus messages, sent and to come, arrive nowhere,
    /// and the log stops reaching it. Its duty cycle still runs. An image of the log it
    /// holds stays for the image liveness timeout, 10 s, and what the cut kept from it
    /// arrives when the cut ends sooner, as a retransmission would bring it.
    ///
    /// When the member leads, its log stops reaching every follower too: what it
    /// appends while cut extends its own log and no other member's, and each
    /// follower's image of it stays for the image liveness timeout, as in a
    /// [`Cluster::cut_between`], and is brought what the cut kept from it when the cut
    /// ends sooner.
    pub fn cut(&mut self, member: i32) {
        let now_ns = self.world.clock.now_ns;
        if let Some(disk) = self.disk_mut(member) {
            disk.cut_since_ns.get_or_insert(now_ns);
        }
    }

    /// Ends a cut.
    pub fn heal(&mut self, member: i32) {
        if let Some(disk) = self.disk_mut(member) {
            disk.cut_since_ns = None;
        }
    }

    /// Parts two members from each other and from nobody else: the consensus messages
    /// between them, sent and to come, arrive nowhere, and when one of them leads, its
    /// log stops reaching the other, whose image of it stays for the image liveness
    /// timeout as it does in a [`Cluster::cut`]. Each still reaches every other
    /// member. Parting a member from itself, or naming an id no member has, does
    /// nothing.
    pub fn cut_between(&mut self, member: i32, other: i32) {
        let (Some(a), Some(b)) = (self.index(member), self.index(other)) else {
            return;
        };
        let parted =
            self.world.cuts.iter().any(|cut| {
                !cut.one_way && ((cut.a == a && cut.b == b) || (cut.a == b && cut.b == a))
            });
        if a != b && !parted {
            let since_ns = self.world.clock.now_ns;
            self.world.cuts.push(Cut {
                a,
                b,
                since_ns,
                one_way: false,
            });
        }
    }

    /// Stops what `member` sends from reaching `other`, and nothing else: a partition
    /// that cuts one way, as a host has when its packets out are dropped on one path.
    /// `member`'s consensus messages to `other` arrive nowhere, and when `member`
    /// leads, its log stops reaching `other`, whose image of it stays for the image
    /// liveness timeout as it does in a [`Cluster::cut`]. `member` still hears
    /// `other`, and each still reaches every other member. [`Cluster::heal_between`]
    /// ends it. Naming one member twice, or an id no member has, does nothing.
    ///
    /// The UDP transport holds such a path for the connection timeout, 5 s, at most:
    /// `other`, hearing no status message from `member`, disconnects its publications
    /// to it (`spec/udp.md`). This cluster has no status messages and holds the cut
    /// for as long as a test asks, which is how a test brings a member a message of a
    /// term it took no part in.
    pub fn cut_one_way(&mut self, member: i32, other: i32) {
        let (Some(a), Some(b)) = (self.index(member), self.index(other)) else {
            return;
        };
        let cut = self
            .world
            .cuts
            .iter()
            .any(|cut| cut.one_way && cut.a == a && cut.b == b);
        if a != b && !cut {
            let since_ns = self.world.clock.now_ns;
            self.world.cuts.push(Cut {
                a,
                b,
                since_ns,
                one_way: true,
            });
        }
    }

    /// Ends every partition between two members, the ones that cut both ways and the
    /// ones that cut one way, in either direction.
    pub fn heal_between(&mut self, member: i32, other: i32) {
        let (Some(a), Some(b)) = (self.index(member), self.index(other)) else {
            return;
        };
        self.world
            .cuts
            .retain(|cut| !((cut.a == a && cut.b == b) || (cut.a == b && cut.b == a)));
    }

    /// Starts the member again on an empty log directory, as a container whose volume
    /// was not kept does: its core is new and its terms, its vote and its recordings
    /// are gone. When it led, its log publication is gone with it, and every
    /// follower's image of it. Every consensus message in flight to it is lost, so the
    /// new core reads none that a peer sent before this start, as on the new driver
    /// such a member starts on (`spec/cluster.md`, "Taking a leader"; ADR 0145). What
    /// the cluster does about it is the test. A member starts on the log it kept with
    /// [`Cluster::restart`].
    pub fn wipe(&mut self, member: i32) {
        let Some(index) = self.index(member) else {
            return;
        };
        self.lose_in_flight_to(index);
        let ids: Vec<i32> = self.world.disks.iter().map(|disk| disk.id).collect();
        let standbys = self.standbys.get(index).cloned().unwrap_or_default();
        let led = self
            .cores
            .get(index)
            .is_some_and(|core| core.role() == Role::Leader);
        if let (Some(core), Ok(new)) = (
            self.cores.get_mut(index),
            Self::core(&ids, &standbys, index, self.seed),
        ) {
            *core = new;
        }
        if let Some(disk) = self.world.disks.get_mut(index) {
            *disk = Disk::empty(member);
        }
        // Its replays are gone with its recordings, and the ones to it with its
        // subscription.
        self.world
            .replays
            .retain(|replay| replay.from != index && replay.to != index);
        if led {
            for publication in &mut self.world.publications {
                if publication.leader == index {
                    publication.closed = true;
                }
            }
            for disk in &mut self.world.disks {
                disk.image_lost = disk.image.is_some();
            }
        }
    }

    /// Starts the member again on the log directory it kept, as a host that lost power
    /// and came back does: its core is new, and its log directory holds the candidate
    /// term it stored, its record of terms and its recordings up to the position they
    /// had synced. What had reached it and was not synced is gone, and so are its
    /// subscriptions, its image of the log and every replay it ran or received. When
    /// it led, its log publication is gone with it, and every follower's image of it.
    /// Every consensus message in flight to it is lost, as at [`Cluster::wipe`]: the
    /// host's driver started again with it, and each peer's stream begins in a new
    /// image at the peer's sender position. A member that was halted or stalled runs
    /// again, and its counts begin at 0. Its members string marks the standbys it
    /// marked before.
    pub fn restart(&mut self, member: i32) {
        let Some(index) = self.index(member) else {
            return;
        };
        let standbys = self.standbys.get(index).cloned().unwrap_or_default();
        // The marks the member ran with made a core before, so they make one now.
        let _ = self.restart_with(member, &standbys);
    }

    /// Starts the member again on the log directory it kept, as [`Cluster::restart`]
    /// does, with a members string that marks the members with `standby_ids` its
    /// standbys: how a standby is promoted and how a voter is made a standby
    /// (`spec/cluster.md`, "Changing a standby's mark"; ADR 0134). The other members
    /// keep the strings they run with, so two members can hold two strings, as they
    /// do while an operator starts them again one after another. Marks that leave no
    /// voter are refused, and the member runs on as it was.
    pub fn restart_with(&mut self, member: i32, standby_ids: &[i32]) -> Result<(), MembersRefused> {
        let Some(index) = self.index(member) else {
            return Ok(());
        };
        let ids: Vec<i32> = self.world.disks.iter().map(|disk| disk.id).collect();
        let new = Self::core(&ids, standby_ids, index, self.seed)?;
        let led = self
            .cores
            .get(index)
            .is_some_and(|core| core.role() == Role::Leader);
        if let Some(core) = self.cores.get_mut(index) {
            *core = new;
        }
        self.lose_in_flight_to(index);
        if let Some(marks) = self.standbys.get_mut(index) {
            *marks = standby_ids.to_vec();
        }
        if let Some(disk) = self.world.disks.get_mut(index) {
            let synced = disk.synced;
            disk.log.retain(|entry| entry.position_after <= synced);
            // A truncation that faulted left the log shorter than the end the member
            // reported: the start ends the log where its entries end, and deletes from
            // its record every term based above that, as a start over a driver makes a
            // log directory agree with itself after a stop or a fault inside a
            // truncation (ADR 0108, ADR 0148). With no such truncation nothing changes:
            // the entries end at the synced position, and no term is based above it,
            // since a member records a term only at a base its append position, what
            // its recording has synced, has reached.
            let end = disk.log.last().map_or(0, |entry| entry.position_after);
            disk.synced = synced.min(end);
            disk.terms.retain(|&(_, base)| base <= end);
            disk.cut_short = false;
            disk.received = disk.synced;
            disk.joined = false;
            disk.image = None;
            disk.image_lost = false;
            disk.sync_paused = false;
            disk.halted = false;
            disk.stalled = false;
            disk.catchup = false;
            disk.catchup_armed = false;
            disk.catchup_image = None;
        }
        self.world
            .replays
            .retain(|replay| replay.from != index && replay.to != index);
        if led {
            for publication in &mut self.world.publications {
                if publication.leader == index {
                    publication.closed = true;
                }
            }
            for disk in &mut self.world.disks {
                disk.image_lost = disk.image.is_some();
            }
        }
        Ok(())
    }

    /// Loses every consensus message in flight to the member at `index`, as a member
    /// started again on a new driver loses them: each peer's stream begins in a new
    /// image, made by the peer's setup frame at its sender position, and a datagram of
    /// the image before is dropped (`spec/udp.md`, "The receiver", steps 1 and 2). The
    /// messages in flight to every other member stay where they are.
    fn lose_in_flight_to(&mut self, index: usize) {
        self.world.net.retain(|flight| flight.to != index);
    }

    /// Takes the member's image of the log away, as the transport does when the
    /// image's stream goes silent: nothing more of the log reaches the member until it
    /// has left the log and joined it again.
    pub fn lose_image(&mut self, member: i32) {
        if let Some(disk) = self.disk_mut(member) {
            disk.image_lost = disk.image.is_some();
        }
    }

    /// Stops the member as a stopped process is: no duty cycle, no message read, no
    /// log received and nothing synced, until [`Cluster::resume`]. A member halted
    /// before the first step and resumed later is a member that starts late.
    pub fn halt(&mut self, member: i32) {
        if let Some(disk) = self.disk_mut(member) {
            disk.halted = true;
        }
    }

    /// Lets a halted member run again.
    pub fn resume(&mut self, member: i32) {
        if let Some(disk) = self.disk_mut(member) {
            disk.halted = false;
        }
    }

    /// Stops the member's consensus module and leaves its driver running, as a process
    /// that stands still under a driver that does not: no duty cycle and no message
    /// read, while the log still reaches the member's recording and syncs, and every
    /// consensus message sent to the member waits, as it does in the image its driver
    /// holds, and is read in the order sent when [`Cluster::unstall`] lets the module
    /// run. A [`Cluster::halt`] is the host that stops, driver and module, and loses
    /// what is sent to it. The messages that wait are in flight: they count toward
    /// [`IN_FLIGHT_CAPACITY`], and one sent while that many are in flight is lost and
    /// counted, as any is.
    ///
    /// [`Cluster::offer`] goes to [`Cluster::leader`], which is never a stalled or a
    /// halted member.
    pub fn stall(&mut self, member: i32) {
        if let Some(disk) = self.disk_mut(member) {
            disk.stalled = true;
        }
    }

    /// Lets a stalled member's module run again: on the next step it reads what
    /// waited for it, the oldest first, and then runs its duty cycle.
    pub fn unstall(&mut self, member: i32) {
        if let Some(disk) = self.disk_mut(member) {
            disk.stalled = false;
        }
    }

    /// Ends the member's recording of the log where its log ends now, so that what
    /// reaches it after is in another recording: what a member that left the log and
    /// joined it again within a term holds, a term in two recordings
    /// (`spec/cluster.md`, "The log"). The member's log is the same bytes. A
    /// replication's replay of its log is of one recording, and ends there (ADR
    /// 0092).
    pub fn end_recording(&mut self, member: i32) {
        if let Some(disk) = self.disk_mut(member) {
            let end = disk.received;
            if end > 0 && !disk.recording_ends.contains(&end) {
                disk.recording_ends.push(end);
            }
        }
    }

    /// Makes the member's next `times` truncations of its log fault `at` the step it
    /// names, as a driver that refuses a command, or does not answer it in time,
    /// leaves one (ADR 0148). Each truncation that faults cuts the member's log and its
    /// record of terms as far as its steps went, and leaves the end the member reports
    /// as it was; the member owes it, and asks for it again. A truncation the
    /// member asks for after the last of them, or one with too few steps to reach a
    /// second, finishes.
    pub fn fault_truncation(&mut self, member: i32, at: TruncationFault, times: u32) {
        if let Some(disk) = self.disk_mut(member) {
            disk.truncation_fault = (times > 0).then_some((at, times));
        }
    }

    /// Whether a truncation of the member's log faulted and none has finished since:
    /// its log is cut short of the end it reports. A start on the log it kept ends
    /// that ([`Cluster::restart`]).
    pub fn log_cut_short(&self, member: i32) -> bool {
        self.world
            .disks
            .iter()
            .any(|disk| disk.id == member && disk.cut_short)
    }

    /// Whether the member, at any time since it last started on an empty log
    /// directory, added a log, joined one or subscribed for a replay while its log
    /// was cut short: what ADR 0148 says a member never does, since that log or
    /// replay begins at an end the member no longer holds.
    pub fn took_a_log_cut_short(&self, member: i32) -> bool {
        self.world
            .disks
            .iter()
            .any(|disk| disk.id == member && disk.took_a_log_cut_short)
    }

    /// The position at which the member's log has a gap: an entry that begins past the
    /// end of the one before it, or a first entry that does not begin at 0. `None`
    /// while its log is whole. A member over a driver whose log had one would list a
    /// recording that begins past the end of the one before it, which its next start
    /// refuses (`listed-recording`; ADR 0108).
    pub fn log_gap(&self, member: i32) -> Option<i64> {
        let disk = self.world.disks.iter().find(|disk| disk.id == member)?;
        let mut end = 0;
        for entry in &disk.log {
            if entry.position != end {
                return Some(end);
            }
            end = entry.position_after;
        }
        None
    }

    /// Stops the member's recording from syncing, so that its append position stays
    /// where it is while the log keeps reaching it.
    pub fn pause_sync(&mut self, member: i32) {
        if let Some(disk) = self.disk_mut(member) {
            disk.sync_paused = true;
        }
    }

    /// Lets the member's recording sync again.
    pub fn resume_sync(&mut self, member: i32) {
        if let Some(disk) = self.disk_mut(member) {
            disk.sync_paused = false;
        }
    }

    /// Back-pressures the leader's log publication, or ends that: while set, an
    /// append writes nothing.
    pub fn back_pressure(&mut self, on: bool) {
        self.world.back_pressured = on;
    }

    /// Holds back the next new leadership term member `from` sends member `to`: the
    /// sender writes it and does not send it, as a sender holds a frame back while its
    /// receiver's image is a receiver window behind, until [`Cluster::release_held_back`]
    /// hands it to `to`, whatever ran meanwhile, a wipe and a restart of `to` among it.
    /// That is the frame of `spec/cluster.md`, "Taking a leader", that a member started
    /// again reads though it was written before its start (ADR 0145, ADR 0161). One is
    /// held for each pair armed, and a pair armed again while it is armed is armed
    /// once; past [`MAX_HELD_BACK`] pairs a hold is not armed.
    pub fn hold_back_new_leadership_term(&mut self, from: i32, to: i32) {
        let index = |id: i32| self.world.disks.iter().position(|disk| disk.id == id);
        if let (Some(from), Some(to)) = (index(from), index(to))
            && !self.world.hold_back.contains(&(from, to))
            && self.world.hold_back.len() < MAX_HELD_BACK
        {
            self.world.hold_back.push((from, to));
        }
    }

    /// The new leadership terms held back, each as its sender, its member, its log
    /// position and its leadership term id, in the order written.
    pub fn held_back(&self) -> Vec<(i32, i32, i64, i64)> {
        self.world
            .held_back
            .iter()
            .filter_map(|flight| {
                let to = self.world.disks.get(flight.to)?.id;
                match Message::parse(Stream::Consensus, &flight.bytes) {
                    Ok(Message::NewLeadershipTerm {
                        log_position,
                        leadership_term_id,
                        leader_member_id,
                        ..
                    }) => Some((leader_member_id, to, log_position, leadership_term_id)),
                    _ => None,
                }
            })
            .collect()
    }

    /// Hands the new leadership terms held back for member `to` to it, due at the next
    /// step, as the sender sends a frame it held to the image the member has now: a
    /// member started again since gets it in its new image (`spec/udp.md`, "The
    /// receiver"). Released to a member in init, one is read as the in-memory cluster
    /// reads any message there (work item 458), so a test releases them once the member
    /// canvasses. The holds still armed are disarmed.
    pub fn release_held_back(&mut self, to: i32) {
        let Some(to) = self.world.disks.iter().position(|disk| disk.id == to) else {
            return;
        };
        let due = self.world.clock.now_ns.saturating_add(STEP_NS);
        let mut kept = Vec::with_capacity(MAX_HELD_BACK);
        for mut flight in self.world.held_back.drain(..) {
            if flight.to != to {
                kept.push(flight);
            } else if self.world.net.len() >= IN_FLIGHT_CAPACITY {
                // Full: lost, as `send` loses a message the queue has no room for.
                self.world.full_queue_losses = self.world.full_queue_losses.saturating_add(1);
            } else {
                flight.deliver_at_ns = due;
                self.world.net.push_back(flight);
            }
        }
        self.world.held_back = kept;
        self.world.hold_back.clear();
    }

    /// Brings at most `entries_per_step` entries a step in each replay from here on, as a
    /// replay over a driver brings its frames over time, where by default a replay
    /// brings all it can reach in one step. A test that stops a leader while a member
    /// it serves is part way through a span sets it low; 0 is taken as 1.
    pub fn set_replay_rate(&mut self, entries_per_step: usize) {
        self.world.replay_rate = entries_per_step.max(1);
    }

    /// How the consensus stream delivers from here on: one message in `loss_one_in` is
    /// lost, 0 for none, and each of the rest arrives up to `max_delay_ms` late, so
    /// that two may arrive out of the order they were sent in. A `max_delay_ms` above
    /// [`MAX_DELAY_MS`] is held to it, which this returns, the delay set. The faults
    /// are the same at every run.
    pub fn set_delivery(&mut self, loss_one_in: u64, max_delay_ms: u64) -> u64 {
        self.world.loss_one_in = loss_one_in;
        self.world.max_delay_ms = max_delay_ms.min(MAX_DELAY_MS);
        self.world.max_delay_ms
    }

    /// How many consensus messages are in flight now, at most [`IN_FLIGHT_CAPACITY`].
    pub fn in_flight(&self) -> usize {
        self.world.net.len()
    }

    /// How many consensus messages were lost because [`IN_FLIGHT_CAPACITY`] were in
    /// flight when they were sent. It only rises.
    pub fn full_queue_losses(&self) -> u64 {
        self.world.full_queue_losses
    }
}

impl core::fmt::Debug for Cluster {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Cluster")
            .field("now_ns", &self.world.clock.now_ns)
            .field("members", &self.cores.len())
            .field(
                "log_position",
                &self
                    .world
                    .publications
                    .last()
                    .map_or(0, |publication| publication.position),
            )
            .field("in_flight", &self.world.net.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// How many consensus messages are in flight to the member at `index`.
    fn in_flight_to(cluster: &Cluster, index: usize) -> usize {
        cluster
            .world
            .net
            .iter()
            .filter(|flight| flight.to == index)
            .count()
    }

    /// A cluster of three that has elected, whose consensus messages are then up to
    /// [`MAX_DELAY_MS`] late for one second, so that every member has messages in
    /// flight to it.
    fn with_messages_in_flight() -> Cluster {
        let mut cluster = Cluster::with_election(&[0, 1, 2], 1).expect("three members");
        cluster.run_ms(3_000);
        assert!(cluster.leader().is_some(), "a leader");
        cluster.set_delivery(0, MAX_DELAY_MS);
        cluster.run_ms(1_000);
        cluster.set_delivery(0, 0);
        for index in 0..3 {
            assert!(in_flight_to(&cluster, index) > 0, "member {index}");
        }
        cluster
    }

    /// A member started again on an empty log directory reads no message that was in
    /// flight to it, so none that a peer sent before it lost its log
    /// (`spec/cluster.md`, "Taking a leader"; ADR 0145). The messages to the others
    /// are kept, and the new core hears the others by what they send from then on.
    #[test]
    fn a_message_in_flight_to_a_member_when_it_is_wiped_never_reaches_the_new_core() {
        let mut cluster = with_messages_in_flight();
        let others = [in_flight_to(&cluster, 0), in_flight_to(&cluster, 2)];
        cluster.wipe(1);
        assert_eq!(in_flight_to(&cluster, 1), 0);
        assert_eq!(
            [in_flight_to(&cluster, 0), in_flight_to(&cluster, 2)],
            others
        );
        cluster.run_ms(MAX_DELAY_MS + 1);
        let member = cluster.member(1).expect("a member");
        assert_eq!(member.election_state(), ElectionState::Closed);
        assert_eq!(Some(member.leader_member_id()), cluster.leader());
    }

    /// The same at a start on the log directory the member kept, as after a power
    /// loss, when the host's driver started again with it.
    #[test]
    fn a_message_in_flight_to_a_member_when_it_is_restarted_never_reaches_the_new_core() {
        let mut cluster = with_messages_in_flight();
        let others = [in_flight_to(&cluster, 0), in_flight_to(&cluster, 2)];
        cluster.restart(1);
        assert_eq!(in_flight_to(&cluster, 1), 0);
        assert_eq!(
            [in_flight_to(&cluster, 0), in_flight_to(&cluster, 2)],
            others
        );
    }
}
