//! The module's own tests: what `ConsensusModule` cannot be asked for from outside.
//! A member leads a term whose base is not position 0, gives its log up, or replays
//! its recordings to a member that is behind only after an election, which this
//! version's appointed leader never holds in one process (`spec/cluster.md`, "The
//! appointed leader"), so the tests here drive the module's [`Io`] itself, over a
//! driver (ADR 0091).

use std::path::PathBuf;

use cavalry_driver::Driver;
use cavalry_log::catalog::CatalogReader;

use super::*;

struct TestDir(PathBuf);

impl TestDir {
    fn new(test: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("cavalry-raft-module-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        TestDir(path)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// A member's ports, from the one file that says how they are taken and why, which the
// crate's tests under `tests/` share: `crates/cavalry-raft/tests/ports/mod.rs`.
#[path = "../../tests/ports/mod.rs"]
mod ports;
use ports::free_ports;

/// Calls `step` until it returns a value, for at most ten seconds.
fn wait_for<T>(what: &str, mut step: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(value) = step() {
            return value;
        }
        assert!(Instant::now() < deadline, "{what}");
        std::thread::sleep(Duration::from_micros(200));
    }
}

#[test]
fn the_channel_of_a_log_at_a_base_names_the_position() {
    let control = "cavalry:udp?control=127.0.0.1:20000|control-mode=manual|fc=min";
    let term = i64::from(UDP_TERM_LENGTH);
    assert_eq!(log_channel_at(control, 0).as_deref(), Some(control));
    assert_eq!(
        log_channel_at(control, 96),
        Some(format!("{control}|init-term-id=0|term-id=0|term-offset=96"))
    );
    assert_eq!(
        log_channel_at(control, 3 * term + 4_096),
        Some(format!(
            "{control}|init-term-id=0|term-id=3|term-offset=4096"
        ))
    );
    // The last term a `term-id` names, and the first position past it.
    let last = i64::from(i32::MAX) * term;
    assert_eq!(
        log_channel_at(control, last),
        Some(format!(
            "{control}|init-term-id=0|term-id=2147483647|term-offset=0"
        ))
    );
    assert_eq!(log_channel_at(control, last + term), None);
    assert_eq!(log_channel_at(control, -32), None);
}

/// A term whose base is past the first term of the log: the publication is created
/// at the base, the member's recording of it begins there, a follower's image of it
/// joins there, and the term's first entry lies at the base in both.
#[test]
fn a_term_whose_base_is_not_zero_is_published_and_recorded_from_its_base() {
    let dir = TestDir::new("term-base");
    let driver_dir = dir.0.join("driver");
    let log_dir = dir.0.join("log");
    // Member 0 is the module. Member 1 is played by the test: a subscription on its
    // `log` endpoint, on the same driver, is the follower the leader's log is sent to.
    let ports = free_ports(10);
    let member = |id: usize| {
        let endpoints: Vec<String> = ports[id * 5..id * 5 + 5]
            .iter()
            .map(|port| format!("127.0.0.1:{port}"))
            .collect();
        format!("{id},{}", endpoints.join(","))
    };
    let members = format!("{}|{}", member(0), member(1));
    let driver = Driver::start(&driver_dir, &log_dir).expect("the driver starts");
    let mut follower = Cavalry::connect_to(&driver_dir).expect("the follower's client");
    let mut followed = follower
        .add_subscription(
            &format!("cavalry:udp?endpoint=127.0.0.1:{}", ports[5 + 2]),
            LOG_STREAM_ID,
        )
        .expect("the follower subscribes on its log endpoint");
    let mut module =
        ConsensusModule::start(&driver_dir, &log_dir, &members, 0).expect("the member starts");

    // One whole term and three frames into the next.
    let base = i64::from(UDP_TERM_LENGTH) + 96;
    let log = module
        .io
        .add_log(1, base)
        .expect("the driver takes the commands")
        .expect("the log has its recording");
    assert_eq!(
        module
            .io
            .log_publication
            .as_ref()
            .expect("the publication")
            .position()
            .expect("the client is open"),
        base
    );
    let catalog = CatalogReader::open(&log_dir).expect("the catalog opens");
    let descriptor = wait_for("the recording's descriptor", || {
        catalog.get(log.recording_id).expect("the slot is read")
    });
    assert_eq!(descriptor.start_position(), base);
    // The follower's image of the term's publication joins at the base.
    let joined = wait_for("the follower's image of the log", || {
        followed.poll(|_, _| {}, 0).expect("the follower polls");
        followed
            .images()
            .first()
            .map(|image| (image.session_id(), image.position()))
    });
    assert_eq!(joined, (log.session_id, base));

    // The term's first entry, as leader ready appends it.
    let entry = Message::NewLeadershipTermEvent {
        leadership_term_id: 1,
        log_position: base,
        timestamp: 0,
        term_base_log_position: base,
        leader_member_id: 0,
        log_session_id: log.session_id,
        time_unit: cavalry_proto::cluster::TimeUnit::Millis,
        app_version: 0,
    };
    let mut encoded = [0u8; 128];
    let length = entry.write(&mut encoded).expect("the entry is encoded");
    wait_for("the entry is appended", || {
        module
            .io
            .append(&entry)
            .expect("the publication takes the entry")
            .then_some(())
    });
    // The frame header and the entry, to the frame alignment of 32 bytes.
    let after = base + i64::try_from((32 + length).div_ceil(32) * 32).expect("a frame's length");
    wait_for(
        "the member's append position reaches the entry's end",
        || (module.io.append_position() == after).then_some(()),
    );
    let mut read = Vec::new();
    wait_for("the follower reads the entry", || {
        followed
            .poll(|body, _| read.extend_from_slice(body), 1)
            .expect("the follower polls");
        (!read.is_empty()).then_some(())
    });
    assert_eq!(read, &encoded[..length]);
    assert_eq!(
        followed.images().first().map(|image| image.position()),
        Some(after)
    );

    // The same term again is a step tried again: the publication and the recording
    // held are the term's. Another term is refused while they are held, whatever
    // its base, since its entries would lie where this term's publication stands.
    let again = module
        .io
        .add_log(1, base)
        .expect("the same term")
        .expect("the log");
    assert_eq!(
        (again.session_id, again.recording_id),
        (log.session_id, log.recording_id)
    );
    for next_base in [after, 0] {
        assert!(
            matches!(
                module.io.add_log(2, next_base),
                Err(RaftError::Unsupported { .. })
            ),
            "a second term at {next_base}"
        );
    }
    drop(module);
    drop(follower);
    drop(driver);
}

/// A base the position keys cannot name is refused before any command is sent.
#[test]
fn a_term_whose_base_the_keys_cannot_name_is_refused() {
    let dir = TestDir::new("term-base-refused");
    let driver_dir = dir.0.join("driver");
    let log_dir = dir.0.join("log");
    let ports = free_ports(5);
    let endpoints: Vec<String> = ports
        .iter()
        .map(|port| format!("127.0.0.1:{port}"))
        .collect();
    let members = format!("0,{}", endpoints.join(","));
    let driver = Driver::start(&driver_dir, &log_dir).expect("the driver starts");
    let mut module =
        ConsensusModule::start(&driver_dir, &log_dir, &members, 0).expect("the member starts");
    for base in [
        -32,
        i64::from(i32::MAX) * i64::from(UDP_TERM_LENGTH) + i64::from(UDP_TERM_LENGTH),
    ] {
        let refused = module.io.add_log(0, base).expect_err("a base no key names");
        assert!(matches!(refused, RaftError::Unsupported { .. }), "{base}");
        assert!(refused.to_string().contains("position keys"), "{refused}");
        assert!(module.io.log_publication.is_none(), "{base}");
    }
    // The member is as it was: a term at a base the keys name is then added.
    assert!(
        module
            .io
            .add_log(0, 0)
            .expect("the driver takes the commands")
            .is_some()
    );
    drop(module);
    drop(driver);
}

/// Member 0 of two as a module over a driver of its own, with the other member played
/// by the test on the same driver: a subscription on member 1's `log` endpoint, the
/// follower the leader's log is sent to, and one on its `catchup` endpoint, where the
/// leader's replays arrive.
struct Leader {
    module: ConsensusModule,
    log: Subscription,
    catchup: Subscription,
    _follower: Cavalry,
    _driver: Driver,
    _dir: TestDir,
}

fn leader(test: &str) -> Leader {
    let dir = TestDir::new(test);
    let driver_dir = dir.0.join("driver");
    let log_dir = dir.0.join("log");
    let ports = free_ports(10);
    let member = |id: usize| {
        let endpoints: Vec<String> = ports[id * 5..id * 5 + 5]
            .iter()
            .map(|port| format!("127.0.0.1:{port}"))
            .collect();
        format!("{id},{}", endpoints.join(","))
    };
    let members = format!("{}|{}", member(0), member(1));
    let driver = Driver::start(&driver_dir, &log_dir).expect("the driver starts");
    let mut follower = Cavalry::connect_to(&driver_dir).expect("the follower's client");
    let endpoint = |port: u16| format!("cavalry:udp?endpoint=127.0.0.1:{port}");
    let log = follower
        .add_subscription(&endpoint(ports[5 + 2]), LOG_STREAM_ID)
        .expect("the follower subscribes on its log endpoint");
    let catchup = follower
        .add_subscription(&endpoint(ports[5 + 3]), LOG_STREAM_ID)
        .expect("the follower subscribes on its catchup endpoint");
    let module =
        ConsensusModule::start(&driver_dir, &log_dir, &members, 0).expect("the member starts");
    Leader {
        module,
        log,
        catchup,
        _follower: follower,
        _driver: driver,
        _dir: dir,
    }
}

/// An entry of the log whose payload tells it from the others: a session message of
/// no session.
fn entry(payload: &[u8]) -> Message<'_> {
    Message::SessionMessage {
        leadership_term_id: 0,
        cluster_session_id: crate::NO_SESSION,
        timestamp: 0,
        payload,
    }
}

impl Leader {
    /// Appends `payload` as an entry and returns the position after it, once the
    /// member's append position, what its recording has synced, has reached it.
    fn append(&mut self, payload: &[u8], from: i64) -> i64 {
        let message = entry(payload);
        let mut encoded = [0u8; 256];
        let length = message.write(&mut encoded).expect("the entry is encoded");
        wait_for("the entry is appended", || {
            self.log.poll(|_, _| {}, 16).expect("the follower polls");
            self.module
                .io
                .append(&message)
                .expect("the publication takes the entry")
                .then_some(())
        });
        let after = from + i64::try_from((32 + length).div_ceil(32) * 32).expect("a length");
        wait_for("the append position reaches the entry's end", || {
            self.log.poll(|_, _| {}, 16).expect("the follower polls");
            (self.module.io.append_position() == after).then_some(())
        });
        after
    }

    /// The session id of the replay held for member 1, which its image carries.
    fn replay_session(&self) -> i32 {
        self.module.io.replays[0]
            .as_ref()
            .expect("a replay is held for member 1")
            .session_id()
    }

    /// The position the image of the replay with `session_id` joined member 1's
    /// `catchup` endpoint at, read before a frame of it is.
    fn replay_joined(&mut self, session_id: i32) -> i64 {
        wait_for("the replay's image", || {
            self.catchup.poll(|_, _| {}, 0).expect("the follower polls");
            self.catchup
                .images()
                .iter()
                .find(|image| image.session_id() == session_id)
                .map(|image| image.position())
        })
    }

    /// Reads the entries the replay with `session_id` brings to member 1's `catchup`
    /// endpoint into `payloads`, until it holds `count` of them.
    fn read_entries(&mut self, session_id: i32, payloads: &mut Vec<Vec<u8>>, count: usize) {
        wait_for("the replay brings the entries", || {
            self.catchup
                .poll(
                    |body, header| {
                        if header.session_id() == session_id
                            && let Ok(Message::SessionMessage { payload, .. }) =
                                Message::parse(Stream::Log, body)
                        {
                            payloads.push(payload.to_vec());
                        }
                    },
                    16,
                )
                .expect("the follower polls");
            (payloads.len() == count).then_some(())
        });
    }

    /// Reads what the replay held for member 1 brings to its `catchup` endpoint until
    /// the driver has said how it ended: the position its image joined at, the
    /// payloads in order, and how it ended.
    fn read_replay(&mut self) -> (i64, Vec<Vec<u8>>, ReplayStopped) {
        let session_id = self.replay_session();
        let joined = self.replay_joined(session_id);
        let mut payloads = Vec::new();
        let stopped = wait_for("the replay's end", || {
            self.catchup
                .poll(
                    |body, header| {
                        if header.session_id() == session_id
                            && let Ok(Message::SessionMessage { payload, .. }) =
                                Message::parse(Stream::Log, body)
                        {
                            payloads.push(payload.to_vec());
                        }
                    },
                    16,
                )
                .expect("the follower polls");
            self.module
                .io
                .replay_stopped(1)
                .expect("the replay is read")
        });
        // The frames up to the end may still be unread when the end is said.
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            let read = self
                .catchup
                .poll(
                    |body, header| {
                        if header.session_id() == session_id
                            && let Ok(Message::SessionMessage { payload, .. }) =
                                Message::parse(Stream::Log, body)
                        {
                            payloads.push(payload.to_vec());
                        }
                    },
                    16,
                )
                .expect("the follower polls");
            if read == 0 {
                std::thread::sleep(Duration::from_millis(20));
                if self.catchup.poll(|_, _| {}, 0).expect("the follower polls") == 0
                    && !self
                        .catchup
                        .images()
                        .iter()
                        .any(|image| image.session_id() == session_id)
                {
                    break;
                }
            }
        }
        (joined, payloads, stopped)
    }
}

/// A leader gives its log up: its recording is stopped, the stop is where its log
/// ends, and the recording is kept for a replay with where it begins and where it
/// stopped. The publication is closed and its term forgotten, so the member leads the
/// next term in the same process, on a publication created at that term's base, which
/// the module refused before. A second call finds nothing to give up.
#[test]
fn a_leader_gives_its_log_up_and_leads_the_next_term_where_it_stopped() {
    let mut leader = leader("remove-log");
    let first = leader
        .module
        .io
        .add_log(0, 0)
        .expect("the driver takes the commands")
        .expect("the log has its recording");
    let one = leader.append(b"one", 0);
    let two = leader.append(b"two", one);

    leader.module.io.remove_log().expect("the log is given up");
    assert!(leader.module.io.log_publication.is_none());
    assert!(leader.module.io.log_recording.is_none());
    assert_eq!(leader.module.io.log_term, None);
    assert_eq!(leader.module.io.append_position(), two);
    assert_eq!(
        leader.module.io.kept.iter().copied().collect::<Vec<_>>(),
        vec![Kept {
            recording_id: first.recording_id,
            start: 0,
            stop: two,
        }]
    );
    leader.module.io.remove_log().expect("nothing to give up");
    assert_eq!(leader.module.io.kept.len(), 1);

    let second = leader
        .module
        .io
        .add_log(1, two)
        .expect("the driver takes the commands")
        .expect("the log has its recording");
    assert_ne!(second.recording_id, first.recording_id);
    assert_eq!(
        leader
            .module
            .io
            .log_publication
            .as_ref()
            .expect("the publication")
            .position()
            .expect("the client is open"),
        two
    );
    let three = leader.append(b"three", two);
    leader.module.io.remove_log().expect("the log is given up");
    assert_eq!(leader.module.io.append_position(), three);
    assert_eq!(
        leader.module.io.kept.iter().copied().collect::<Vec<_>>(),
        vec![
            Kept {
                recording_id: first.recording_id,
                start: 0,
                stop: two,
            },
            Kept {
                recording_id: second.recording_id,
                start: two,
                stop: three,
            },
        ]
    );
}

/// A leader replays the recordings it stopped to a member's `catchup` endpoint. The
/// replay's image joins at the position asked, brings the entries from there, and
/// ends by itself at the end asked or at the recording's stop, whichever comes first:
/// a replication that spans two recordings is two replays. How a replay ended is
/// handed over once. A position no recording holds, and a member that is none of the
/// cluster's, start nothing.
#[test]
fn a_leader_replays_the_recordings_it_stopped_to_a_members_catchup_endpoint() {
    let mut leader = leader("replay-kept");
    leader
        .module
        .io
        .add_log(0, 0)
        .expect("the driver takes the commands")
        .expect("the log has its recording");
    let one = leader.append(b"one", 0);
    let two = leader.append(b"two", one);
    leader.module.io.remove_log().expect("the log is given up");
    leader
        .module
        .io
        .add_log(1, two)
        .expect("the driver takes the commands")
        .expect("the log has its recording");
    let three = leader.append(b"three", two);
    leader.module.io.remove_log().expect("the log is given up");

    // From the start to an end in the second recording: the first recording alone.
    assert!(
        leader
            .module
            .io
            .start_replay(1, 0, Some(three))
            .expect("the replay starts")
    );
    let (joined, payloads, stopped) = leader.read_replay();
    assert_eq!(joined, 0);
    assert_eq!(payloads, vec![b"one".to_vec(), b"two".to_vec()]);
    assert_eq!(
        stopped,
        ReplayStopped {
            position: two,
            ended: true
        }
    );
    assert_eq!(
        leader.module.io.replay_stopped(1).expect("read again"),
        None,
        "handed over once"
    );

    // The next recording's, from where the last ended, to the end asked.
    assert!(
        leader
            .module
            .io
            .start_replay(1, two, Some(three))
            .expect("the replay starts")
    );
    let (joined, payloads, stopped) = leader.read_replay();
    assert_eq!(joined, two);
    assert_eq!(payloads, vec![b"three".to_vec()]);
    assert_eq!(
        stopped,
        ReplayStopped {
            position: three,
            ended: true
        }
    );

    // From the middle of a recording to an end below its stop.
    assert!(
        leader
            .module
            .io
            .start_replay(1, one, Some(two))
            .expect("the replay starts")
    );
    let (joined, payloads, stopped) = leader.read_replay();
    assert_eq!((joined, payloads), (one, vec![b"two".to_vec()]));
    assert_eq!(
        stopped,
        ReplayStopped {
            position: two,
            ended: true
        }
    );

    // A catch-up's replay, with no end, is of the log this member leads: a position
    // in a recording that has stopped starts none.
    assert!(
        !leader
            .module
            .io
            .start_replay(1, 0, None)
            .expect("an answer")
    );

    // The end of the log, with no live recording: nothing holds it. Nor does
    // anything for a member the cluster does not have.
    for (member, position) in [(1, three), (1, three + 64), (7, 0)] {
        assert!(
            !leader
                .module
                .io
                .start_replay(member, position, Some(three + 128))
                .expect("an answer"),
            "member {member} at {position}"
        );
    }
    assert_eq!(leader.module.io.replay_stopped(1).expect("read"), None);
}

/// A catch-up's replay, with no end, follows the leader's live recording: it brings
/// what the leader has appended and synced from the position asked, goes on as the
/// leader appends, and ends when the leader stops it, at the position it reached and
/// not by itself. A stop for a member with no replay does nothing.
#[test]
fn a_leader_follows_its_live_recording_until_it_stops_the_replay() {
    let mut leader = leader("replay-live");
    leader
        .module
        .io
        .add_log(0, 0)
        .expect("the driver takes the commands")
        .expect("the log has its recording");
    let one = leader.append(b"one", 0);
    leader.module.io.stop_replay(1).expect("nothing to stop");

    assert!(
        leader
            .module
            .io
            .start_replay(1, 0, None)
            .expect("the replay starts")
    );
    let session_id = leader.replay_session();
    let mut payloads: Vec<Vec<u8>> = Vec::new();
    leader.read_entries(session_id, &mut payloads, 1);
    assert_eq!(leader.module.io.replay_stopped(1).expect("read"), None);
    let two = leader.append(b"two", one);
    leader.read_entries(session_id, &mut payloads, 2);
    assert_eq!(payloads, vec![b"one".to_vec(), b"two".to_vec()]);

    leader
        .module
        .io
        .stop_replay(1)
        .expect("the replay is stopped");
    let stopped = wait_for("the replay's end", || {
        leader.module.io.replay_stopped(1).expect("read")
    });
    assert_eq!(
        stopped,
        ReplayStopped {
            position: two,
            ended: false
        }
    );
    leader.module.io.stop_replay(1).expect("nothing to stop");
}

/// A replay asked for a member the leader still replays to: the one running is
/// stopped first, and the new one starts once it has stopped, the order the core
/// leaves to the I/O. The earlier replay's end is forgotten. The new one is a
/// catch-up's from where the leader's log ends, which holds nothing yet: its image
/// joins there and brings what the leader appends after.
#[test]
fn a_replay_asked_while_one_runs_stops_it_first_and_may_begin_where_the_log_ends() {
    let mut leader = leader("replay-again");
    leader
        .module
        .io
        .add_log(0, 0)
        .expect("the driver takes the commands")
        .expect("the log has its recording");
    let one = leader.append(b"one", 0);
    assert!(
        leader
            .module
            .io
            .start_replay(1, 0, None)
            .expect("the replay starts")
    );
    let first = leader.replay_session();
    let mut payloads: Vec<Vec<u8>> = Vec::new();
    leader.read_entries(first, &mut payloads, 1);

    assert!(
        leader
            .module
            .io
            .start_replay(1, one, None)
            .expect("the replay starts")
    );
    let second = leader.replay_session();
    assert_ne!(second, first);
    assert_eq!(leader.replay_joined(second), one);
    // The first follows a live recording and has a subscriber: it ends only when it
    // is stopped, and its image goes from the subscription once it has.
    wait_for("the first replay's image goes", || {
        leader
            .catchup
            .poll(|_, _| {}, 16)
            .expect("the follower polls");
        (!leader
            .catchup
            .images()
            .iter()
            .any(|image| image.session_id() == first))
        .then_some(())
    });
    assert_eq!(
        leader.module.io.replay_stopped(1).expect("read"),
        None,
        "the first replay's end is forgotten, and the second runs"
    );

    let two = leader.append(b"two", one);
    let mut payloads: Vec<Vec<u8>> = Vec::new();
    leader.read_entries(second, &mut payloads, 1);
    assert_eq!(payloads, vec![b"two".to_vec()]);
    leader
        .module
        .io
        .stop_replay(1)
        .expect("the replay is stopped");
    let stopped = wait_for("the replay's end", || {
        leader.module.io.replay_stopped(1).expect("read")
    });
    assert_eq!(
        stopped,
        ReplayStopped {
            position: two,
            ended: false
        }
    );
}

/// A replay that no subscription takes ends by itself after the replay connect
/// timeout, 5 s, where it began, and is not one that ended at its end: the member
/// that asked is not there, and the core follows it with nothing.
#[test]
fn a_replay_with_no_subscriber_is_said_not_to_have_ended_at_its_end() {
    let mut leader = leader("replay-unheard");
    leader
        .module
        .io
        .add_log(0, 0)
        .expect("the driver takes the commands")
        .expect("the log has its recording");
    let one = leader.append(b"one", 0);
    leader.module.io.remove_log().expect("the log is given up");
    // Member 1 is no longer on its catchup endpoint.
    let Leader {
        mut module,
        catchup,
        log,
        _follower,
        _driver,
        _dir,
    } = leader;
    drop(catchup);
    assert!(
        module
            .io
            .start_replay(1, 0, Some(one))
            .expect("the replay starts")
    );
    let started = Instant::now();
    let stopped = wait_for("the replay's end", || {
        module.io.replay_stopped(1).expect("read")
    });
    assert_eq!(
        stopped,
        ReplayStopped {
            position: 0,
            ended: false
        }
    );
    assert!(
        started.elapsed() >= Duration::from_secs(4),
        "{:?}",
        started.elapsed()
    );
    drop(module);
    drop(log);
}

/// The table of recordings kept holds the last 1,024: one more forgets the oldest,
/// which the member then no longer replays, and the others keep their order.
#[test]
fn the_recordings_kept_are_the_last_1024() {
    let mut leader = leader("kept-bound");
    let io = &mut leader.module.io;
    for id in 0..i64::try_from(KEPT_RECORDINGS).expect("a count") {
        io.keep(Kept {
            recording_id: id,
            start: id * 32,
            stop: id * 32 + 32,
        });
    }
    assert_eq!(io.kept.len(), KEPT_RECORDINGS);
    assert_eq!(io.recording_at(0), Some((0, Some(32))));
    io.keep(Kept {
        recording_id: 5_000,
        start: 32_768,
        stop: 32_800,
    });
    assert_eq!(io.kept.len(), KEPT_RECORDINGS);
    assert_eq!(io.kept.capacity(), KEPT_RECORDINGS, "no growth");
    assert_eq!(io.recording_at(0), None, "the oldest is forgotten");
    assert_eq!(io.recording_at(32), Some((1, Some(64))));
    assert_eq!(io.recording_at(32_768), Some((5_000, Some(32_800))));
    assert_eq!(io.recording_at(32_800), None);
}

/// Members 0 and 1 of two, each a module over a driver of its own, as in a cluster:
/// member 0 leads and member 1 follows, and the test drives the [`Io`] of both.
struct Pair {
    leader: ConsensusModule,
    member: ConsensusModule,
    leader_driver_dir: PathBuf,
    member_driver_dir: PathBuf,
    member_log_dir: PathBuf,
    _drivers: Vec<Driver>,
    _dir: TestDir,
}

fn pair(test: &str) -> Pair {
    let dir = TestDir::new(test);
    let ports = free_ports(10);
    let member = |id: usize| {
        let endpoints: Vec<String> = ports[id * 5..id * 5 + 5]
            .iter()
            .map(|port| format!("127.0.0.1:{port}"))
            .collect();
        format!("{id},{}", endpoints.join(","))
    };
    let members = format!("{}|{}", member(0), member(1));
    let mut drivers = Vec::new();
    let mut modules = Vec::new();
    for id in 0..2 {
        let driver_dir = dir.0.join(format!("driver-{id}"));
        let log_dir = dir.0.join(format!("log-{id}"));
        drivers.push(Driver::start(&driver_dir, &log_dir).expect("the driver starts"));
        modules.push(
            ConsensusModule::start(&driver_dir, &log_dir, &members, id).expect("the member starts"),
        );
    }
    let member = modules.pop().expect("member 1");
    let leader = modules.pop().expect("member 0");
    Pair {
        leader,
        member,
        leader_driver_dir: dir.0.join("driver-0"),
        member_driver_dir: dir.0.join("driver-1"),
        member_log_dir: dir.0.join("log-1"),
        _drivers: drivers,
        _dir: dir,
    }
}

impl Pair {
    /// Appends `payload` on the leader and returns the position after it, once the
    /// leader's append position has reached it.
    fn append(&mut self, payload: &[u8], from: i64) -> i64 {
        let message = entry(payload);
        let mut encoded = [0u8; 256];
        let length = message.write(&mut encoded).expect("the entry is encoded");
        wait_for("the entry is appended", || {
            self.leader
                .io
                .append(&message)
                .expect("the publication takes the entry")
                .then_some(())
        });
        let after = from + i64::try_from((32 + length).div_ceil(32) * 32).expect("a length");
        wait_for(
            "the leader's append position reaches the entry's end",
            || (self.leader.io.append_position() == after).then_some(()),
        );
        after
    }
}

impl Pair {
    /// The start position the catalog of member 1's driver holds for a recording.
    fn start_of(&self, recording_id: i64) -> i64 {
        let catalog = CatalogReader::open(&self.member_log_dir).expect("the catalog opens");
        catalog
            .get(recording_id)
            .expect("the slot is read")
            .expect("the recording's descriptor")
            .start_position()
    }

    /// Runs member 1's observation of its log subscription until `done` says so.
    fn observe<T>(&mut self, what: &str, mut done: impl FnMut(&mut ClientIo) -> Option<T>) -> T {
        wait_for(what, || {
            self.member.io.observe_log().expect("the log is observed");
            done(&mut self.member.io)
        })
    }
}

/// A follower's recording that was armed before the leader's image arrived begins at
/// the image's join position. The election is told that position once the driver
/// has started the recording, and from then the recording is the member's log: its
/// append position follows what the recording has synced, the module names the
/// recording, and when the member leaves the log the recording is kept from where it
/// begins to where it stopped (ADR 0093).
#[test]
fn a_recording_armed_before_its_image_is_told_at_the_join_and_counted() {
    let mut pair = pair("armed-first");
    pair.member.io.join_log().expect("subscribed and armed");
    let log = pair
        .leader
        .io
        .add_log(0, 0)
        .expect("the driver takes the commands")
        .expect("the log has its recording");
    let one = pair.append(b"one", 0);
    let start = pair.observe("the recording's start is told", |io| {
        io.log_image(log.session_id).expect("the image is read")
    });
    assert_eq!(start, 0);
    let recording_id = pair
        .member
        .log_recording_id()
        .expect("the recording is named");
    assert_eq!(pair.start_of(recording_id), 0);
    pair.observe(
        "the member's append position reaches the entry's end",
        |io| (io.append_position() == one).then_some(()),
    );
    pair.member.io.leave_log().expect("the log is left");
    assert_eq!(pair.member.io.append_position(), one);
    assert_eq!(
        pair.member.io.kept.iter().copied().collect::<Vec<_>>(),
        vec![Kept {
            recording_id,
            start: 0,
            stop: one,
        }]
    );
}

/// Card 269. A follower joins the log with two commands, and an image that arrives
/// between them is there before the recording is armed: the recording then begins
/// at what the image had received, past its join position. The test makes the gap
/// wide: the member subscribes, the leader's first entry reaches the member's
/// driver, and only then does the member arm.
///
/// The module takes where the recording begins from the catalog. It tells the
/// election that position, 96, where the image joined at 0: an image that joined past
/// the member's append position, which follower log await fails on. The recording is
/// no part of the member's log: the append position stays 0 while the recording
/// syncs a second entry, the module names no recording, and the member that leaves
/// the log keeps nothing. Before ADR 0093 the module told the election 0 and
/// reported an append position of 192, with the first entry in no recording.
#[test]
fn a_recording_armed_after_its_image_arrived_is_told_where_it_begins_and_never_counted() {
    let mut pair = pair("armed-late");
    let log = pair
        .leader
        .io
        .add_log(0, 0)
        .expect("the driver takes the commands")
        .expect("the log has its recording");
    // The first of `join_log`'s two commands, and not yet the second.
    {
        let io = &mut pair.member.io;
        io.log_subscription = Some(
            io.client
                .add_subscription(&io.log_endpoint_channel, LOG_STREAM_ID)
                .expect("the member subscribes"),
        );
    }
    // A second subscriber on the member's driver, to know what the image has
    // received: the member's own subscription reads nothing of it.
    let mut watcher = Cavalry::connect_to(&pair.member_driver_dir).expect("the watcher");
    let channel = pair.member.io.log_endpoint_channel.clone();
    let mut watch = watcher
        .add_subscription(&channel, LOG_STREAM_ID)
        .expect("the watcher subscribes");
    let one = pair.append(b"one", 0);
    let mut seen = 0;
    wait_for("the entry reaches the member's driver", || {
        seen += watch.poll(|_, _| {}, 16).expect("the watcher polls");
        (seen >= 1).then_some(())
    });
    assert_eq!(
        pair.member.io.log_image(log.session_id).expect("read"),
        None,
        "no recording has started"
    );

    pair.member.io.join_log().expect("armed, late");
    let start = pair.observe("the recording's start is told", |io| {
        io.log_image(log.session_id).expect("the image is read")
    });
    assert_eq!(
        start, one,
        "where the recording begins, not where the image joined"
    );
    let recording_id = pair
        .member
        .io
        .held
        .as_ref()
        .expect("the recording is held")
        .recording
        .recording_id();
    assert_eq!(pair.start_of(recording_id), one);

    let two = pair.append(b"two", one);
    pair.observe("the member's recording syncs the second entry", |io| {
        io.held
            .as_ref()
            .filter(|held| held.recording.synced_position() >= two)
            .map(|_| ())
    });
    assert_eq!(
        pair.member.io.append_position(),
        0,
        "the recording begins past where the member's log ends"
    );
    assert_eq!(pair.member.log_recording_id(), None);
    pair.member.io.leave_log().expect("the log is left");
    assert_eq!(pair.member.io.append_position(), 0);
    assert!(pair.member.io.kept.is_empty());
}

/// An armed recording takes the next image on the endpoint, whatever its session.
/// One taken by an image the election does not ask for, here a publication of the
/// test's own on the member's `log` endpoint, is never told to the election and is no
/// part of the member's log, though it begins where that log ends: the append
/// position does not move as it syncs, the module names no recording, and the member
/// that leaves the log keeps nothing (ADR 0093).
#[test]
fn a_recording_of_another_sessions_image_is_never_told_and_never_counted() {
    let mut pair = pair("another-session");
    pair.member.io.join_log().expect("subscribed and armed");
    let mut stranger = Cavalry::connect_to(&pair.leader_driver_dir).expect("the stranger");
    let channel = pair.member.io.log_endpoint_channel.clone();
    let mut stray = stranger
        .add_exclusive_publication(&channel, LOG_STREAM_ID)
        .expect("the stray publication");
    let message = entry(b"of no term");
    let mut encoded = [0u8; 256];
    let length = message.write(&mut encoded).expect("the entry is encoded");
    wait_for("the stray entry is published", || {
        pair.member.io.observe_log().expect("the log is observed");
        stray.offer(&encoded[..length]).is_ok().then_some(())
    });
    let end = i64::try_from((32 + length).div_ceil(32) * 32).expect("a length");
    pair.observe("the stray image's recording syncs the entry", |io| {
        io.held
            .as_ref()
            .filter(|held| held.recording.synced_position() >= end)
            .map(|_| ())
    });
    let held = pair.member.io.held.as_ref().expect("the recording is held");
    assert_eq!(held.start, 0, "it begins where the member's log ends");
    let stray_session = held.recording.session_id();
    let wanted = stray_session.wrapping_add(1);
    assert_eq!(
        pair.member.io.log_image(wanted).expect("read"),
        None,
        "the election asks for another session's image"
    );
    assert_eq!(pair.member.io.append_position(), 0);
    assert_eq!(pair.member.log_recording_id(), None);
    pair.member.io.leave_log().expect("the log is left");
    assert_eq!(pair.member.io.append_position(), 0);
    assert!(pair.member.io.kept.is_empty());
}
