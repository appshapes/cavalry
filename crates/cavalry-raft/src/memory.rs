//! A cluster in memory: every member's [`Consensus`] with its I/O played by one
//! struct, on a clock the caller steps a millisecond at a time. No driver, no socket
//! and no file: the consensus stream is a queue, a leader's log publication is a list
//! of entries, a member's recording is the entries that have reached it, a log of its
//! own, and its sync is a flag. It is what the crate's example and its tests run, and
//! what a test of a service can run before it runs on three drivers.
//!
//! It is a test double, not a transport: it allocates, and nothing of it is on a
//! message's path in a running member. What it plays faithfully is what the core
//! depends on: a consensus message arrives whole, late or never; a follower's image of
//! the log joins at the position the leader's publication is at when the follower
//! subscribes; a member's append position is what its recording has synced; the
//! leader's own append position rises by its own sync, like any member's; and every
//! member holds the bytes that reached it and no others, so two members can hold
//! different bytes at one position, which a test of the cluster's safety must be able
//! to find.
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
//! - The logs and the records of terms have no bound: a log publication holds every
//!   entry appended to it for the cluster's life, each member's log holds every entry
//!   that reached it, and a member's record of terms holds every term it records. A
//!   test that offers a million entries to three members holds four million.

use std::collections::VecDeque;

use cavalry_proto::cluster::{LEADER_HEARTBEAT_TIMEOUT_NS, Message, Stream, entry_position};

use crate::consensus::{
    Clock, Consensus, Counts, ElectionState, Io, Log, MembersRefused, Refused, Role, terms_above,
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
    /// Gone with the member that added it.
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

/// A partition between two members, by their places among the members.
struct Cut {
    a: usize,
    b: usize,
    since_ns: u64,
}

/// What the world holds for one member: what its `terms` file and its recording would.
#[derive(Clone)]
struct Disk {
    id: i32,
    candidate_term: i64,
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
    /// When its cut began, for the image liveness timeout.
    cut_since_ns: Option<u64>,
    /// Its image of the log is gone, until it leaves the log and joins again.
    image_lost: bool,
    halted: bool,
}

impl Disk {
    /// A new log directory: no term, no vote, no recording.
    fn empty(id: i32) -> Self {
        Self {
            id,
            candidate_term: -1,
            terms: Vec::new(),
            joined: false,
            image: None,
            log: Vec::new(),
            received: 0,
            synced: 0,
            sync_paused: false,
            cut_since_ns: None,
            image_lost: false,
            halted: false,
        }
    }
}

/// Everything outside the cores.
struct World {
    clock: Clock,
    disks: Vec<Disk>,
    /// Every consensus message in flight, at most [`IN_FLIGHT_CAPACITY`], in the order
    /// sent.
    net: VecDeque<Flight>,
    /// The messages lost because `net` was full.
    full_queue_losses: u64,
    /// Every log publication a member added, for the cluster's life, in the order
    /// added.
    publications: Vec<Publication>,
    /// The partitions between two members that are set now.
    cuts: Vec<Cut>,
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

/// Since when nothing sent by one of the members at `a` and `b` reaches the other:
/// the earliest of the cuts set now that part them, a cut of either from everyone
/// or a partition between the two. `None` while they reach each other.
fn parted_since(disks: &[Disk], cuts: &[Cut], a: usize, b: usize) -> Option<u64> {
    let own = [a, b]
        .into_iter()
        .filter_map(|index| disks.get(index).and_then(|disk| disk.cut_since_ns));
    let between = cuts
        .iter()
        .filter(|cut| (cut.a == a && cut.b == b) || (cut.a == b && cut.b == a))
        .map(|cut| cut.since_ns);
    own.chain(between).min()
}

/// One member's [`Io`], for the length of one call into its core.
struct MemberIo<'a> {
    world: &'a mut World,
    own: usize,
}

impl Io for MemberIo<'_> {
    type Fault = core::convert::Infallible;

    fn send(&mut self, member: i32, message: &Message<'_>) -> bool {
        let Some(to) = self.world.disks.iter().position(|disk| disk.id == member) else {
            return false;
        };
        let mut buffer = [0u8; 256];
        let Ok(length) = message.write(&mut buffer) else {
            return false;
        };
        // A cut or halted member's messages leave it and arrive nowhere, as a
        // datagram's do, and so do the ones between two members a partition parts
        // and the ones the delivery setting loses.
        let halted = [self.own, to]
            .into_iter()
            .any(|index| self.world.disks.get(index).is_none_or(|disk| disk.halted));
        let cut =
            halted || parted_since(&self.world.disks, &self.world.cuts, self.own, to).is_some();
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

    fn add_log(&mut self, term: i64, base: i64) -> Result<Option<Log>, Self::Fault> {
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
        let parted = parted_since(&self.world.disks, &self.world.cuts, self.own, leader);
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

    fn leave_log(&mut self) -> Result<(), Self::Fault> {
        if let Some(disk) = self.world.disks.get_mut(self.own) {
            disk.joined = false;
            disk.image = None;
            disk.image_lost = false;
        }
        Ok(())
    }

    fn append_position(&mut self) -> i64 {
        self.world.disks.get(self.own).map_or(0, |disk| disk.synced)
    }

    fn terms_after(&mut self, term: i64) -> [Option<(i64, i64)>; 2] {
        let record = self.world.disks.get(self.own).map(|disk| &disk.terms);
        terms_above(record.into_iter().flatten().copied(), term)
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
    /// `Some` for a cluster whose members elect their leader: the seed each member's
    /// own is made from.
    election: Option<u64>,
}

impl Cluster {
    /// A cluster of the members with `member_ids`, every member started, at time 0.
    /// Its leader is appointed: the member with the lowest id (`spec/cluster.md`, "The
    /// appointed leader"), as in a cluster over drivers.
    pub fn new(member_ids: &[i32]) -> Result<Self, MembersRefused> {
        Self::start(member_ids, None)
    }

    /// A cluster of the members with `member_ids` that elect their leader
    /// ([`Consensus::with_election`]; ADR 0077), every member started, at time 0.
    /// Each member's nomination delays come from `seed` and its place among the
    /// members, so that no two members draw the same delays and a seed gives the same
    /// elections at every run. Which member leads depends on the seed:
    /// [`Cluster::leader`] says.
    pub fn with_election(member_ids: &[i32], seed: u64) -> Result<Self, MembersRefused> {
        Self::start(member_ids, Some(seed))
    }

    /// The core of the member at `index` among `member_ids`, appointed or electing.
    fn core(
        member_ids: &[i32],
        index: usize,
        election: Option<u64>,
    ) -> Result<Consensus, MembersRefused> {
        let id = member_ids.get(index).copied().unwrap_or(-1);
        match election {
            None => Consensus::new(member_ids, id),
            // One seed for each member: the cluster's, moved on by the member's place.
            Some(seed) => Consensus::with_election(
                member_ids,
                id,
                seed.wrapping_add(1)
                    .wrapping_mul(0x9E37_79B9_7F4A_7C15)
                    .wrapping_add((index as u64).wrapping_mul(0xD1B5_4A32_D192_ED03)),
            ),
        }
    }

    fn start(member_ids: &[i32], election: Option<u64>) -> Result<Self, MembersRefused> {
        let cores = (0..member_ids.len())
            .map(|index| Self::core(member_ids, index, election))
            .collect::<Result<Vec<_>, _>>()?;
        let disks = member_ids.iter().map(|&id| Disk::empty(id)).collect();
        Ok(Self {
            cores,
            election,
            world: World {
                clock: Clock {
                    now_ns: 0,
                    time_ms: 1_700_000_000_000,
                },
                disks,
                net: VecDeque::with_capacity(IN_FLIGHT_CAPACITY),
                full_queue_losses: 0,
                publications: Vec::new(),
                cuts: Vec::new(),
                back_pressured: false,
                loss_one_in: 0,
                max_delay_ms: 0,
                random: 0x9E37_79B9_7F4A_7C15,
            },
        })
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
                let parted = parted_since(
                    &self.world.disks,
                    &self.world.cuts,
                    index,
                    publication.leader,
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
                // the safety check of a test finds. The election's truncation, when a
                // member has it, cuts the member's log to where it joins.
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
        let mut index = 0;
        while index < self.world.net.len() {
            let due = self
                .world
                .net
                .get(index)
                .is_some_and(|flight| flight.deliver_at_ns <= now_ns);
            if !due {
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
                let Ok(()) = core.on_message(clock, &message, &mut io);
            }
        }
        for (own, core) in self.cores.iter_mut().enumerate() {
            if self.world.disks.get(own).is_none_or(|disk| disk.halted) {
                continue;
            }
            let clock = self.world.clock;
            let mut io = MemberIo {
                world: &mut self.world,
                own,
            };
            let Ok(_) = core.do_work(clock, &mut io);
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

    /// The member that leads: the one whose role is leader, the one of the highest
    /// term when a member that stopped still holds an older one, and never a halted
    /// member. `None` while no running member leads, during an election.
    pub fn leader(&self) -> Option<i32> {
        self.cores
            .iter()
            .zip(&self.world.disks)
            .filter(|(core, disk)| core.role() == Role::Leader && !disk.halted)
            .max_by_key(|(core, _)| core.leadership_term_id())
            .map(|(core, _)| core.member_id())
    }

    /// Offers `payload` to the leader as its own process does, and returns the
    /// position after the entry ([`Consensus::offer`]). The leader is the appointed
    /// one, the member with the lowest id, in a cluster made with [`Cluster::new`],
    /// and [`Cluster::leader`] in one that elects, where an offer made while no member
    /// leads is refused as not the leader's.
    pub fn offer(&mut self, payload: &[u8]) -> Result<i64, Refused> {
        let leader = match self.election {
            None => self.cores.first().map_or(-1, Consensus::leader_member_id),
            Some(_) => self.leader().unwrap_or(-1),
        };
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
        let Ok(outcome) = core.offer(clock, payload, &mut io);
        outcome.map(|()| {
            self.world
                .publication_of(own)
                .and_then(|index| self.world.publications.get(index))
                .map_or(0, |publication| publication.position)
        })
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
        let parted = self
            .world
            .cuts
            .iter()
            .any(|cut| (cut.a == a && cut.b == b) || (cut.a == b && cut.b == a));
        if a != b && !parted {
            let since_ns = self.world.clock.now_ns;
            self.world.cuts.push(Cut { a, b, since_ns });
        }
    }

    /// Ends the partition between two members.
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
    /// follower's image of it. A member of this version does not start again on the
    /// log it had (`spec/cluster.md`, "The appointed leader"); this is the start it
    /// can make, and what the cluster does about it is the test.
    pub fn wipe(&mut self, member: i32) {
        let Some(index) = self.index(member) else {
            return;
        };
        let ids: Vec<i32> = self.world.disks.iter().map(|disk| disk.id).collect();
        let led = self
            .cores
            .get(index)
            .is_some_and(|core| core.role() == Role::Leader);
        if let (Some(core), Ok(new)) = (
            self.cores.get_mut(index),
            Self::core(&ids, index, self.election),
        ) {
            *core = new;
        }
        if let Some(disk) = self.world.disks.get_mut(index) {
            *disk = Disk::empty(member);
        }
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
