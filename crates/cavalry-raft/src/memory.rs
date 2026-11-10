//! A cluster in memory: every member's [`Consensus`] with its I/O played by one
//! struct, on a clock the caller steps a millisecond at a time. No driver, no socket
//! and no file: the consensus stream is a queue, the log is a list of entries, a
//! member's recording is how far the log has reached it and its sync is a flag. It is
//! what the crate's example and its tests run, and what a test of a service can run
//! before it runs on three drivers.
//!
//! It is a test double, not a transport: it allocates, and nothing of it is on a
//! message's path in a running member. What it plays faithfully is what the core
//! depends on: a consensus message arrives whole, late or never; a follower's image of
//! the log joins at the position the leader's publication is at when the follower
//! subscribes; a member's append position is what its recording has synced; and the
//! leader's own append position rises by its own sync, like any member's.

use std::collections::VecDeque;

use cavalry_proto::cluster::{Message, Stream, entry_position};

use crate::consensus::{
    Clock, Consensus, Counts, ElectionState, Io, Log, MembersRefused, Refused, Role,
};

/// The MTU length the cluster's log positions are computed with: the UDP transport's
/// (`spec/udp.md`).
const MTU: usize = 1408;

/// How long a consensus message takes with no delay asked: one step.
const STEP_NS: u64 = 1_000_000;

/// How long an image outlives the last datagram of its stream: the transport's image
/// liveness timeout (`spec/udp.md`).
const IMAGE_LIVENESS_TIMEOUT_NS: u64 = 10_000_000_000;

/// One consensus message between two members.
struct Flight {
    deliver_at_ns: u64,
    to: usize,
    bytes: Vec<u8>,
}

/// One entry of the leader's log: its bytes and the position after it.
struct Entry {
    position_after: i64,
    bytes: Vec<u8>,
}

/// What the world holds for one member: what its `terms` file and its recording would.
#[derive(Clone)]
struct Disk {
    id: i32,
    candidate_term: i64,
    terms: Vec<(i64, i64)>,
    /// Subscribed to the log, with a recording armed.
    joined: bool,
    /// Where its image of the log joined, once it has one.
    join_position: Option<i64>,
    /// The position up to which the log has reached its recording.
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
            join_position: None,
            received: 0,
            synced: 0,
            sync_paused: false,
            cut_since_ns: None,
            image_lost: false,
            halted: false,
        }
    }

    fn cut(&self) -> bool {
        self.cut_since_ns.is_some()
    }
}

/// Everything outside the cores.
struct World {
    clock: Clock,
    disks: Vec<Disk>,
    net: VecDeque<Flight>,
    log: Vec<Entry>,
    log_session_id: Option<i32>,
    log_position: i64,
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
        // datagram's do; so do the ones the delivery setting loses.
        let cut =
            self.world.disks.iter().enumerate().any(|(index, disk)| {
                (index == self.own || index == to) && (disk.cut() || disk.halted)
            });
        let lost = self.world.loss_one_in > 0
            && self
                .world
                .next_random()
                .is_multiple_of(self.world.loss_one_in);
        if cut || lost {
            return true;
        }
        let delay_ms = if self.world.max_delay_ms == 0 {
            0
        } else {
            self.world.next_random() % (self.world.max_delay_ms + 1)
        };
        self.world.net.push_back(Flight {
            deliver_at_ns: self.world.clock.now_ns + STEP_NS + delay_ms * STEP_NS,
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
        self.world.log_session_id = Some(session_id);
        self.world.log_position = base;
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
        let position = self.world.log_position;
        let now_ns = self.world.clock.now_ns;
        let exists = self.world.log_session_id == Some(session_id);
        let Some(disk) = self.world.disks.get_mut(self.own) else {
            return Ok(None);
        };
        if !exists || !disk.joined || disk.image_lost {
            return Ok(None);
        }
        if let Some(since_ns) = disk.cut_since_ns {
            // Nothing of the log arrives: no image comes, and one already there is
            // gone when the image liveness timeout has passed.
            if now_ns.saturating_sub(since_ns) >= IMAGE_LIVENESS_TIMEOUT_NS {
                disk.image_lost = disk.join_position.is_some();
                return Ok(None);
            }
            return Ok(disk.join_position);
        }
        // The image joins where the publication is when the subscriber first sees it.
        Ok(Some(*disk.join_position.get_or_insert(position)))
    }

    fn leave_log(&mut self) -> Result<(), Self::Fault> {
        if let Some(disk) = self.world.disks.get_mut(self.own) {
            disk.joined = false;
            disk.join_position = None;
            disk.image_lost = false;
        }
        Ok(())
    }

    fn append_position(&mut self) -> i64 {
        self.world.disks.get(self.own).map_or(0, |disk| disk.synced)
    }

    fn append(&mut self, entry: &Message<'_>) -> Result<bool, Self::Fault> {
        if self.world.back_pressured {
            return Ok(false);
        }
        let mut bytes = vec![0u8; entry.encoded_length()];
        let Ok(length) = entry.write(&mut bytes) else {
            return Ok(false);
        };
        let Some(after) = entry_position(self.world.log_position, length, MTU) else {
            return Ok(false);
        };
        self.world.log_position = after.position_after;
        self.world.log.push(Entry {
            position_after: after.position_after,
            bytes,
        });
        if let Some(disk) = self.world.disks.get_mut(self.own) {
            disk.received = after.position_after;
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
}

impl Cluster {
    /// A cluster of the members with `member_ids`, every member started, at time 0.
    pub fn new(member_ids: &[i32]) -> Result<Self, MembersRefused> {
        let cores = member_ids
            .iter()
            .map(|&id| Consensus::new(member_ids, id))
            .collect::<Result<Vec<_>, _>>()?;
        let disks = member_ids.iter().map(|&id| Disk::empty(id)).collect();
        Ok(Self {
            cores,
            world: World {
                clock: Clock {
                    now_ns: 0,
                    time_ms: 1_700_000_000_000,
                },
                disks,
                net: VecDeque::new(),
                log: Vec::new(),
                log_session_id: None,
                log_position: 0,
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
        self.world.clock.now_ns += STEP_NS;
        self.world.clock.time_ms += 1;
        let log_position = self.world.log_position;
        for disk in &mut self.world.disks {
            // A recording holds the image from its join position: only one that joined
            // at the end of what the member held extends its contiguous log.
            if disk.joined
                && !disk.cut()
                && !disk.halted
                && !disk.image_lost
                && disk.join_position.is_some_and(|join| join <= disk.received)
            {
                disk.received = disk.received.max(log_position);
            }
            if !disk.sync_paused && !disk.halted {
                disk.synced = disk.received;
            }
        }
        let now_ns = self.world.clock.now_ns;
        let mut waiting = VecDeque::with_capacity(self.world.net.len());
        while let Some(flight) = self.world.net.pop_front() {
            if flight.deliver_at_ns > now_ns {
                waiting.push_back(flight);
                continue;
            }
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
        // Messages sent while these arrived are behind the ones still waiting.
        waiting.append(&mut self.world.net);
        self.world.net = waiting;
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

    /// The bytes of the log the member's recording holds: every entry up to the
    /// position the log has reached it at, encoded as `spec/cluster.md` gives it, one
    /// after another. Empty for a member that holds none.
    pub fn log(&self, member: i32) -> Vec<u8> {
        let received = self
            .index(member)
            .and_then(|index| self.world.disks.get(index))
            .map_or(0, |disk| disk.received);
        self.world
            .log
            .iter()
            .take_while(|entry| entry.position_after <= received)
            .flat_map(|entry| entry.bytes.iter().copied())
            .collect()
    }

    /// Offers `payload` to the leader, the member with the lowest id, as its own
    /// process does, and returns the position after the entry
    /// ([`Consensus::offer`]).
    pub fn offer(&mut self, payload: &[u8]) -> Result<i64, Refused> {
        let leader = self.cores.first().map_or(-1, Consensus::leader_member_id);
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
        outcome.map(|()| self.world.log_position)
    }

    /// Cuts the member off: its consensus messages, sent and to come, arrive nowhere,
    /// and the log stops reaching it. Its duty cycle still runs. An image of the log it
    /// holds stays for the image liveness timeout, 10 s, and what the cut kept from it
    /// arrives when the cut ends sooner, as a retransmission would bring it.
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
        if let (Some(core), Ok(new)) = (self.cores.get_mut(index), Consensus::new(&ids, member)) {
            *core = new;
        }
        if let Some(disk) = self.world.disks.get_mut(index) {
            *disk = Disk::empty(member);
        }
        if led {
            self.world.log_session_id = None;
            for disk in &mut self.world.disks {
                disk.image_lost = disk.join_position.is_some();
            }
        }
    }

    /// Takes the member's image of the log away, as the transport does when the
    /// image's stream goes silent: nothing more of the log reaches the member until it
    /// has left the log and joined it again.
    pub fn lose_image(&mut self, member: i32) {
        if let Some(disk) = self.disk_mut(member) {
            disk.image_lost = disk.join_position.is_some();
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
    /// that two may arrive out of the order they were sent in. The faults are the same
    /// at every run.
    pub fn set_delivery(&mut self, loss_one_in: u64, max_delay_ms: u64) {
        self.world.loss_one_in = loss_one_in;
        self.world.max_delay_ms = max_delay_ms;
    }
}

impl core::fmt::Debug for Cluster {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Cluster")
            .field("now_ns", &self.world.clock.now_ns)
            .field("members", &self.cores.len())
            .field("log_position", &self.world.log_position)
            .finish()
    }
}
