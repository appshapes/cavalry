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

/// A member whose log directory is not its driver's is refused at its start, before
/// anything is written there (ADR 0093). The directory holds no catalog: the error is
/// `RaftError::Catalog`, its reason `catalog`, its message names the page's "Members
/// and streams", and the directory is as empty as it was, with no `terms` file.
#[test]
fn a_member_started_on_a_directory_with_no_catalog_is_refused_and_writes_nothing_there() {
    let dir = TestDir::new("no-catalog");
    let driver_dir = dir.0.join("driver");
    let other = dir.0.join("not-the-drivers");
    std::fs::create_dir_all(&other).expect("the other directory");
    let ports = free_ports(10);
    let member = |id: usize| {
        let endpoints: Vec<String> = ports[id * 5..id * 5 + 5]
            .iter()
            .map(|port| format!("127.0.0.1:{port}"))
            .collect();
        format!("{id},{}", endpoints.join(","))
    };
    let members = format!("{}|{}", member(0), member(1));
    let _driver = Driver::start(&driver_dir, dir.0.join("log")).expect("the driver starts");

    let Err(error) = ConsensusModule::start(&driver_dir, &other, &members, 0) else {
        panic!("a member started on a directory with no catalog");
    };
    assert!(matches!(error, RaftError::Catalog(_)), "{error}");
    assert_eq!(error.reason(), "catalog");
    let message = error.to_string();
    assert!(message.contains(DOC_MEMBERS), "{message}");
    assert_eq!(
        std::fs::read_dir(&other)
            .expect("the other directory is read")
            .count(),
        0,
        "nothing was written into the directory"
    );
}

/// A catalog that is not the driver's gives no start position (ADR 0093). The
/// leader's driver holds a recording of another stream in slot 0 of its catalog.
/// The member's driver starts the member's recording of the log, recording 0 of its
/// own catalog. With the leader's catalog in the member's hands, the slot holds a
/// recording of another session and stream: the hand-over is `RaftError::Catalog`
/// with `unknown`, on every duty cycle, the election is told nothing and nothing is
/// counted. With its own driver's catalog the member takes the recording on its next
/// duty cycle, and tells the election where it begins.
#[test]
fn a_catalog_slot_that_holds_another_recording_gives_no_start() {
    let mut pair = pair("another-catalog");
    let leader_log_dir = pair._dir.0.join("log-0");
    let mut stranger = Cavalry::connect_to(&pair.leader_driver_dir).expect("the stranger");
    let _other = stranger
        .add_exclusive_publication("cavalry:ipc", 7)
        .expect("a publication of another stream");
    let other = stranger
        .start_recording("cavalry:ipc", 7)
        .expect("its recording");
    assert_eq!(other.recording_id(), 0);

    pair.member.io.join_log().expect("subscribed and armed");
    let log = pair
        .leader
        .io
        .add_log(0, 0)
        .expect("the driver takes the commands")
        .expect("the log has its recording");
    pair.member.io.catalog = CatalogReader::open(&leader_log_dir).expect("the catalog opens");
    let error = wait_for("the hand-over is refused", || {
        pair.member.io.observe_log().err()
    });
    assert!(
        matches!(
            error,
            RaftError::Catalog(CatalogError::Unknown { recording_id: 0 })
        ),
        "{error}"
    );
    assert_eq!(error.reason(), "catalog");
    for _ in 0..3 {
        assert!(matches!(
            pair.member.io.observe_log(),
            Err(RaftError::Catalog(CatalogError::Unknown {
                recording_id: 0
            }))
        ));
    }
    assert!(pair.member.io.held.is_none());
    assert_eq!(
        pair.member.io.log_image(log.session_id).expect("read"),
        None
    );
    assert_eq!(pair.member.io.append_position(), 0);

    pair.member.io.catalog = CatalogReader::open(&pair.member_log_dir).expect("the catalog opens");
    let start = pair.observe("the recording's start is told", |io| {
        io.log_image(log.session_id).expect("the image is read")
    });
    assert_eq!(start, 0);
    assert_eq!(pair.member.log_recording_id(), Some(0));
}

impl Pair {
    /// A subscriber on member 1's `log` endpoint that is no part of the member: with
    /// it the leader's log publication has a receiver and takes appends while the
    /// member itself is behind and out of the log.
    fn receive_log(&mut self) -> (Cavalry, Subscription) {
        let mut client = Cavalry::connect_to(&self.member_driver_dir).expect("a receiver");
        let channel = self.member.io.log_endpoint_channel.clone();
        let subscription = client
            .add_subscription(&channel, LOG_STREAM_ID)
            .expect("the receiver subscribes");
        (client, subscription)
    }

    /// Runs member 1's observation of both its subscriptions until `done` says so.
    fn observe_both<T>(
        &mut self,
        what: &str,
        mut done: impl FnMut(&mut ClientIo) -> Option<T>,
    ) -> T {
        wait_for(what, || {
            self.member.io.observe_log().expect("the log is observed");
            self.member
                .io
                .observe_catchup()
                .expect("the catch-up is observed");
            done(&mut self.member.io)
        })
    }
}

/// A member replicates a term that lies in two of its leader's recordings, asking
/// for each replay as its core does (ADR 0092), over two drivers (ADR 0094). For
/// each: it subscribes on its `catchup` endpoint with a recording armed, and its
/// catch-up position carries its id and that endpoint; the leader replays the
/// recording that holds the position; the election is told where the member's
/// recording of the image begins, which is where it asked from; the append position
/// follows what that recording syncs; when the replay has ended the image is said to
/// have gone; and the member removes the subscription, keeping the recording from
/// where it begins to where it stopped, which is where its log ends.
#[test]
fn a_member_records_each_replay_it_asks_for_and_its_log_ends_where_each_stopped() {
    let mut pair = pair("replicate");
    let _receiver = pair.receive_log();
    pair.leader
        .io
        .add_log(0, 0)
        .expect("the driver takes the commands")
        .expect("the log has its recording");
    let one = pair.append(b"one", 0);
    let two = pair.append(b"two", one);
    pair.leader.io.remove_log().expect("the log is given up");
    pair.leader
        .io
        .add_log(1, two)
        .expect("the driver takes the commands")
        .expect("the log has its recording");
    let three = pair.append(b"three", two);
    pair.leader.io.remove_log().expect("the log is given up");

    assert_eq!(pair.member.io.catchup_image().expect("read"), None);
    let mut kept = Vec::new();
    for (from, to) in [(0, two), (two, three)] {
        pair.member.io.add_catchup().expect("subscribed and armed");
        wait_for("the catch-up position leaves", || {
            pair.member
                .io
                .send_catchup_position(0, 1, from)
                .then_some(())
        });
        let asked = wait_for("the leader reads the catch-up position", || {
            let mut read = None;
            pair.leader
                .consensus
                .poll(
                    |body, _| {
                        if let Ok(Message::CatchupPosition {
                            leadership_term_id,
                            log_position,
                            follower_member_id,
                            catchup_endpoint,
                        }) = Message::parse(Stream::Consensus, body)
                        {
                            read = Some((
                                leadership_term_id,
                                log_position,
                                follower_member_id,
                                catchup_endpoint.to_vec(),
                            ));
                        }
                    },
                    16,
                )
                .expect("the leader polls");
            read
        });
        assert_eq!(
            asked,
            (
                1,
                from,
                1,
                pair.member.io.catchup_endpoint.as_bytes().to_vec()
            )
        );

        assert!(
            pair.leader
                .io
                .start_replay(1, from, Some(three))
                .expect("the replay starts")
        );
        let start = pair.observe_both("the replay's image is told", |io| {
            io.catchup_image().expect("the image is read")
        });
        assert_eq!(start, from, "where the member asked from");
        pair.observe_both("the append position reaches the recording's end", |io| {
            (io.append_position() == to).then_some(())
        });
        pair.observe_both("the image is said to have gone", |io| {
            io.catchup_image()
                .expect("the image is read")
                .is_none()
                .then_some(())
        });
        let recording_id = pair
            .member
            .io
            .catchup_held
            .as_ref()
            .expect("the recording is held")
            .recording
            .recording_id();
        assert_eq!(pair.start_of(recording_id), from);
        pair.member
            .io
            .remove_catchup()
            .expect("the subscription is removed");
        assert!(pair.member.io.catchup_subscription.is_none());
        assert_eq!(pair.member.io.append_position(), to);
        kept.push(Kept {
            recording_id,
            start: from,
            stop: to,
        });
        assert_eq!(
            pair.member.io.kept.iter().copied().collect::<Vec<_>>(),
            kept
        );
        assert_eq!(pair.member.io.catchup_image().expect("read"), None);
    }
    pair.member.io.remove_catchup().expect("nothing to remove");
    assert_eq!(pair.member.log_recording_id(), None, "it is in no log");
}

/// A catch-up within the leader's term, with the live add (state 13) over two
/// drivers: the member records a replay that follows the leader's live recording,
/// then adds its `log` endpoint's subscription while the replay runs. The election
/// is told where the live recording begins, at or past the member's append position.
/// From there the two recordings overlap and the append position follows either.
/// The leader stops the replay and the member removes its catch-up subscription: its
/// log ends at the further of what the two hold, and goes on by the live recording
/// alone. When it leaves the log it keeps both recordings, the replay's first.
#[test]
fn a_member_that_catches_up_adds_the_live_log_and_goes_on_by_it() {
    let mut pair = pair("catch-up");
    let _receiver = pair.receive_log();
    let log = pair
        .leader
        .io
        .add_log(0, 0)
        .expect("the driver takes the commands")
        .expect("the log has its recording");
    let one = pair.append(b"one", 0);
    let two = pair.append(b"two", one);

    pair.member.io.add_catchup().expect("subscribed and armed");
    assert!(
        pair.leader
            .io
            .start_replay(1, 0, None)
            .expect("the replay starts")
    );
    let start = pair.observe_both("the replay's image is told", |io| {
        io.catchup_image().expect("the image is read")
    });
    assert_eq!(start, 0);
    pair.observe_both("the append position reaches the leader's", |io| {
        (io.append_position() == two).then_some(())
    });
    assert_eq!(
        pair.member.log_recording_id(),
        None,
        "not on the live log yet"
    );

    // The live add: the image of the leader's log is there already, so the recording
    // begins at what it has received, at or past the member's append position.
    pair.member.io.join_log().expect("subscribed and armed");
    let live_start = pair.observe_both("the live recording's start is told", |io| {
        io.log_image(log.session_id).expect("the image is read")
    });
    assert!(live_start >= two, "{live_start}");
    let three = pair.append(b"three", two);
    pair.observe_both("the append position reaches the third entry", |io| {
        (io.append_position() == three).then_some(())
    });
    let live_id = pair
        .member
        .log_recording_id()
        .expect("the live recording is the member's log now");

    pair.leader
        .io
        .stop_replay(1)
        .expect("the replay is stopped");
    let replay_id = pair
        .member
        .io
        .catchup_held
        .as_ref()
        .expect("the replay's recording is held")
        .recording
        .recording_id();
    pair.member
        .io
        .remove_catchup()
        .expect("the catch-up subscription is removed");
    assert_eq!(pair.member.io.append_position(), three);
    let replay_stop = pair.member.io.stopped_at;
    assert!(replay_stop >= live_start, "{replay_stop} {live_start}");

    let four = pair.append(b"four", three);
    pair.observe_both("the append position follows the live recording", |io| {
        (io.append_position() == four).then_some(())
    });
    pair.member.io.leave_log().expect("the log is left");
    assert_eq!(pair.member.io.append_position(), four);
    assert_eq!(
        pair.member.io.kept.iter().copied().collect::<Vec<_>>(),
        vec![
            Kept {
                recording_id: replay_id,
                start: 0,
                stop: replay_stop,
            },
            Kept {
                recording_id: live_id,
                start: live_start,
                stop: four,
            },
        ]
    );
    // Where the two overlap, a replay of this member's own is of the one that
    // begins first.
    assert_eq!(
        pair.member.io.recording_at(0),
        Some((replay_id, Some(replay_stop)))
    );
    assert_eq!(
        pair.member.io.recording_at(three),
        Some((live_id, Some(four)))
    );
}

/// The live recording begins where the member's log ended when it subscribed, or
/// past it, and the replay goes on meanwhile. By the duty cycle in which the driver
/// hands the live recording over, the replay's recording may have carried the log
/// past the live recording's start: the live recording counts all the same, since
/// what it must not begin below is where the log ended when the member subscribed,
/// not where it ends when the recording is handed over. Here the member subscribes
/// on its `log` endpoint, the leader appends a second entry, which the replay
/// brings, and only then does the member look at its log subscription. The replay
/// is stopped, and a third entry reaches the member by the live recording alone.
#[test]
fn a_live_recording_counts_though_the_replay_passed_its_start_before_it_was_handed_over() {
    let mut pair = pair("handed-over-late");
    let _receiver = pair.receive_log();
    let log = pair
        .leader
        .io
        .add_log(0, 0)
        .expect("the driver takes the commands")
        .expect("the log has its recording");
    let one = pair.append(b"one", 0);
    pair.member.io.add_catchup().expect("subscribed and armed");
    assert!(
        pair.leader
            .io
            .start_replay(1, 0, None)
            .expect("the replay starts")
    );
    pair.observe_both("the replay's image is told", |io| {
        io.catchup_image().expect("the image is read")
    });
    pair.observe_both("the append position reaches the leader's", |io| {
        (io.append_position() == one).then_some(())
    });

    pair.member.io.join_log().expect("subscribed and armed");
    let two = pair.append(b"two", one);
    wait_for("the replay brings the second entry", || {
        pair.member
            .io
            .observe_catchup()
            .expect("the catch-up is observed");
        (pair.member.io.append_position() == two).then_some(())
    });
    let live_start = pair.observe_both("the live recording's start is told", |io| {
        io.log_image(log.session_id).expect("the image is read")
    });
    assert!(
        (one..two).contains(&live_start),
        "it began before the replay passed it: {live_start}"
    );

    pair.leader
        .io
        .stop_replay(1)
        .expect("the replay is stopped");
    pair.observe_both("the replay's recording has stopped", |io| {
        io.catchup_held
            .as_ref()
            .and_then(|held| held.recording.stop_position())
    });
    let three = pair.append(b"three", two);
    pair.observe_both("the append position follows the live recording", |io| {
        (io.append_position() == three).then_some(())
    });
}

/// A replay short enough to come and go between two of the member's duty cycles is
/// still told to the election: its recording has stopped before the election first
/// asks, and the first answer is where it begins all the same, so that its image is
/// checked and its bytes counted. The answer after is that the image has gone.
#[test]
fn a_replay_that_ended_before_the_election_asked_is_told_once_and_then_gone() {
    let mut pair = pair("short-replay");
    let _receiver = pair.receive_log();
    pair.leader
        .io
        .add_log(0, 0)
        .expect("the driver takes the commands")
        .expect("the log has its recording");
    let one = pair.append(b"one", 0);
    pair.leader.io.remove_log().expect("the log is given up");

    pair.member.io.add_catchup().expect("subscribed and armed");
    assert!(
        pair.leader
            .io
            .start_replay(1, 0, Some(one))
            .expect("the replay starts")
    );
    // The duty cycles observe; the election does not ask until the replay is over.
    pair.observe_both("the replay's recording has stopped", |io| {
        io.catchup_held
            .as_ref()
            .and_then(|held| held.recording.stop_position())
    });
    assert_eq!(pair.member.io.append_position(), 0, "not told yet");
    assert_eq!(pair.member.io.catchup_image().expect("read"), Some(0));
    assert_eq!(pair.member.io.append_position(), one);
    assert_eq!(pair.member.io.catchup_image().expect("read"), None);
    pair.member
        .io
        .remove_catchup()
        .expect("the subscription is removed");
    assert_eq!(pair.member.io.append_position(), one);
    assert_eq!(pair.member.io.kept.len(), 1);
}

/// A member that begins again in follower catch-up leaves the log before it removes
/// its catch-up subscription, as init does. It holds two recordings then, and the
/// live one begins inside what the replay's holds: the replay's is stopped first, so
/// that both are kept in order and the log ends at the further of the two stops,
/// here the live recording's, which holds an entry the replay never brought.
#[test]
fn a_member_that_leaves_in_a_catch_up_keeps_both_recordings() {
    let mut pair = pair("leave-in-catch-up");
    let _receiver = pair.receive_log();
    let log = pair
        .leader
        .io
        .add_log(0, 0)
        .expect("the driver takes the commands")
        .expect("the log has its recording");
    let one = pair.append(b"one", 0);
    pair.member.io.add_catchup().expect("subscribed and armed");
    assert!(
        pair.leader
            .io
            .start_replay(1, 0, None)
            .expect("the replay starts")
    );
    pair.observe_both("the replay's image is told", |io| {
        io.catchup_image().expect("the image is read")
    });
    pair.observe_both("the append position reaches the leader's", |io| {
        (io.append_position() == one).then_some(())
    });
    pair.member.io.join_log().expect("subscribed and armed");
    let live_start = pair.observe_both("the live recording's start is told", |io| {
        io.log_image(log.session_id).expect("the image is read")
    });
    // The replay stops; what the leader appends after reaches the live recording
    // alone.
    pair.leader
        .io
        .stop_replay(1)
        .expect("the replay is stopped");
    pair.observe_both("the replay's recording has stopped", |io| {
        io.catchup_held
            .as_ref()
            .and_then(|held| held.recording.stop_position())
    });
    let two = pair.append(b"two", one);
    pair.observe_both("the append position follows the live recording", |io| {
        (io.append_position() == two).then_some(())
    });

    pair.member.io.leave_log().expect("the log is left");
    pair.member
        .io
        .remove_catchup()
        .expect("the catch-up subscription is removed");
    assert_eq!(pair.member.io.append_position(), two);
    let kept: Vec<_> = pair.member.io.kept.iter().copied().collect();
    assert_eq!(kept.len(), 2, "{kept:?}");
    assert_eq!((kept[0].start, kept[1].start), (0, live_start));
    assert_eq!(kept[1].stop, two);
    assert!(kept[0].stop >= live_start, "{kept:?}");
}

/// A recording of the live log whose image joined below where the member's log ends
/// is no part of that log: the election is told where it begins, which is a join
/// position mismatch, and the member's append position never moves by it (ADR 0094).
/// The member follows term 0 to its first entry and leaves; the test's leader then
/// leads a log that begins at 0 again.
#[test]
fn a_live_recording_that_begins_below_the_members_log_end_is_never_counted() {
    let mut pair = pair("below");
    pair.member.io.join_log().expect("subscribed and armed");
    let log = pair
        .leader
        .io
        .add_log(0, 0)
        .expect("the driver takes the commands")
        .expect("the log has its recording");
    let one = pair.append(b"one", 0);
    pair.observe_both("the first log is told", |io| {
        io.log_image(log.session_id).expect("the image is read")
    });
    pair.observe_both("the member holds the first entry", |io| {
        (io.append_position() == one).then_some(())
    });
    pair.member.io.leave_log().expect("the log is left");
    pair.leader.io.remove_log().expect("the log is given up");

    // A log of another term that begins at 0, below where the member's log ends.
    pair.member.io.join_log().expect("subscribed and armed");
    let again = pair
        .leader
        .io
        .add_log(1, 0)
        .expect("the driver takes the commands")
        .expect("the log has its recording");
    let start = pair.observe_both("the second log is told", |io| {
        io.log_image(again.session_id).expect("the image is read")
    });
    assert_eq!(start, 0, "below the member's append position, {one}");
    let message = entry(b"of another log, two entries long");
    let mut encoded = [0u8; 256];
    let length = message.write(&mut encoded).expect("the entry is encoded");
    let end = i64::try_from((32 + length).div_ceil(32) * 32).expect("a length") * 2;
    for _ in 0..2 {
        wait_for("the entry is appended", || {
            pair.leader
                .io
                .append(&message)
                .expect("the publication takes the entry")
                .then_some(())
        });
    }
    assert!(end > one);
    pair.observe_both("the member's recording syncs past its log's end", |io| {
        io.held
            .as_ref()
            .filter(|held| held.recording.synced_position() >= end)
            .map(|_| ())
    });
    assert_eq!(pair.member.io.append_position(), one);
    assert_eq!(pair.member.log_recording_id(), None);
    pair.member.io.leave_log().expect("the log is left");
    assert_eq!(pair.member.io.append_position(), one);
    assert_eq!(pair.member.io.kept.len(), 1);
}

/// Why the catch-up position is encoded into the module's scratch: with a host name
/// of the length DNS allows, the message is longer than the 128 bytes the other
/// consensus messages are encoded into, and fits 1,408.
#[test]
fn a_catch_up_position_with_a_long_host_name_does_not_fit_128_bytes() {
    let endpoint = format!("{}.example:65535", "a".repeat(240));
    let message = Message::CatchupPosition {
        leadership_term_id: 1,
        log_position: 0,
        follower_member_id: 1,
        catchup_endpoint: endpoint.as_bytes(),
    };
    assert!(message.write(&mut [0u8; 128]).is_err());
    let length = message
        .write(&mut [0u8; 1408])
        .expect("the scratch holds it");
    assert!(length > 128, "{length}");
}

/// A replay whose sender vanishes, here the leader's driver stopped, sends no end of
/// stream. The member's image of it goes at the image liveness timeout, 10 s, and
/// only then is the image said to have gone: the driver stops the member's recording
/// when it retires the image. What the replay had brought is the member's log. The
/// leader heartbeat timeout is 10 s too, so a member's election fails for want of
/// progress no later than this, and it does not ask again (ADR 0094).
#[test]
fn a_replay_whose_sender_vanished_is_said_gone_at_the_image_liveness_timeout() {
    let mut pair = pair("sender-vanishes");
    let _receiver = pair.receive_log();
    pair.leader
        .io
        .add_log(0, 0)
        .expect("the driver takes the commands")
        .expect("the log has its recording");
    let one = pair.append(b"one", 0);
    pair.member.io.add_catchup().expect("subscribed and armed");
    assert!(
        pair.leader
            .io
            .start_replay(1, 0, None)
            .expect("the replay starts")
    );
    pair.observe_both("the replay's image is told", |io| {
        io.catchup_image().expect("the image is read")
    });
    pair.observe_both("the append position reaches the leader's", |io| {
        (io.append_position() == one).then_some(())
    });

    let Pair {
        leader,
        mut member,
        mut _drivers,
        _dir,
        ..
    } = pair;
    let vanished = Instant::now();
    drop(leader);
    drop(_drivers.remove(0));
    let deadline = vanished + cavalry_proto::udp::IMAGE_LIVENESS_TIMEOUT + Duration::from_secs(5);
    let gone = loop {
        member
            .io
            .observe_catchup()
            .expect("the catch-up is observed");
        if member.io.catchup_image().expect("read").is_none() {
            break vanished.elapsed();
        }
        assert!(Instant::now() < deadline, "the image never went");
        std::thread::sleep(Duration::from_millis(5));
    };
    assert!(
        gone >= cavalry_proto::udp::IMAGE_LIVENESS_TIMEOUT - Duration::from_secs(1),
        "{gone:?}"
    );
    assert_eq!(member.io.append_position(), one);
    member
        .io
        .remove_catchup()
        .expect("the subscription is removed");
    assert_eq!(member.io.append_position(), one);
    assert_eq!(member.io.kept.len(), 1);
}

/// A recording of another session's image is not the log, though the image the
/// election asks for is the one the member noted (ADR 0093). A stranger's publication
/// on the member's `log` endpoint takes the armed recording, publishes an entry and
/// closes, and its image goes before the member looks at its log subscription. The
/// leader's log then arrives and is the image the member notes. Asked for the
/// leader's session, the module holds a recording of the stranger's session: it says
/// nothing, counts nothing, and keeps nothing when it leaves.
#[test]
fn a_recording_taken_by_an_image_that_went_is_not_the_noted_images() {
    let mut pair = pair("stale-recording");
    pair.member.io.join_log().expect("subscribed and armed");
    let mut stranger = Cavalry::connect_to(&pair.leader_driver_dir).expect("the stranger");
    let channel = pair.member.io.log_endpoint_channel.clone();
    let mut stray = stranger
        .add_exclusive_publication(&channel, LOG_STREAM_ID)
        .expect("the stray publication");
    let stray_session = stray.session_id();
    let message = entry(b"of no term");
    let mut encoded = [0u8; 256];
    let length = message.write(&mut encoded).expect("the entry is encoded");
    // The member's subscription is polled here and not by the module, which is not
    // to look at it yet: the stray image is read to its end, so that it can go.
    fn images(io: &mut ClientIo) -> usize {
        let subscription = io.log_subscription.as_mut().expect("subscribed");
        subscription.poll(|_, _| {}, 16).expect("polled");
        subscription.images().len()
    }
    wait_for("the stray entry is published", || {
        images(&mut pair.member.io);
        stray.offer(&encoded[..length]).is_ok().then_some(())
    });
    wait_for("the stray image is there", || {
        (images(&mut pair.member.io) == 1).then_some(())
    });
    stray.close().expect("the stray publication closes");
    let gone = Instant::now() + Duration::from_secs(15);
    while images(&mut pair.member.io) != 0 {
        assert!(Instant::now() < gone, "the stray image has gone");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(matches!(pair.member.io.image, LogImage::Awaited));

    let log = pair
        .leader
        .io
        .add_log(0, 0)
        .expect("the driver takes the commands")
        .expect("the log has its recording");
    assert_ne!(log.session_id, stray_session);
    pair.observe("the leader's image is noted", |io| {
        matches!(io.image, LogImage::Joined { session_id, .. } if session_id == log.session_id)
            .then_some(())
    });
    pair.observe("the stray recording is handed over", |io| {
        io.held.as_ref().map(|_| ())
    });
    let held = pair.member.io.held.as_ref().expect("the recording is held");
    assert_eq!(held.recording.session_id(), stray_session);
    assert_eq!(
        pair.member.io.log_image(log.session_id).expect("read"),
        None,
        "the recording held is of another session's image"
    );
    assert!(
        !pair
            .member
            .io
            .held
            .as_ref()
            .is_some_and(|held| held.checked)
    );
    assert_eq!(pair.member.io.append_position(), 0);
    assert_eq!(pair.member.log_recording_id(), None);
    pair.member.io.leave_log().expect("the log is left");
    assert_eq!(pair.member.io.append_position(), 0);
    assert!(pair.member.io.kept.is_empty());
}

impl Pair {
    /// Member 1 follows the leader's log of `term` from `base` through the entries
    /// `payloads`, armed before the leader's log exists, then leaves it; the leader
    /// gives the log up. Returns where the log then ends, and the member's recording.
    fn follow_and_leave(&mut self, term: i64, base: i64, payloads: &[&[u8]]) -> (i64, i64) {
        self.member.io.join_log().expect("subscribed and armed");
        let log = self
            .leader
            .io
            .add_log(term, base)
            .expect("the driver takes the commands")
            .expect("the log has its recording");
        let mut end = base;
        for payload in payloads {
            end = self.append(payload, end);
        }
        self.observe_both("the log is told", |io| {
            io.log_image(log.session_id).expect("the image is read")
        });
        self.observe_both("the member holds the term", |io| {
            (io.append_position() == end).then_some(())
        });
        let recording_id = self
            .member
            .log_recording_id()
            .expect("the recording is named");
        self.member.io.leave_log().expect("the log is left");
        self.leader.io.remove_log().expect("the log is given up");
        (end, recording_id)
    }

    /// Where the catalog of member 1's driver says a recording stopped, `None` for one
    /// that is purged.
    fn stop_of(&self, recording_id: i64) -> Option<i64> {
        CatalogReader::open(&self.member_log_dir)
            .expect("the catalog opens")
            .stop_position(recording_id)
            .expect("the slot is read")
    }
}

/// A member truncates its log over a driver (ADR 0095). It holds term 0 in one
/// recording, two entries to 192, and term 1 in another, two more to 384, with both
/// terms in its record. Truncated inside the second recording, to 288, that
/// recording is cut there and the record keeps both terms. Truncated again to where
/// the second recording begins, 192, that recording holds nothing and is forgotten;
/// term 1, whose base is not above 192, stays in the record. Truncated to 96, inside
/// the first recording, the first is cut. Each time the log ends at the position.
#[test]
fn a_member_truncates_its_log_recording_by_recording() {
    let mut pair = pair("truncate");
    let (two, first) = pair.follow_and_leave(0, 0, &[b"one", b"two"]);
    let (four, second) = pair.follow_and_leave(1, two, &[b"three", b"four"]);
    let one = two / 2;
    let three = two + (four - two) / 2;
    pair.member
        .io
        .record_term(0, 0)
        .expect("term 0 is recorded");
    pair.member
        .io
        .record_term(1, two)
        .expect("term 1 is recorded");
    assert_eq!(pair.member.io.append_position(), four);

    assert_eq!(
        pair.member.io.truncate_log(three).expect("truncated"),
        Some(1)
    );
    assert_eq!(pair.member.io.append_position(), three);
    assert_eq!(pair.stop_of(second), Some(three));
    assert_eq!(
        pair.member.io.kept.iter().copied().collect::<Vec<_>>(),
        vec![
            Kept {
                recording_id: first,
                start: 0,
                stop: two,
            },
            Kept {
                recording_id: second,
                start: two,
                stop: three,
            },
        ]
    );
    // Asked again, as a leader's next new leadership term would: nothing changes.
    assert_eq!(
        pair.member.io.truncate_log(three).expect("truncated"),
        Some(1)
    );
    assert_eq!(pair.member.io.kept.len(), 2);

    assert_eq!(
        pair.member.io.truncate_log(two).expect("truncated"),
        Some(1),
        "a term based at the position stays in the record"
    );
    assert_eq!(pair.member.io.append_position(), two);
    assert_eq!(pair.stop_of(second), Some(two));
    assert_eq!(
        pair.member.io.kept.iter().copied().collect::<Vec<_>>(),
        vec![Kept {
            recording_id: first,
            start: 0,
            stop: two,
        }]
    );

    assert_eq!(
        pair.member.io.truncate_log(one).expect("truncated"),
        Some(0),
        "term 1's base is above the position"
    );
    assert_eq!(pair.member.io.append_position(), one);
    assert_eq!(pair.stop_of(first), Some(one));
    assert_eq!(
        pair.member.io.kept.iter().copied().collect::<Vec<_>>(),
        vec![Kept {
            recording_id: first,
            start: 0,
            stop: one,
        }]
    );
}

/// A truncation below where a recording begins purges it: the member holds two
/// recordings and truncates inside the first. The second is gone from the driver's
/// catalog and from the table, the first is cut, term 1 leaves the record, and a
/// replay of this member's own from the position finds nothing. Asked again,
/// nothing changes.
#[test]
fn a_truncation_below_a_recording_purges_it() {
    let mut pair = pair("truncate-purge");
    let (two, first) = pair.follow_and_leave(0, 0, &[b"one", b"two"]);
    let (_four, second) = pair.follow_and_leave(1, two, &[b"three", b"four"]);
    let one = two / 2;
    pair.member
        .io
        .record_term(0, 0)
        .expect("term 0 is recorded");
    pair.member
        .io
        .record_term(1, two)
        .expect("term 1 is recorded");

    assert_eq!(
        pair.member.io.truncate_log(one).expect("truncated"),
        Some(0)
    );
    assert_eq!(pair.member.io.append_position(), one);
    assert_eq!(pair.stop_of(first), Some(one));
    assert_eq!(pair.stop_of(second), None, "purged");
    assert_eq!(
        pair.member.io.kept.iter().copied().collect::<Vec<_>>(),
        vec![Kept {
            recording_id: first,
            start: 0,
            stop: one,
        }]
    );
    assert_eq!(pair.member.io.recording_at(one), None);
    assert_eq!(
        pair.member.io.truncate_log(one).expect("truncated"),
        Some(0)
    );
    assert_eq!(pair.member.io.kept.len(), 1);
}

/// A member that still holds a recording is not out of the log, and a truncation
/// asked of it does nothing and says a step is under way.
#[test]
fn a_truncation_asked_of_a_member_in_the_log_does_nothing() {
    let mut pair = pair("truncate-in-log");
    pair.member.io.join_log().expect("subscribed and armed");
    let log = pair
        .leader
        .io
        .add_log(0, 0)
        .expect("the driver takes the commands")
        .expect("the log has its recording");
    let one = pair.append(b"one", 0);
    pair.observe_both("the log is told", |io| {
        io.log_image(log.session_id).expect("the image is read")
    });
    pair.observe_both("the member holds the entry", |io| {
        (io.append_position() == one).then_some(())
    });
    assert_eq!(pair.member.io.truncate_log(0).expect("asked"), None);
    assert_eq!(pair.member.io.append_position(), one);
}

/// After a catch-up a member keeps two recordings that overlap, the replay's and the
/// live one (ADR 0094). A truncation to a position both hold cuts both there, so
/// that neither brings a replay of this member's own what its log no longer holds.
#[test]
fn a_truncation_cuts_both_of_two_recordings_that_overlap() {
    let mut pair = pair("truncate-overlap");
    let _receiver = pair.receive_log();
    let log = pair
        .leader
        .io
        .add_log(0, 0)
        .expect("the driver takes the commands")
        .expect("the log has its recording");
    let one = pair.append(b"one", 0);
    pair.member.io.add_catchup().expect("subscribed and armed");
    assert!(
        pair.leader
            .io
            .start_replay(1, 0, None)
            .expect("the replay starts")
    );
    pair.observe_both("the replay's image is told", |io| {
        io.catchup_image().expect("the image is read")
    });
    pair.observe_both("the append position reaches the leader's", |io| {
        (io.append_position() == one).then_some(())
    });
    // The live add, and two more entries that both recordings hold.
    pair.member.io.join_log().expect("subscribed and armed");
    let two = pair.append(b"two", one);
    let three = pair.append(b"three", two);
    wait_for("the replay brings both entries", || {
        pair.member
            .io
            .observe_catchup()
            .expect("the catch-up is observed");
        (pair.member.io.append_position() == three).then_some(())
    });
    let live_start = pair.observe_both("the live recording's start is told", |io| {
        io.log_image(log.session_id).expect("the image is read")
    });
    assert!(live_start < two, "{live_start}");
    pair.observe_both("the live recording holds both entries", |io| {
        io.held
            .as_ref()
            .filter(|held| held.recording.synced_position() >= three)
            .map(|_| ())
    });
    pair.leader
        .io
        .stop_replay(1)
        .expect("the replay is stopped");
    pair.observe_both("the replay's recording has stopped", |io| {
        io.catchup_held
            .as_ref()
            .and_then(|held| held.recording.stop_position())
    });
    pair.member.io.leave_log().expect("the log is left");
    pair.member
        .io
        .remove_catchup()
        .expect("the catch-up subscription is removed");
    let kept: Vec<_> = pair.member.io.kept.iter().copied().collect();
    assert_eq!(kept.len(), 2, "{kept:?}");
    assert!(kept.iter().all(|kept| kept.stop >= three), "{kept:?}");
    pair.member
        .io
        .record_term(0, 0)
        .expect("term 0 is recorded");

    assert_eq!(
        pair.member.io.truncate_log(two).expect("truncated"),
        Some(0)
    );
    assert_eq!(pair.member.io.append_position(), two);
    let cut: Vec<_> = pair.member.io.kept.iter().copied().collect();
    assert_eq!(
        cut.iter()
            .map(|kept| (kept.start, kept.stop))
            .collect::<Vec<_>>(),
        vec![(0, two), (live_start, two)]
    );
    for kept in cut {
        assert_eq!(pair.stop_of(kept.recording_id), Some(two));
    }
    assert_eq!(pair.member.io.recording_at(two), None);
}

/// The mark of the last sync asked is one recording's. A member follows term 0 to
/// 192, and its last request was for the position written then, 192. It truncates
/// to 96 and follows term 1 from there: the new recording will write 192 again with
/// its first entry. Taken in hand, the recording starts with no mark, so that sync
/// is asked; with the mark left at 192 it was skipped, and the entry counted toward
/// a commit only at the recorder's own sync, up to a second later.
#[test]
fn a_recording_taken_in_hand_starts_with_no_sync_asked() {
    let mut pair = pair("sync-mark");
    let (two, _) = pair.follow_and_leave(0, 0, &[b"one", b"two"]);
    let one = two / 2;
    pair.member
        .io
        .record_term(0, 0)
        .expect("term 0 is recorded");
    assert_eq!(pair.member.io.sync_asked_at, two);
    assert_eq!(
        pair.member.io.truncate_log(one).expect("truncated"),
        Some(0)
    );
    assert_eq!(pair.member.io.sync_asked_at, two, "the mark is the old one");

    pair.member.io.join_log().expect("subscribed and armed");
    let log = pair
        .leader
        .io
        .add_log(1, one)
        .expect("the driver takes the commands")
        .expect("the log has its recording");
    pair.observe("the recording is taken in hand", |io| {
        io.held.as_ref().map(|_| ())
    });
    assert_eq!(pair.member.io.sync_asked_at, NO_SYNC_ASKED);

    assert_eq!(pair.append(b"two", one), two);
    pair.observe_both("the log is told", |io| {
        io.log_image(log.session_id).expect("the image is read")
    });
    pair.observe_both("the member holds the entry", |io| {
        (io.append_position() == two).then_some(())
    });
    assert_eq!(pair.member.io.sync_asked_at, two);
}
