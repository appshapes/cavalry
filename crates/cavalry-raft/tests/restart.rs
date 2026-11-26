//! Three members over three drivers on the loopback, stopped and started again on the
//! log directories they kept (`spec/cluster.md`, "A member's start"; ADR 0089, ADR
//! 0108): a follower, the leader, all three at once, and all three after their hosts
//! lost power together, each log holding a frame past its sync whose body is gone
//! (ADR 0144), once in the segment file that holds the synced position and once with
//! the tail written on into the next segment file (work item 471). A member that
//! starts again
//! reads its log back from the recordings its record of them names, joins the
//! cluster with it, and replicates only what it lacks. Each test holds that nothing
//! committed is lost: a service beside the member that started again reads every
//! committed entry from position 0, once and in order, the same bytes a service
//! beside another member reads.
//!
//! A member's driver is closed and started again with it, as `cavalry run` does: the
//! driver directory is made anew, and the log directory is the one the member had.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use cavalry_client::{Cavalry, CommittedLog};
use cavalry_driver::Driver;
use cavalry_log::catalog::CatalogReader;
use cavalry_log::layout::{CatalogHeader, Descriptor, crc32, locate, segment_file_name};
use cavalry_log::recordings::RecordingsFile;
use cavalry_proto::frame::{DataFrameHeader, Flags, FrameType};
use cavalry_raft::{ConsensusModule, ElectionState, RaftError, Refused, Role};
use cavalry_shm::counters::CountersReader;
use cavalry_shm::counters::generated::{
    CLUSTER_LEADER_MEMBER_ID, CLUSTER_LOG_RECORDINGS_FORGOTTEN, CLUSTER_LOG_RECORDINGS_LISTED,
    CLUSTER_LOG_RECORDINGS_UNSYNCED, CLUSTER_TRUNCATIONS_FINISHED, RECORDING_CUT_TO_SYNCED,
    RECORDING_FILES_REMOVED, RECORDING_RECOVERED,
};

mod ports;
use ports::free_ports;

/// The length in the log of one entry these tests offer, and of a new leadership term
/// event: a 32-byte frame header, a 32-byte session header or the event's 64 bytes,
/// and the 8-byte payload, aligned to 32.
const FRAME: i64 = 96;

struct TestDir(PathBuf);

impl TestDir {
    fn new(test: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "cavalry-raft-restart-{}-{test}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        TestDir(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The members string of three members on the loopback.
fn members_string() -> String {
    let ports = free_ports(15);
    (0..3)
        .map(|member| {
            let endpoints: Vec<String> = ports[member * 5..member * 5 + 5]
                .iter()
                .map(|port| format!("127.0.0.1:{port}"))
                .collect();
            format!("{member},{}", endpoints.join(","))
        })
        .collect::<Vec<_>>()
        .join("|")
}

struct Member {
    driver: Driver,
    module: ConsensusModule,
    dir: PathBuf,
    log_dir: PathBuf,
}

/// Starts member `id` on its directories under `dir`: a driver, and the member over
/// it. A log directory that is there is the one the member kept.
fn start_member(dir: &Path, members: &str, id: i32) -> Member {
    let driver_dir = dir.join(format!("member-{id}"));
    let log_dir = dir.join(format!("member-{id}-log"));
    let driver = Driver::start(&driver_dir, &log_dir).expect("the driver starts");
    let module = ConsensusModule::start(&driver_dir, &log_dir, members, id)
        .expect("the member starts, on a new log directory or on the one it kept");
    Member {
        driver,
        module,
        dir: driver_dir,
        log_dir,
    }
}

/// Starts the three members, in an order that does not put the member with the
/// lowest id first.
fn start(dir: &Path, members: &str) -> Vec<Member> {
    [1, 0, 2]
        .into_iter()
        .map(|id| start_member(dir, members, id))
        .collect()
}

/// Drives every member's duty cycle until `done`, or fails with `what`.
fn run_until(members: &mut [Member], what: &str, mut done: impl FnMut(&[Member]) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !done(members) {
        assert!(Instant::now() < deadline, "{what}: {:?}", states(members));
        let mut work = 0;
        for member in members.iter_mut() {
            work += member.module.do_work().expect("the member's duty cycle");
        }
        if work == 0 {
            std::thread::sleep(Duration::from_micros(200));
        }
    }
}

fn states(members: &[Member]) -> Vec<String> {
    members.iter().map(|m| format!("{:?}", m.module)).collect()
}

/// True when every member of `members` has closed its election in one term above
/// `above` and exactly one of them leads.
fn led(members: &[Member], above: i64) -> bool {
    let term = members[0].module.leadership_term_id();
    term > above
        && members.iter().all(|m| {
            m.module.election_state() == ElectionState::Closed
                && m.module.leadership_term_id() == term
        })
        && members
            .iter()
            .filter(|m| m.module.role() == Role::Leader)
            .count()
            == 1
}

/// Runs the members until they are led in a term above `above`, then puts the member
/// that leads first.
fn run_until_led(members: &mut [Member], above: i64) {
    run_until(members, "the members have a leader", |members| {
        led(members, above)
    });
    let leader = members
        .iter()
        .position(|m| m.module.role() == Role::Leader)
        .expect("one member leads");
    members.swap(0, leader);
}

/// The leader, `members[0]`, appends `count` entries numbered from `from`, a
/// back-pressured offer made again once every member has run.
fn offer_entries(members: &mut [Member], from: i64, count: i64) {
    for i in from..from + count {
        loop {
            match members[0].module.offer(&i.to_le_bytes()) {
                Ok(()) => break,
                Err(RaftError::Offer(Refused::BackPressured)) => {
                    for member in members.iter_mut() {
                        member.module.do_work().expect("the member's duty cycle");
                    }
                }
                Err(other) => panic!("{other}"),
            }
        }
    }
}

/// Runs the members until each has appended and committed to `end`.
fn run_until_committed(members: &mut [Member], what: &str, end: i64) {
    run_until(members, what, |members| {
        members
            .iter()
            .all(|m| m.module.commit_position() == end && m.module.append_position() == end)
    });
}

/// The value of the member's counter of `type_id`, found in its driver's counters file
/// by the type and the member id in the key.
fn counter(member: &Member, type_id: i32) -> i64 {
    let reader = CountersReader::open(&member.dir).expect("the counters file opens");
    let key = member.module.member_id().to_le_bytes();
    for id in 0..reader.capacity() {
        if let Some(record) = reader.record(id).expect("a record")
            && record.type_id() == type_id
            && record.key().starts_with(&key)
        {
            return reader.read_acquire(id).expect("the counter is read");
        }
    }
    panic!(
        "member {} has no counter of type {type_id}",
        member.module.member_id()
    );
}

/// The member's record of its log's recordings as its log directory holds it
/// (`spec/durable-log.md`, "Log recordings"): the floor position, and each recording's
/// id and start position.
fn record_of(log_dir: &Path, member_id: i32) -> (i64, Vec<(i64, i64)>) {
    let file = RecordingsFile::open_kept(log_dir, member_id).expect("the record opens");
    (
        file.floor_position(),
        file.entries()
            .iter()
            .map(|entry| (entry.recording_id, entry.start_position))
            .collect(),
    )
}

/// Holds what a member's record names after it started again, `after`, against what
/// it named before it stopped, `named`, and returns the recordings it has made
/// since. The record begins with the ones it named before, in their order, without
/// those that held nothing: a recording a member was making when it left a log
/// before a byte arrived is one of them, and a start leaves such a recording out
/// (`spec/cluster.md`, "A member's start"). Its first recording begins at 0.
fn made_since<'a>(named: &[(i64, i64)], after: &'a [(i64, i64)]) -> &'a [(i64, i64)] {
    let kept: Vec<(i64, i64)> = named
        .iter()
        .filter(|entry| after.contains(entry))
        .copied()
        .collect();
    assert_eq!(
        after.first().map(|(_, start)| *start),
        Some(0),
        "{after:?} after {named:?}"
    );
    assert_eq!(after[..kept.len()], kept[..], "{after:?} after {named:?}");
    &after[kept.len()..]
}

/// What a service read: each entry's position and its bytes.
type Read = Vec<(i64, Vec<u8>)>;

/// Polls `log` once into `read`, and holds the poll to the service's rule: every entry
/// it was given begins below the commit position the poll took as its bound.
fn poll_log(log: &mut CommittedLog, read: &mut Read) {
    let before = read.len();
    let delivered = log
        .poll(
            |entry, _, position| read.push((position, entry.to_vec())),
            64,
        )
        .expect("the committed log polls");
    assert_eq!(delivered, read.len() - before);
    for (position, _) in &read[before..] {
        assert!(
            *position < log.commit_position(),
            "an entry at {position} read with a bound of {}",
            log.commit_position()
        );
    }
}

/// A service beside `members[index]` reads the committed log from position 0 to
/// `to`, as a service does across the recordings of its member's log
/// (`spec/cluster.md`, "The service"; ADR 0103): it opens the log where it has read
/// to, reads to the end of that recording, and opens it again. Every handle ends
/// whole, with nothing reported.
fn read_committed(members: &mut [Member], index: usize, to: i64) -> Read {
    let mut client =
        Cavalry::connect_to(&members[index].dir).expect("the service's client connects");
    let mut read = Read::new();
    let mut position = 0;
    while position < to {
        let mut log = client
            .committed_log(position)
            .expect("the driver answers")
            .expect("the member names a recording of its log");
        run_until(members, "the service read a recording of the log", |_| {
            poll_log(&mut log, &mut read);
            log.is_ended() || log.position() == to
        });
        assert!(
            log.position() > position,
            "a handle opened at {position} ended there"
        );
        assert_eq!(
            (log.failed_at(), log.withdrawn_at(), log.faulted()),
            (None, None, None),
            "the handle opened at {position}"
        );
        position = log.position();
        log.close(&mut client).expect("the handle closes");
    }
    client.close().expect("the service's client closes");
    read
}

/// The positions of `count` entries of [`FRAME`] bytes from `from`.
fn positions(from: i64, count: i64) -> Vec<i64> {
    (0..count).map(|i| from + i * FRAME).collect()
}

/// A follower's process is lost and started again. While it is away the two that
/// remain commit ten entries more. Started on the log directory it kept, the member
/// reads its log back: before it has heard another member its append position is
/// where its log ended when it stopped, not 0. It joins its leader in the leader's
/// term, with no election among the others, replicates the ten entries it lacks and
/// nothing below them, and its record of its log's recordings goes on from the
/// recordings it named before. A service beside it reads all twenty-one entries
/// from 0, the bytes a service beside the leader reads.
#[test]
fn a_follower_that_stopped_starts_again_on_its_log_and_replicates_what_it_lacks() {
    let dir = TestDir::new("follower");
    let members_string = members_string();
    let mut members = start(dir.path(), &members_string);
    run_until_led(&mut members, -1);
    offer_entries(&mut members, 0, 10);
    let first = FRAME + 10 * FRAME;
    run_until_committed(&mut members, "the first entries committed", first);
    let term = members[0].module.leadership_term_id();
    let leader = members[0].module.member_id();

    // The follower's module goes without a close, as a process that was killed does,
    // and its driver with it.
    let stopped = members.remove(2);
    let id = stopped.module.member_id();
    drop(stopped.module);
    stopped.driver.close().expect("the driver closes");
    let (floor, named) = record_of(&stopped.log_dir, id);
    assert_eq!(floor, 0);
    assert_eq!(named.first().map(|(_, start)| *start), Some(0), "{named:?}");

    offer_entries(&mut members, 10, 10);
    let second = first + 10 * FRAME;
    run_until_committed(
        &mut members,
        "the two that remain committed ten more",
        second,
    );

    let started = Instant::now();
    members.push(start_member(dir.path(), &members_string, id));
    // Alone, before any other member has run: the log it kept, to where it ended.
    run_until(
        &mut members[2..],
        "the member read its log back",
        |members| members[0].module.append_position() == first,
    );
    assert_eq!(members[2].module.commit_position(), 0, "it knows none yet");

    run_until(
        &mut members,
        "the member that started again holds what was committed without it",
        |members| {
            members.iter().all(|m| {
                m.module.election_state() == ElectionState::Closed
                    && m.module.commit_position() == second
                    && m.module.append_position() == second
            })
        },
    );
    println!(
        "restart: follower {id} started again on its log and held the ten entries it \
         lacked {:?} after its start",
        started.elapsed()
    );
    // The leader led on: the same term, and no election but the member's own.
    assert_eq!(members[0].module.member_id(), leader);
    assert_eq!(members[0].module.role(), Role::Leader);
    for member in &members {
        assert_eq!(member.module.leadership_term_id(), term);
        assert_eq!(
            counter(member, CLUSTER_LEADER_MEMBER_ID.0),
            i64::from(leader)
        );
        // An election of the member's own that failed and began again, on a
        // runner that starved it, is no fault: what is held is that nothing was
        // refused and that the two others stayed in their term, above.
        let counts = member.module.counts();
        assert_eq!(counts.truncations_refused, 0, "{counts:?}");
    }
    assert_eq!(members[2].module.role(), Role::Follower);
    assert!(members[2].module.counts().elections >= 1);
    // The counters of each member's record of its log's recordings (ADR 0108). The
    // two that started on new directories counted the list their creation wrote
    // and the one that names their recording; the member that started again
    // created none, and counted each list that took a recording into its log. No
    // write failed, no recording was forgotten, and no start finished a truncation.
    for (index, member) in members.iter().enumerate() {
        let listed = counter(member, CLUSTER_LOG_RECORDINGS_LISTED.0);
        assert!(
            listed >= if index == 2 { 1 } else { 2 },
            "member {index} counted {listed} lists"
        );
        for id in [
            CLUSTER_LOG_RECORDINGS_UNSYNCED,
            CLUSTER_LOG_RECORDINGS_FORGOTTEN,
            CLUSTER_TRUNCATIONS_FINISHED,
        ] {
            assert_eq!(counter(member, id.0), 0, "member {index}, counter {}", id.0);
        }
    }

    // Its record goes on from the recordings it named before the stop, and each one
    // after them begins at or above where its log ended then: nothing below was
    // replicated a second time.
    let (floor, after) = record_of(&members[2].log_dir, id);
    assert_eq!(floor, 0);
    let since = made_since(&named, &after);
    assert!(!since.is_empty(), "{after:?} after {named:?}");
    assert!(since.iter().all(|(_, start)| *start >= first), "{after:?}");

    let beside_leader = read_committed(&mut members, 0, second);
    let beside_member = read_committed(&mut members, 2, second);
    assert_eq!(
        beside_member
            .iter()
            .map(|(position, _)| *position)
            .collect::<Vec<_>>(),
        positions(0, 21)
    );
    assert_eq!(beside_member, beside_leader);

    for member in members {
        member.module.close().expect("the member closes");
        member.driver.close().expect("the driver closes");
    }
}

/// The leader's process is lost. The two that remain elect another and commit five
/// entries in its term. The member that led starts again on the log directory it
/// kept: it reads back the log of the term it led, canvasses with it, and joins the
/// new leader as a follower in the new term, replicating the new term's entries and
/// truncating nothing. A service beside it reads the entries it committed as a
/// leader and the ones committed without it, the bytes a service beside the new
/// leader reads.
#[test]
fn a_leader_that_stopped_starts_again_on_its_log_as_a_follower_of_the_next_term() {
    let dir = TestDir::new("leader");
    let members_string = members_string();
    let mut members = start(dir.path(), &members_string);
    run_until_led(&mut members, -1);
    offer_entries(&mut members, 0, 10);
    let first = FRAME + 10 * FRAME;
    run_until_committed(&mut members, "the first entries committed", first);
    let first_term = members[0].module.leadership_term_id();
    let before = read_committed(&mut members, 0, first);
    assert_eq!(before.len(), 11);

    let stopped = members.remove(0);
    let id = stopped.module.member_id();
    drop(stopped.module);
    stopped.driver.close().expect("the driver closes");
    let (_, named) = record_of(&stopped.log_dir, id);
    assert_eq!(
        named.first().map(|(_, start)| *start),
        Some(0),
        "its log from position 0: {named:?}"
    );

    run_until_led(&mut members, first_term);
    let term = members[0].module.leadership_term_id();
    let leader = members[0].module.member_id();
    assert_ne!(leader, id);
    // The new term's event, where the log ended, and five entries after it.
    offer_entries(&mut members, 10, 5);
    let second = first + FRAME + 5 * FRAME;
    run_until_committed(
        &mut members,
        "the two that remain committed in the new term",
        second,
    );

    let started = Instant::now();
    members.push(start_member(dir.path(), &members_string, id));
    run_until(
        &mut members[2..],
        "the member read its log back",
        |members| members[0].module.append_position() == first,
    );
    run_until(
        &mut members,
        "the member that led follows the new leader and holds its log",
        |members| {
            members.iter().all(|m| {
                m.module.election_state() == ElectionState::Closed
                    && m.module.leadership_term_id() == term
                    && m.module.commit_position() == second
                    && m.module.append_position() == second
            })
        },
    );
    println!(
        "restart: member {id}, which led term {first_term}, started again on its log and \
         followed member {leader} in term {term} {:?} after its start",
        started.elapsed()
    );
    assert_eq!(members[0].module.member_id(), leader);
    assert_eq!(members[0].module.role(), Role::Leader);
    assert_eq!(members[2].module.role(), Role::Follower);
    for member in &members {
        assert_eq!(
            counter(member, CLUSTER_LEADER_MEMBER_ID.0),
            i64::from(leader)
        );
        let counts = member.module.counts();
        assert_eq!(counts.truncations_refused, 0, "{counts:?}");
    }
    let (floor, after) = record_of(&members[2].log_dir, id);
    assert_eq!(floor, 0);
    let since = made_since(&named, &after);
    assert!(since.iter().all(|(_, start)| *start >= first), "{after:?}");

    let beside_leader = read_committed(&mut members, 0, second);
    let beside_member = read_committed(&mut members, 2, second);
    assert_eq!(beside_member.len(), 17);
    assert_eq!(beside_member, beside_leader);
    assert_eq!(
        beside_member[..11],
        before[..],
        "what the member committed as a leader"
    );

    for member in members {
        member.module.close().expect("the member closes");
        member.driver.close().expect("the driver closes");
    }
}

/// All three members close and start again, each on the log directory it kept. They
/// elect a leader in a term above the one they held, since each kept its record of
/// terms and its vote, and the log goes on where it ended: the new term's event and
/// five entries are committed after the eleven the cluster committed before. A
/// service beside each member reads all seventeen from 0, the same bytes on each,
/// and the first eleven are the bytes a service read before the members stopped.
#[test]
fn all_three_members_start_again_on_their_logs_and_the_log_goes_on() {
    let dir = TestDir::new("all");
    let members_string = members_string();
    let mut members = start(dir.path(), &members_string);
    run_until_led(&mut members, -1);
    offer_entries(&mut members, 0, 10);
    let first = FRAME + 10 * FRAME;
    run_until_committed(&mut members, "the first entries committed", first);
    let first_term = members[0].module.leadership_term_id();
    let before = read_committed(&mut members, 0, first);
    assert_eq!(before.len(), 11);

    for member in members {
        member.module.close().expect("the member closes");
        member.driver.close().expect("the driver closes");
    }

    let started = Instant::now();
    let mut members = start(dir.path(), &members_string);
    for member in &mut members {
        // Each alone, before it has heard another: the log it kept.
        run_until(
            std::slice::from_mut(member),
            "the member read its log back",
            |members| members[0].module.append_position() == first,
        );
    }
    run_until_led(&mut members, first_term);
    let term = members[0].module.leadership_term_id();
    println!(
        "restart: three members started again on their logs and member {} led term {term}, \
         after term {first_term}, {:?} after their start",
        members[0].module.member_id(),
        started.elapsed()
    );
    offer_entries(&mut members, 10, 5);
    let second = first + FRAME + 5 * FRAME;
    run_until_committed(&mut members, "the log went on", second);
    for member in &members {
        let counts = member.module.counts();
        assert_eq!(counts.truncations_refused, 0, "{counts:?}");
        let (floor, named) = record_of(&member.log_dir, member.module.member_id());
        assert_eq!(floor, 0);
        assert_eq!(named.first().map(|(_, start)| *start), Some(0), "{named:?}");
    }

    let read: Vec<Read> = (0..3)
        .map(|index| read_committed(&mut members, index, second))
        .collect();
    assert_eq!(read[0].len(), 17);
    assert_eq!(read[0][..11], before[..]);
    assert_eq!(read[1], read[0]);
    assert_eq!(read[2], read[0]);

    for member in members {
        member.module.close().expect("the member closes");
        member.driver.close().expect("the driver closes");
    }
}

/// Leaves `member`'s log directory as a power loss on its host would, once the member
/// has closed: the recording that ends its log reads live in the catalog again, and
/// past the position it had synced lies a frame whose header survived and whose body
/// did not, a frame of an entry's length with every field the `frame` check reads
/// and a body of zeros (`spec/durable-log.md`, "The driver"; ADR 0144). Returns the
/// position the log had synced to, where the frame begins.
fn lose_power(log_dir: &Path, member_id: i32) -> i64 {
    let descriptor = last_recording(log_dir, member_id);
    let recording_id = descriptor.recording_id();
    let synced = descriptor.stop_position();
    assert!(synced > 0, "the recording holds the log: {descriptor:?}");
    // The stop the close wrote, undone: the stop fields at −1 and −1 under their
    // checksum, as a recording reads when the power was lost while it ran.
    let mut stop = [0u8; 20];
    stop[4..12].copy_from_slice(&(-1i64).to_le_bytes());
    stop[12..20].copy_from_slice(&(-1i64).to_le_bytes());
    let checksum = crc32(&stop[4..20]);
    stop[..4].copy_from_slice(&checksum.to_le_bytes());
    let slot = CatalogHeader::slot_offset(u64::try_from(recording_id).expect("an id"));
    write_at(&log_dir.join("catalog"), &stop, slot + 4);
    // The frame past the sync: its header whole, its body lost.
    let located = locate(&descriptor, synced).expect("a position of the recording");
    let header = DataFrameHeader::new(FrameType::Data, Flags::UNFRAGMENTED, FRAME as usize)
        .expect("a header")
        .with_term_offset(located.term_offset)
        .with_term_id(located.term_id)
        .with_session_id(descriptor.session_id())
        .with_stream_id(descriptor.stream_id())
        .to_bytes();
    let mut frame = [0u8; FRAME as usize];
    frame[..header.len()].copy_from_slice(&header);
    write_at(
        &log_dir.join(&located.segment_file),
        &frame,
        located.offset_in_segment as u64,
    );
    synced
}

/// The catalog's descriptor of the recording that ends the member's log.
fn last_recording(log_dir: &Path, member_id: i32) -> Descriptor {
    let (_, named) = record_of(log_dir, member_id);
    let &(recording_id, _) = named.last().expect("the log is in a recording");
    CatalogReader::open(log_dir)
        .expect("the catalog opens")
        .get(recording_id)
        .expect("the slot reads")
        .expect("the catalog holds the recording")
}

/// Leaves `member`'s log directory as [`lose_power`] does, with the recorder written on
/// past its sync to the end of the segment file that holds the synced position and
/// rolled into the next, as a power loss can leave a recording that grew fast (work
/// item 471; `crates/cavalry-log/tests/power_loss.rs` holds the recovery at the log,
/// work item 405): the frame past the sync with its body lost, a padding frame at the
/// start of the segment's last term, as the recorder pads a term it has filled, and
/// the next segment file at the segment file length, as the recorder makes it, with a
/// frame at its base whose body is lost. The synced position stays where it was, as a
/// roll moves no synced position. Returns the synced position, the padding frame's
/// file and offset in it, and the next segment file.
fn lose_power_across_a_roll(log_dir: &Path, member_id: i32) -> (i64, (PathBuf, u64), PathBuf) {
    let descriptor = last_recording(log_dir, member_id);
    let synced = lose_power(log_dir, member_id);
    let segment = i64::from(descriptor.segment_file_length());
    let term = i64::from(descriptor.term_length());
    let header = |kind: FrameType, position: i64, length: usize| {
        let located = locate(&descriptor, position).expect("a position of the recording");
        let bytes = DataFrameHeader::new(kind, Flags::UNFRAGMENTED, length)
            .expect("a header")
            .with_term_offset(located.term_offset)
            .with_term_id(located.term_id)
            .with_session_id(descriptor.session_id())
            .with_stream_id(descriptor.stream_id())
            .to_bytes();
        (located, bytes)
    };
    let next_base = locate(&descriptor, synced)
        .expect("the synced position")
        .segment_base_position
        + segment;
    assert!(
        synced < next_base - term,
        "the log ends in the segment's first terms, so the tail crosses all of them"
    );
    let (last_term, padding) = header(
        FrameType::Pad,
        next_base - term,
        usize::try_from(term).expect("a term length"),
    );
    let padding_at = (
        log_dir.join(&last_term.segment_file),
        u64::try_from(last_term.offset_in_segment).expect("an offset"),
    );
    write_at(&padding_at.0, &padding, padding_at.1);
    let next = log_dir.join(segment_file_name(descriptor.recording_id(), next_base));
    std::fs::File::create(&next)
        .and_then(|file| file.set_len(u64::try_from(segment).expect("a length")))
        .expect("the next segment file is made");
    let (_, first_frame) = header(FrameType::Data, next_base, FRAME as usize);
    let mut frame = [0u8; FRAME as usize];
    frame[..first_frame.len()].copy_from_slice(&first_frame);
    write_at(&next, &frame, 0);
    (synced, padding_at, next)
}

fn write_at(path: &Path, bytes: &[u8], offset: u64) {
    use std::os::unix::fs::FileExt;
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .expect("the file opens")
        .write_all_at(bytes, offset)
        .expect("the bytes are written");
}

/// The hosts of all three members lose power together, each while its recorder held
/// a frame of the log that it had not synced, and that frame's body is lost: the case
/// in which a cluster could commit bytes that no client sent (work item 287; ADR
/// 0144). Started again, each member's driver cuts the recording to what it had
/// synced and counts the cut, so each member's log ends where the cluster's committed
/// log ended, and not one frame past it. The members elect in a term above the one
/// they held and the log goes on. A service beside each member reads all seventeen
/// entries from 0, the same bytes on each, and the first eleven are the bytes a
/// service read before the power was lost: no member hands the damaged frame on.
#[test]
fn three_members_whose_hosts_lost_power_together_start_again_on_what_they_synced() {
    power_lost_together("power", false);
}

/// As the test above, with each recorder written on past its sync to the end of the
/// segment file and rolled into the next before the power was lost (work item 471).
/// Started again, each member's driver cuts the recording to what it had synced in
/// the segment that holds it, zeroes the rest of that segment, the padding frame of its
/// last term among it, and removes the next segment file, which lies wholly past the
/// stop, counting the cut and the file removed. No member reads a recording past its
/// stop, so the next segment's removal shows in the counter and the directory alone;
/// the members' logs, and what every service reads, are as in the test above.
#[test]
fn three_members_whose_unsynced_tails_crossed_a_segment_roll_start_again_on_what_they_synced() {
    power_lost_together("power-across-a-roll", true);
}

/// The two tests above: the power lost on all three hosts, with each recorder's tail
/// in the segment that holds its synced position or, with `across_a_roll`, written on
/// into the next segment file.
fn power_lost_together(test: &str, across_a_roll: bool) {
    let dir = TestDir::new(test);
    let members_string = members_string();
    let mut members = start(dir.path(), &members_string);
    run_until_led(&mut members, -1);
    offer_entries(&mut members, 0, 10);
    let first = FRAME + 10 * FRAME;
    run_until_committed(&mut members, "the first entries committed", first);
    let first_term = members[0].module.leadership_term_id();
    let before = read_committed(&mut members, 0, first);
    assert_eq!(before.len(), 11);

    let mut ids = Vec::new();
    let mut rolled = Vec::new();
    for member in members {
        let id = member.module.member_id();
        member.module.close().expect("the member closes");
        member.driver.close().expect("the driver closes");
        if across_a_roll {
            let (synced, padding, next) = lose_power_across_a_roll(&member.log_dir, id);
            assert_eq!(synced, first, "member {id}");
            rolled.push((padding, next));
            ids.push(id);
            continue;
        }
        assert_eq!(lose_power(&member.log_dir, id), first, "member {id}");
        ids.push(id);
    }

    let mut members = start(dir.path(), &members_string);
    for member in &mut members {
        let static_counter = |id: cavalry_shm::counters::CounterId| {
            CountersReader::open(&member.dir)
                .expect("the counters file opens")
                .read(id.record())
                .expect("a static counter")
        };
        assert_eq!(
            (
                static_counter(RECORDING_RECOVERED),
                static_counter(RECORDING_CUT_TO_SYNCED),
                static_counter(RECORDING_FILES_REMOVED)
            ),
            (1, 1, i64::from(across_a_roll)),
            "member {}'s driver rebuilt the live recording, cut it, and removed a next \
             segment file it found",
            member.module.member_id()
        );
        // Each alone, before it has heard another: the log it synced, and not the
        // frame past it.
        run_until(
            std::slice::from_mut(member),
            "the member read its log back",
            |members| members[0].module.append_position() == first,
        );
    }
    for ((padding, offset), next) in &rolled {
        assert!(
            !next.exists(),
            "{} lay past the stop and is gone",
            next.display()
        );
        let mut bytes = [0u8; 32];
        use std::os::unix::fs::FileExt;
        std::fs::File::open(padding)
            .and_then(|file| file.read_exact_at(&mut bytes, *offset))
            .expect("the segment reads");
        assert_eq!(
            bytes,
            [0u8; 32],
            "the padding frame past the stop is zeroed in {}",
            padding.display()
        );
    }
    run_until_led(&mut members, first_term);
    offer_entries(&mut members, 10, 5);
    let second = first + FRAME + 5 * FRAME;
    run_until_committed(&mut members, "the log went on", second);

    let read: Vec<Read> = (0..3)
        .map(|index| read_committed(&mut members, index, second))
        .collect();
    assert_eq!(read[0].len(), 17);
    assert_eq!(read[0][..11], before[..]);
    assert_eq!(read[1], read[0]);
    assert_eq!(read[2], read[0]);
    assert!(
        read[0]
            .iter()
            .all(|(_, entry)| entry.iter().any(|&byte| byte != 0)),
        "no entry of zeros, which the lost body would have been"
    );

    for member in members {
        member.module.close().expect("the member closes");
        member.driver.close().expect("the driver closes");
    }
    assert_eq!(ids.len(), 3);
}
