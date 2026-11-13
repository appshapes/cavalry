//! The module's own tests: what `ConsensusModule` cannot be asked for from outside.
//! A member leads a term whose base is not position 0, gives its log up, or replays
//! its recordings to a member that is behind only after an election. The tests here
//! wait for none: they drive the module's [`Io`] itself, over a driver, each call
//! where the test puts it (ADR 0091).

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
    leader_log_dir: PathBuf,
    member_driver_dir: PathBuf,
    member_log_dir: PathBuf,
    /// The members string both were started with.
    members: String,
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
        leader_log_dir: dir.0.join("log-0"),
        member_driver_dir: dir.0.join("driver-1"),
        member_log_dir: dir.0.join("log-1"),
        members,
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

    /// Runs member 1's observation of its log subscription until `done` says so, as
    /// a duty cycle does: the observation, then `done` where the core's step is,
    /// then the record of the log's recordings for a recording the step was told of
    /// (ADR 0108).
    fn observe<T>(&mut self, what: &str, mut done: impl FnMut(&mut ClientIo) -> Option<T>) -> T {
        wait_for(what, || {
            self.member.io.observe_log().expect("the log is observed");
            let done = done(&mut self.member.io);
            self.member
                .io
                .list_counted()
                .expect("the record of the log's recordings is written");
            done
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
            let done = done(&mut self.member.io);
            self.member
                .io
                .list_counted()
                .expect("the record of the log's recordings is written");
            done
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
    // The duty cycle that told the election ends with the record of the log's
    // recordings, and the recording counts from the next (ADR 0108).
    assert_eq!(pair.member.io.append_position(), 0, "told, not yet listed");
    pair.member
        .io
        .list_counted()
        .expect("the record of the log's recordings is written");
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
    // The driver goes first, and the leader's client after it. A client that closes
    // while its driver runs has its replay ended by that driver, which sends an end
    // of stream: the member's image would then go within a heartbeat or two, and not
    // at the liveness timeout, whenever the driver ran a duty cycle between the two
    // drops, as it did once in three runs on a loaded runner.
    drop(_drivers.remove(0));
    let vanished = Instant::now();
    drop(leader);
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

/// A purge whose answer was never read is done (ADR 0095). The member's client purges
/// the second of its two recordings, as a truncation before this one would have, and
/// the table still names it. A truncation below it is answered "unknown recording"
/// for that purge, takes it as done, forgets the recording and cuts the first.
#[test]
fn a_truncation_takes_a_recording_already_purged_as_purged() {
    let mut pair = pair("truncate-purged");
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
    pair.member
        .io
        .client
        .purge_recording(second)
        .expect("the driver purges the recording");
    assert_eq!(pair.stop_of(second), None, "purged");
    assert_eq!(pair.member.io.kept.len(), 2, "the table still names it");

    assert_eq!(
        pair.member.io.truncate_log(one).expect("truncated"),
        Some(0)
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

/// A member drops a term its leader's record lacks from its record, over a driver
/// (ADR 0096). It holds term 0 to 192 and has recorded term 1 at 192, where its log
/// ends, with no byte of it. A truncation to 192 leaves term 1, whose base is not
/// above the position. `drop_terms_above(0)` deletes it from the `terms` file, by
/// term, and returns term 0; asked again it changes nothing. The file read again
/// from the member's log directory holds term 0 and a term recorded after the drop
/// at the same base.
#[test]
fn a_member_drops_a_term_from_its_record_by_term_where_a_truncation_leaves_it() {
    let mut pair = pair("drop-terms");
    let (two, _) = pair.follow_and_leave(0, 0, &[b"one", b"two"]);
    pair.member
        .io
        .record_term(0, 0)
        .expect("term 0 is recorded");
    pair.member
        .io
        .record_term(1, two)
        .expect("term 1 is recorded");
    assert_eq!(pair.member.io.term_at_or_below(1), 1);
    assert_eq!(pair.member.io.term_at_or_below(-1), -1);
    assert_eq!(
        pair.member.io.truncate_log(two).expect("truncated"),
        Some(1),
        "a term based at the position stays in the record"
    );

    assert_eq!(pair.member.io.drop_terms_above(0).expect("dropped"), 0);
    assert_eq!(pair.member.io.log_leadership_term_id(), 0);
    assert_eq!(pair.member.io.term_at_or_below(1), 0);
    assert_eq!(pair.member.io.terms_after(0), [None, None]);
    assert_eq!(pair.member.io.append_position(), two);
    assert_eq!(
        pair.member.io.drop_terms_above(0).expect("nothing to drop"),
        0
    );

    pair.member
        .io
        .record_term(3, two)
        .expect("a term after the dropped one is recorded");
    let file = TermsFile::open(&pair.member_log_dir).expect("the terms file opens");
    assert_eq!(
        file.terms()
            .map(|entry| (entry.leadership_term_id, entry.term_base_log_position))
            .collect::<Vec<_>>(),
        vec![(0, 0), (3, two)]
    );
}

/// The member's file of its log's recordings as a service reads it: the live
/// recording and where it begins, and the recordings below it.
fn log_recordings_of(module: &ConsensusModule) -> (i64, i64, Vec<(i64, i64, i64)>) {
    let bytes = std::fs::read(&module.io.recordings_path).expect("the member's file");
    let file = cavalry_proto::cluster::LogRecordings::read(&bytes, module.io.member_id)
        .expect("the file passes its checks");
    (
        file.live_recording_id(),
        file.live_start_position(),
        file.entries()
            .map(|entry| {
                (
                    entry.recording_id,
                    entry.start_position,
                    entry.stop_position,
                )
            })
            .collect(),
    )
}

/// What the file of a member's log's recordings lists (`spec/cluster.md`, "The log's
/// recordings"; ADR 0103). In a catch-up, once the live recording is the member's
/// log: the replay's recording, which is still being made, from where it begins to
/// what it has synced, at or past where the live recording begins. After the member
/// left that log, for the recording it names next, which begins where its log ends:
/// the two recordings it stopped, which overlap, in the order they begin. And the
/// leader's, in the next term it leads: its recording of the term it gave up.
#[test]
fn the_file_of_recordings_lists_a_catch_ups_recording_and_then_what_the_member_kept() {
    let mut pair = pair("recordings-file");
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
    pair.observe_both("the replay's image is told", |io| {
        io.catchup_image().expect("the image is read")
    });
    pair.observe_both("the append position reaches the leader's", |io| {
        (io.append_position() == two).then_some(())
    });
    assert_eq!(pair.member.io.live_recording(), None);
    pair.member.io.join_log().expect("subscribed and armed");
    let live_start = pair.observe_both("the live recording's start is told", |io| {
        io.log_image(log.session_id).expect("the image is read")
    });
    let three = pair.append(b"three", two);
    pair.observe_both("the append position reaches the third entry", |io| {
        (io.append_position() == three).then_some(())
    });

    // The member names its live recording: the file it writes first.
    let (live_id, start) = pair
        .member
        .io
        .live_recording()
        .expect("the live recording is the member's log now");
    assert_eq!(start, live_start);
    let replay = pair
        .member
        .io
        .catchup_held
        .as_ref()
        .expect("the replay's recording is held");
    let replay_id = replay.recording.recording_id();
    let synced = replay.recording.synced_position();
    assert!(synced >= live_start, "{synced} {live_start}");
    pair.member
        .io
        .write_log_recordings(live_id, start)
        .expect("the file is written");
    let (live, begins, below) = log_recordings_of(&pair.member);
    assert_eq!((live, begins), (live_id, live_start));
    assert_eq!(below.len(), 1);
    let (id, from, to) = below[0];
    assert_eq!((id, from), (replay_id, 0));
    assert!((synced..=three).contains(&to), "{to} {synced} {three}");
    assert!(
        !pair.member.io.recordings_tmp_path.exists(),
        "no file is left under the other name"
    );

    // The member leaves the log. The file it writes for the recording it names next,
    // one that begins where its log ends: both recordings it stopped.
    pair.leader
        .io
        .stop_replay(1)
        .expect("the replay is stopped");
    pair.member
        .io
        .remove_catchup()
        .expect("the catch-up subscription is removed");
    let replay_stop = pair.member.io.stopped_at;
    pair.member.io.leave_log().expect("the log is left");
    assert_eq!(pair.member.io.live_recording(), None);
    assert_eq!(pair.member.io.append_position(), three);
    let next_id = live_id + 100;
    pair.member
        .io
        .write_log_recordings(next_id, three)
        .expect("the file is written");
    assert_eq!(
        log_recordings_of(&pair.member),
        (
            next_id,
            three,
            vec![(replay_id, 0, replay_stop), (live_id, live_start, three)]
        )
    );

    // The leader's own file: the recording of the term it gave up, below its
    // recording of the term it leads next.
    pair.leader.io.remove_log().expect("the log is given up");
    pair.leader
        .io
        .add_log(1, three)
        .expect("the driver takes the commands")
        .expect("the log has its recording");
    let (leads_id, base) = pair
        .leader
        .io
        .live_recording()
        .expect("the leader's recording of its log");
    assert_eq!(base, three);
    pair.leader
        .io
        .write_log_recordings(leads_id, base)
        .expect("the file is written");
    let (live, begins, below) = log_recordings_of(&pair.leader);
    assert_eq!((live, begins), (leads_id, three));
    assert_eq!(
        below
            .iter()
            .map(|&(_, from, to)| (from, to))
            .collect::<Vec<_>>(),
        vec![(0, three)]
    );
}

/// A truncation waits until `cluster.log_recording_id` holds −1 (ADR 0103): a
/// service takes the member's commit position as its bound while the counter names
/// a recording, so no recording of the log is cut before the duty cycle that left
/// the log has stored −1. Asked while the counter still names the recording the
/// member left, the truncation does nothing and says a step is under way; asked
/// after, it cuts.
#[test]
fn a_truncation_waits_until_the_counter_names_no_recording() {
    let mut pair = pair("truncate-named");
    let (two, first) = pair.follow_and_leave(0, 0, &[b"one", b"two"]);
    let one = two / 2;
    pair.member
        .io
        .record_term(0, 0)
        .expect("term 0 is recorded");

    // The member has left the log, and the cycle that stores −1 has not run.
    pair.member.io.named = first;
    assert_eq!(pair.member.io.truncate_log(one).expect("asked"), None);
    assert_eq!(pair.member.io.append_position(), two);
    assert_eq!(pair.stop_of(first), Some(two), "nothing was cut");
    assert_eq!(pair.member.io.kept.len(), 1);

    pair.member.io.named = NO_RECORDING;
    assert_eq!(
        pair.member.io.truncate_log(one).expect("truncated"),
        Some(0)
    );
    assert_eq!(pair.member.io.append_position(), one);
    assert_eq!(pair.stop_of(first), Some(one));
}

/// A member that closes stores −1 in `cluster.log_recording_id` before it removes its
/// file of the log's recordings (`spec/cluster.md`, "The log's recordings"; ADR 0103).
/// A service that finds no file loads the counter again, and must not find there the
/// id of the recording the file was for. Closing the member's client would not do it:
/// the driver removes a client's counters when it reads the command, some time after
/// the close has returned, and the store is done when it returns. So once the store
/// is made, and before the client is closed, the counter holds −1 and the file is
/// there; when the close returns the file is gone.
#[test]
fn a_member_that_closes_names_no_recording_before_it_removes_its_file() {
    let Leader {
        mut module,
        log,
        catchup,
        _follower,
        _driver,
        _dir,
    } = leader("close-order");
    // The member names recording 7 from position 0: the file first, then the id.
    module
        .io
        .write_log_recordings(7, 0)
        .expect("the file is written");
    module.counters.log_recording_id.store(7);
    let file = module.io.recordings_path.clone();
    assert!(file.exists());
    let reader = cavalry_shm::counters::CountersReader::open(&_dir.0.join("driver"))
        .expect("the counters file opens");
    let key = module.io.member_id.to_le_bytes();
    let named = || {
        (0..reader.capacity()).find_map(|id| match reader.record(id) {
            Ok(Some(record))
                if record.type_id() == CLUSTER_LOG_RECORDING_ID.0
                    && record.key().starts_with(&key) =>
            {
                Some(reader.read_acquire(id).expect("the counter is read"))
            }
            _ => None,
        })
    };
    assert_eq!(named(), Some(7));

    let mut seen = false;
    module
        .close_observed(|| {
            assert_eq!(named(), Some(NO_RECORDING), "the counter names a recording");
            assert!(file.exists(), "the file is removed only after the store");
            seen = true;
        })
        .expect("the member closes");
    assert!(seen);
    assert!(!file.exists(), "the member removed its file when it closed");
    drop((log, catchup));
}

/// A list of recordings that the module's own writer refuses is the module's fault,
/// and the member says so: what the check found, that it is a fault to report, and
/// none of the advice a reader of the file is given, since no other process wrote
/// the list. A file the operating system would not write keeps its own remedy.
#[test]
fn a_list_the_writer_refuses_is_said_to_be_the_modules_fault_and_not_the_directorys() {
    let refused = RaftError::LogRecordings {
        path: PathBuf::from("/driver/log-recordings-2"),
        source: std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            RefusedList(LogRecordingsError::Entry { index: 3 }),
        ),
    };
    let text = refused.to_string();
    assert!(
        text.starts_with(
            "member stopped (log-recordings): /driver/log-recordings-2 could not be written: \
             the list the member made of its log's recordings is one the file's format \
             refuses (recordings-entry): entry 3 has an id below 0,"
        ),
        "{text}"
    );
    assert!(
        text.contains(
            "This is a fault of the consensus module and not of the directory: report it \
             with this message"
        ),
        "{text}"
    );
    for absent in [
        "whatever else writes in the driver directory",
        "open the log again",
        "Make the driver directory writable",
    ] {
        assert!(!text.contains(absent), "{absent}: {text}");
    }

    let unwritable = RaftError::LogRecordings {
        path: PathBuf::from("/driver/log-recordings-2.tmp"),
        source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
    };
    let text = unwritable.to_string();
    assert!(
        text.contains("Make the driver directory writable by the member's user"),
        "{text}"
    );
    assert!(!text.contains("fault of the consensus module"), "{text}");
}

/// A recording as a record of the log's recordings names it.
fn listed(recording_id: i64, start_position: i64) -> ListedRecording {
    ListedRecording {
        recording_id,
        start_position,
    }
}

/// A stopped recording of the log as the catalog holds it.
fn stopped(start: i64, stop: i64) -> Cataloged {
    Cataloged::Held {
        start,
        stream_id: LOG_STREAM_ID,
        stop: Some(stop),
    }
}

/// What a start made of a record of the log's recordings: the table of recordings
/// and where the log ends, or its refusal.
type KeptLog = Result<(Vec<Kept>, i64), RaftError>;

/// A case of a refusal: what it is, the catalog, the recording the refusal names,
/// and what the catalog holds in its place.
type Refusal = (&'static str, Vec<(i64, Cataloged)>, i64, ListedFound);

/// [`kept_log`] over a catalog given as a table, a recording id the table lacks
/// being one the catalog does not hold. Returns what the start made of it, and the
/// recordings it stopped, each of which is given `live_stop` as its stop.
fn read_back(
    floor: i64,
    list: &[ListedRecording],
    catalog: &[(i64, Cataloged)],
    live_stop: i64,
) -> (KeptLog, Vec<i64>) {
    let mut stops = Vec::new();
    let result = kept_log(
        floor,
        list,
        |recording_id| {
            Ok(catalog
                .iter()
                .find(|(id, _)| *id == recording_id)
                .map_or(Cataloged::Missing, |(_, found)| *found))
        },
        |recording_id| {
            stops.push(recording_id);
            Ok(live_stop)
        },
    );
    (
        result.map(|(kept, end)| (kept.into_iter().collect(), end)),
        stops,
    )
}

/// A member's start reads its log back from the recordings its record names, and
/// from no other (ADR 0108). The record names one recording to 96, a catch-up's
/// replay from 96 and the live recording the catch-up joined, which begins inside
/// the replay's and was still being made. The catalog also holds recording 1, which
/// the member made and never counted: it begins at 0 and runs to 512, bytes of a
/// history the cluster cut, over the end of the member's log. The log read back is
/// the three the record names, in order, to where the last one stopped, and only
/// the one still being made is stopped. A recording that holds nothing carries the
/// log nowhere and is left out of the table.
#[test]
fn a_start_reads_the_log_back_from_the_recordings_its_record_names() {
    let list = [listed(0, 0), listed(2, 96), listed(3, 192)];
    let live = Cataloged::Held {
        start: 192,
        stream_id: LOG_STREAM_ID,
        stop: None,
    };
    let catalog = [
        (0, stopped(0, 96)),
        (1, stopped(0, 512)),
        (2, stopped(96, 256)),
        (3, live),
    ];
    let (read, stops) = read_back(0, &list, &catalog, 288);
    let (kept, end) = read.expect("the log is read back");
    assert_eq!(end, 288);
    assert_eq!(
        kept,
        vec![
            Kept {
                recording_id: 0,
                start: 0,
                stop: 96
            },
            Kept {
                recording_id: 2,
                start: 96,
                stop: 256
            },
            Kept {
                recording_id: 3,
                start: 192,
                stop: 288
            },
        ]
    );
    assert_eq!(stops, vec![3], "only the recording still being made");

    // The live recording stopped where it began, and one the member led from 256
    // holds nothing either: the log ends where the replay's recording stopped.
    let list = [listed(0, 0), listed(2, 96), listed(3, 192), listed(4, 256)];
    let catalog = [
        (0, stopped(0, 96)),
        (2, stopped(96, 256)),
        (3, stopped(192, 192)),
        (4, stopped(256, 256)),
    ];
    let (read, stops) = read_back(0, &list, &catalog, 0);
    let (kept, end) = read.expect("the log is read back");
    assert_eq!(end, 256);
    assert_eq!(
        kept.iter()
            .map(|kept| kept.recording_id)
            .collect::<Vec<_>>(),
        vec![0, 2]
    );
    assert!(stops.is_empty());
}

/// A truncation purges a member's recordings from the last one back and writes the
/// record after them, so a member that stopped inside one finds the record's last
/// recordings gone from the catalog, and its log ends where the ones that are left
/// end (ADR 0108). With every recording it names purged, the log ends at the
/// record's floor, 0. The log ends at the furthest end of the recordings, which is
/// not the last one's when that one ends inside the one before it.
#[test]
fn a_start_ends_the_log_where_a_truncation_cut_short_left_its_recordings() {
    let list = [listed(4, 0), listed(5, 96), listed(6, 192)];
    let whole = [
        (4, stopped(0, 96)),
        (5, stopped(96, 192)),
        (6, stopped(192, 288)),
    ];
    for (held, end, kept_ids) in [
        (3, 288, vec![4, 5, 6]),
        (2, 192, vec![4, 5]),
        (1, 96, vec![4]),
        (0, 0, vec![]),
    ] {
        let (read, stops) = read_back(0, &list, &whole[..held], 0);
        let (kept, read_end) = read.expect("the log is read back");
        assert_eq!(read_end, end, "{held} recordings held");
        assert_eq!(
            kept.iter()
                .map(|kept| kept.recording_id)
                .collect::<Vec<_>>(),
            kept_ids
        );
        assert!(stops.is_empty());
    }
    let (read, _) = read_back(0, &[], &[], 0);
    assert_eq!(read.expect("a new member's record"), (vec![], 0));
    // A replay's recording to 288 and the live one a catch-up joined at 192, cut
    // to 256 by a truncation that stopped before it cut the replay's: the log ends
    // at 288, the furthest end, and not at the last recording's.
    let overlap = [listed(4, 0), listed(5, 96), listed(6, 192)];
    let catalog = [
        (4, stopped(0, 96)),
        (5, stopped(96, 288)),
        (6, stopped(192, 256)),
    ];
    let (read, _) = read_back(0, &overlap, &catalog, 0);
    assert_eq!(read.expect("the log is read back").1, 288);
}

/// Anything else the catalog holds in place of a recording the record names
/// refuses the start, and nothing is stopped first (ADR 0108): the log read back
/// would be shorter than the one the member reported, and a member with a log
/// behind its own is a voter for a candidate that lacks a committed entry. Each
/// refusal names the recording and says what to do.
#[test]
fn a_start_is_refused_when_the_catalog_does_not_hold_the_log_the_record_names() {
    let list = [listed(0, 0), listed(2, 96), listed(3, 192)];
    let live = Cataloged::Held {
        start: 0,
        stream_id: LOG_STREAM_ID,
        stop: None,
    };
    let other_stream = Cataloged::Held {
        start: 96,
        stream_id: LOG_STREAM_ID + 1,
        stop: Some(192),
    };
    let cases: [Refusal; 5] = [
        (
            "a recording missing before one that is held",
            vec![(0, live), (3, stopped(192, 288))],
            2,
            ListedFound::Missing,
        ),
        (
            "a recording the driver found damaged",
            vec![(0, live), (2, Cataloged::Invalid), (3, stopped(192, 288))],
            2,
            ListedFound::Invalid,
        ),
        (
            "a recording that begins elsewhere",
            vec![(0, live), (2, stopped(128, 192)), (3, stopped(192, 288))],
            2,
            ListedFound::Other {
                start_position: 128,
                stream_id: LOG_STREAM_ID,
            },
        ),
        (
            "a recording of another stream",
            vec![(0, live), (2, other_stream), (3, stopped(192, 288))],
            2,
            ListedFound::Other {
                start_position: 96,
                stream_id: LOG_STREAM_ID + 1,
            },
        ),
        (
            "a recording that begins past where the ones before it end",
            vec![
                (0, stopped(0, 96)),
                (2, stopped(96, 160)),
                (3, stopped(192, 288)),
            ],
            3,
            ListedFound::Gap { end_position: 160 },
        ),
    ];
    for (what, catalog, recording, expected) in cases {
        let (read, stops) = read_back(0, &list, &catalog, 96);
        let refused = read.expect_err(what);
        assert!(
            matches!(
                &refused,
                RaftError::ListedRecording { recording_id, found, .. }
                    if *recording_id == recording && *found == expected
            ),
            "{what}: {refused}"
        );
        assert_eq!(refused.reason(), "listed-recording");
        assert!(stops.is_empty(), "{what}: stopped {stops:?}");
        let text = refused.to_string();
        assert!(
            text.contains(&format!("names recording {recording} as part of its log")),
            "{text}"
        );
        assert!(
            text.contains("Do not start this member alone on a new log directory"),
            "{text}"
        );
        assert!(
            text.contains("docs/reference/durable-log.md#log-recordings"),
            "{text}"
        );
    }
}

/// A member that has forgotten a recording of its log does not start on it (ADR
/// 0108): a start replays the log from position 0, and this version takes no
/// snapshot to begin from instead. A record whose floor is above 0 is refused
/// before the catalog is read, whether it names recordings or none. So is a record
/// that names more recordings that hold bytes than the table keeps, 1,024, which
/// the start would have to forget: before anything is stopped when the stopped
/// ones alone are too many, and after the stop when a recording that was still
/// being made is the one too many. A record of 1,026 names of which two hold
/// nothing is a log in 1,024 recordings, and starts.
#[test]
fn a_start_is_refused_on_a_log_the_member_forgot_a_recording_of() {
    let refused = |read: KeptLog, floor: i64, recordings: usize| {
        let refused = read.expect_err("a log that does not begin at 0, or in too many recordings");
        assert!(
            matches!(
                &refused,
                RaftError::LogForgotten { floor_position, recordings: named }
                    if *floor_position == floor && *named == recordings
            ),
            "{refused}"
        );
        assert_eq!(refused.reason(), "log-forgotten");
        let text = refused.to_string();
        assert!(
            text.contains("Do not start this member alone on a new log directory"),
            "{text}"
        );
        assert!(
            text.contains("docs/reference/durable-log.md#log-recordings"),
            "{text}"
        );
        text
    };
    // A floor above 0, over three recordings and over none.
    let list = [listed(4, 320), listed(5, 416), listed(6, 512)];
    let catalog = [
        (4, stopped(320, 416)),
        (5, stopped(416, 512)),
        (
            6,
            Cataloged::Held {
                start: 512,
                stream_id: LOG_STREAM_ID,
                stop: None,
            },
        ),
    ];
    let (read, stops) = read_back(320, &list, &catalog, 608);
    let text = refused(read, 320, 3);
    assert!(
        text.contains("begins at position 320 and not at 0"),
        "{text}"
    );
    assert!(stops.is_empty(), "stopped {stops:?}");
    let (read, _) = read_back(256, &[], &[], 0);
    refused(read, 256, 0);

    // 1,026 names, the most a record holds, every one a stopped recording that
    // holds bytes: refused, and nothing is stopped.
    let count = i64::try_from(cavalry_log::recordings::MAX_ENTRIES).expect("a count");
    let list: Vec<ListedRecording> = (0..count).map(|id| listed(id, id * 32)).collect();
    let mut catalog: Vec<(i64, Cataloged)> = (0..count)
        .map(|id| (id, stopped(id * 32, id * 32 + 32)))
        .collect();
    let (read, stops) = read_back(0, &list, &catalog, 0);
    let text = refused(read, 0, cavalry_log::recordings::MAX_ENTRIES);
    assert!(
        text.contains("names 1026 recordings that hold a part of its log, and a member keeps 1024"),
        "{text}"
    );
    assert!(stops.is_empty(), "stopped {stops:?}");

    // 1,024 stopped ones that hold bytes, one that holds nothing, and the last
    // still being made. Stopped with bytes in it, it is the one too many.
    let empty = usize::try_from(count - 2).expect("an index");
    let last = usize::try_from(count - 1).expect("an index");
    catalog[empty].1 = stopped((count - 2) * 32, (count - 2) * 32);
    catalog[last].1 = Cataloged::Held {
        start: (count - 2) * 32,
        stream_id: LOG_STREAM_ID,
        stop: None,
    };
    let mut list = list;
    list[last].start_position = (count - 2) * 32;
    let (read, stops) = read_back(0, &list, &catalog, count * 32);
    refused(read, 0, KEPT_RECORDINGS + 1);
    assert_eq!(stops, vec![count - 1], "the recording still being made");

    // Stopped where it began, it holds nothing: the log is in 1,024 recordings,
    // the table holds them without growing, and the member starts.
    let (read, stops) = read_back(0, &list, &catalog, (count - 2) * 32);
    let (kept, end) = read.expect("the log is read back");
    assert_eq!(end, (count - 2) * 32);
    assert_eq!(kept.len(), KEPT_RECORDINGS);
    assert_eq!(kept.first().map(|kept| kept.recording_id), Some(0));
    assert_eq!(kept.last().map(|kept| kept.recording_id), Some(count - 3));
    assert_eq!(stops, vec![count - 1]);
}

/// The record of a member's log's recordings as its log directory holds it, read
/// by a reader of its own: the floor position, the sequence and the recordings.
fn record_in(log_dir: &Path, member_id: i32) -> (i64, i64, Vec<(i64, i64)>) {
    let file = RecordingsFile::open_kept(log_dir, member_id).expect("the record opens");
    (
        file.floor_position(),
        file.sequence(),
        file.entries()
            .iter()
            .map(|entry| (entry.recording_id, entry.start_position))
            .collect(),
    )
}

/// A recording is part of a member's log once the member's record names it, synced,
/// and the member reports no position it holds before (ADR 0108). A follower's
/// recording has synced an entry and the election has been told its image: the
/// append position is still 0 and the record is the one the start created. The duty
/// cycle's last step writes the record, once, and the append position is then the
/// entry's end. A cycle in which no recording joins writes nothing. A leader's own
/// recording is in its record before `add_log` hands the log over, so before the
/// leader appends.
#[test]
fn the_record_names_a_recording_before_the_member_reports_a_position_it_holds() {
    let mut pair = pair("list-first");
    pair.member.io.join_log().expect("subscribed and armed");
    let log = pair
        .leader
        .io
        .add_log(0, 0)
        .expect("the driver takes the commands")
        .expect("the log has its recording");
    assert_eq!(
        record_in(&pair.leader_log_dir, 0),
        (0, 1, vec![(log.recording_id, 0)]),
        "the leader's record names its recording before it appends"
    );
    let one = pair.append(b"one", 0);
    wait_for("the member's recording has synced the entry", || {
        pair.member.io.observe_log().expect("the log is observed");
        pair.member
            .io
            .held
            .as_ref()
            .filter(|held| held.recording.synced_position() == one)
            .map(|_| ())
    });
    assert_eq!(
        pair.member
            .io
            .log_image(log.session_id)
            .expect("the image is read"),
        Some(0)
    );
    assert_eq!(pair.member.io.append_position(), 0, "told, not yet listed");
    assert_eq!(record_in(&pair.member_log_dir, 1), (0, 0, vec![]));
    // What each has counted in `cluster.log_recordings_listed`: the list its
    // creation wrote, and for the leader the list that names its recording.
    assert_eq!(pair.member.io.list_counts.listed, 1);
    assert_eq!(pair.leader.io.list_counts.listed, 2);

    // A list the file refuses is a write that failed: counted, the recording
    // stays out of the log, and the next cycle writes again.
    pair.member.io.kept.push_back(Kept {
        recording_id: -1,
        start: 0,
        stop: 0,
    });
    let unwritten = pair
        .member
        .io
        .list_counted()
        .expect_err("a list the file refuses");
    assert_eq!(unwritten.reason(), "recordings");
    assert_eq!(pair.member.io.list_counts.unsynced, 1);
    assert_eq!(pair.member.io.list_counts.listed, 1);
    assert_eq!(pair.member.io.append_position(), 0, "still not listed");
    assert_eq!(record_in(&pair.member_log_dir, 1), (0, 0, vec![]));
    pair.member.io.kept.pop_back();

    pair.member
        .io
        .list_counted()
        .expect("the record is written");
    assert_eq!(pair.member.io.list_counts.listed, 2);
    assert_eq!(pair.member.io.list_counts.unsynced, 1);
    let recording_id = pair
        .member
        .io
        .held
        .as_ref()
        .map(|held| held.recording.recording_id())
        .expect("the recording held");
    assert_eq!(
        record_in(&pair.member_log_dir, 1),
        (0, 1, vec![(recording_id, 0)])
    );
    assert_eq!(pair.member.io.append_position(), one);
    pair.member
        .io
        .list_counted()
        .expect("nothing is to be written");
    assert_eq!(pair.member.io.recordings.sequence(), 1, "no second write");
    assert_eq!(pair.member.io.list_counts.listed, 2);

    // A table at its bound forgets its oldest recording, and counts it.
    assert_eq!(pair.member.io.list_counts.forgotten, 0);
    let held = pair.member.io.kept.len();
    for index in held..=KEPT_RECORDINGS {
        let start = i64::try_from(index).expect("a position") * 32 + 4096;
        pair.member.io.keep(Kept {
            recording_id: 5000 + i64::try_from(index).expect("an id"),
            start,
            stop: start + 32,
        });
    }
    assert_eq!(pair.member.io.kept.len(), KEPT_RECORDINGS);
    assert_eq!(pair.member.io.list_counts.forgotten, 1);
}

/// A member stops and starts again on the log directory it kept (ADR 0089, ADR
/// 0108). It followed term 0 to its first entry and left; then a log of another
/// term that begins at 0 again reached its driver, two entries long, and its
/// recording of that image was never counted: it begins below where the member's
/// log ends (ADR 0094). That recording is in the catalog, on the log's stream, from
/// 0 to past the end of the member's log, and the record does not name it. Started
/// again, the member's log is the one recording its record names and ends where it
/// did; its record of terms and its stored candidate term are the ones it had.
#[test]
fn a_member_starts_again_on_the_log_it_kept_and_takes_no_recording_it_never_counted() {
    let mut pair = pair("start-again");
    let (one, first) = pair.follow_and_leave(0, 0, &[b"one"]);
    pair.member
        .io
        .record_term(0, 0)
        .expect("term 0 is recorded");
    pair.member
        .io
        .store_candidate_term(3)
        .expect("a vote is stored");

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
    let stray = pair.observe_both("the member's recording syncs past its log's end", |io| {
        io.held
            .as_ref()
            .filter(|held| held.recording.synced_position() >= end)
            .map(|held| held.recording.recording_id())
    });
    pair.member.io.leave_log().expect("the log is left");
    assert_eq!(pair.member.io.append_position(), one);
    assert_eq!(
        pair.stop_of(stray),
        Some(end),
        "in the catalog, past the log's end"
    );
    assert_eq!(pair.start_of(stray), 0);
    assert_eq!(record_in(&pair.member_log_dir, 1), (0, 1, vec![(first, 0)]));

    let Pair {
        leader: _leader,
        member,
        member_driver_dir,
        member_log_dir,
        members,
        _drivers,
        _dir,
        ..
    } = pair;
    member.close().expect("the member closes");
    let mut member = ConsensusModule::start(&member_driver_dir, &member_log_dir, &members, 1)
        .expect("the member starts on the log it kept");
    assert_eq!(member.io.append_position(), one);
    assert_eq!(
        member.io.kept.iter().copied().collect::<Vec<_>>(),
        vec![Kept {
            recording_id: first,
            start: 0,
            stop: one,
        }]
    );
    assert_eq!(member.io.log_leadership_term_id(), 0);
    assert_eq!(member.io.candidate_term(), 3);
    assert_eq!(
        record_in(&member_log_dir, 1),
        (0, 1, vec![(first, 0)]),
        "the record was as the catalog holds the log, and was not written again"
    );
    assert_eq!(
        member.io.list_counts,
        ListCounts::default(),
        "a start that created nothing, wrote nothing and finished no truncation"
    );
}

/// A recording the record names that holds nothing is no truncation cut short (ADR
/// 0108). The member follows term 0 to two entries, then joins the log of term 1
/// where its own ends and leaves it before an entry arrives: its record names both
/// recordings, and the second begins and stops at the end of the first. Started
/// again, the member's log is the first recording; the start writes the record
/// without the one that holds nothing, which is counted as a list written, and it
/// counts no truncation finished: no recording was gone, and term 1, based at the
/// end of the log, is not above it and stays.
#[test]
fn a_start_leaves_out_a_recording_that_holds_nothing_and_counts_no_truncation() {
    let mut pair = pair("start-empty");
    let (two, first) = pair.follow_and_leave(0, 0, &[b"one", b"two"]);
    let (end, second) = pair.follow_and_leave(1, two, &[]);
    assert_eq!(end, two);
    for (term, base) in [(0, 0), (1, two)] {
        pair.member
            .io
            .record_term(term, base)
            .expect("the term is recorded");
    }
    assert_eq!(
        record_in(&pair.member_log_dir, 1),
        (0, 2, vec![(first, 0), (second, two)])
    );
    assert_eq!(pair.stop_of(second), Some(two), "it holds nothing");
    let Pair {
        leader: _leader,
        member,
        member_driver_dir,
        member_log_dir,
        members,
        _drivers,
        _dir,
        ..
    } = pair;
    member.close().expect("the member closes");

    let mut member = ConsensusModule::start(&member_driver_dir, &member_log_dir, &members, 1)
        .expect("the member starts on the log it kept");
    assert_eq!(member.io.append_position(), two);
    assert_eq!(
        member.io.kept.iter().copied().collect::<Vec<_>>(),
        vec![Kept {
            recording_id: first,
            start: 0,
            stop: two,
        }]
    );
    assert_eq!(record_in(&member_log_dir, 1), (0, 3, vec![(first, 0)]));
    assert_eq!(
        member.io.list_counts,
        ListCounts {
            listed: 1,
            ..ListCounts::default()
        },
        "a list written, and no truncation finished"
    );
    assert_eq!(member.io.log_leadership_term_id(), 1);
}

/// A member's process is lost while its driver runs on, and the recording it was
/// making of its leader's log is still being made when it starts again (ADR 0108,
/// Context). The start stops that recording by its id, reads where it stopped from
/// the catalog, and the member's log ends there: at the entry it held, since nothing
/// arrived after it. The record named the recording already and is not written
/// again.
#[test]
fn a_start_stops_a_recording_its_driver_went_on_making() {
    let mut pair = pair("start-live");
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
    let recording_id = pair
        .member
        .io
        .held
        .as_ref()
        .map(|held| held.recording.recording_id())
        .expect("the recording held");
    pair.member
        .io
        .record_term(0, 0)
        .expect("term 0 is recorded");
    assert_eq!(
        record_in(&pair.member_log_dir, 1),
        (0, 1, vec![(recording_id, 0)])
    );
    let Pair {
        leader: _leader,
        member,
        member_driver_dir,
        member_log_dir,
        members,
        _drivers,
        _dir,
        ..
    } = pair;
    // The process is lost: nothing of the module runs again, and nothing of it is
    // closed, so no stop is sent. Its driver, and the leader's log, go on.
    std::mem::forget(member);
    let catalog = CatalogReader::open(&member_log_dir).expect("the catalog opens");
    assert!(
        matches!(
            cataloged(&catalog, recording_id),
            Ok(Cataloged::Held { stop: None, .. })
        ),
        "the recording is still being made"
    );

    let mut member = ConsensusModule::start(&member_driver_dir, &member_log_dir, &members, 1)
        .expect("the member starts on the log it kept");
    assert_eq!(
        catalog
            .stop_position(recording_id)
            .expect("the slot is read"),
        Some(one),
        "the start stopped it"
    );
    assert_eq!(member.io.append_position(), one);
    assert_eq!(
        member.io.kept.iter().copied().collect::<Vec<_>>(),
        vec![Kept {
            recording_id,
            start: 0,
            stop: one,
        }]
    );
    assert_eq!(
        record_in(&member_log_dir, 1),
        (0, 1, vec![(recording_id, 0)]),
        "not written again"
    );
    assert_eq!(member.io.list_counts, ListCounts::default());
}

/// A member that stopped inside a truncation starts where its recordings end, and
/// drops the terms based above that (ADR 0095's Revisit, ADR 0108). The member
/// holds term 0 to two entries and term 1 to four, in a recording each, and has
/// recorded term 2 at the end of its log. A truncation has purged the second
/// recording and gone no further: the record still names both, and the record of
/// terms holds all three. Started again, the member's log is the first recording,
/// its record names that one alone, and of its terms the one based above the end of
/// its log is gone: term 1, based at the end, stays, as a truncation leaves it.
#[test]
fn a_member_that_stopped_inside_a_truncation_starts_where_its_recordings_end() {
    stopped_inside_a_truncation("start-cut-short", false);
}

/// The same member, stopped one step later: the truncation had written the record
/// of the log's recordings, without the recording it purged, and had not yet
/// deleted a term. The start finds the record as the catalog holds the log, writes
/// it no second time, and deletes the term based above the end of the log.
#[test]
fn a_member_that_stopped_before_a_truncation_deleted_its_terms_deletes_them() {
    stopped_inside_a_truncation("start-terms-left", true);
}

/// The two tests above: a truncation of the second of two recordings that the
/// member's stop cut short, after its `purge recording` when `record_written` is
/// not set, and after its write of the record of the log's recordings when it is.
fn stopped_inside_a_truncation(test: &str, record_written: bool) {
    let mut pair = pair(test);
    let (two, first) = pair.follow_and_leave(0, 0, &[b"one", b"two"]);
    let (four, second) = pair.follow_and_leave(1, two, &[b"three", b"four"]);
    for (term, base) in [(0, 0), (1, two), (2, four)] {
        pair.member
            .io
            .record_term(term, base)
            .expect("the term is recorded");
    }
    assert_eq!(
        record_in(&pair.member_log_dir, 1),
        (0, 2, vec![(first, 0), (second, two)])
    );
    let Pair {
        leader: _leader,
        member,
        member_driver_dir,
        member_log_dir,
        members,
        _drivers,
        _dir,
        ..
    } = pair;
    member.close().expect("the member closes");
    // The truncation's first command, and then the member stopped; or its write of
    // the record as well, and then the member stopped.
    let mut client = Cavalry::connect_to(&member_driver_dir).expect("a client");
    client
        .purge_recording(second)
        .expect("the recording is purged");
    client.close().expect("the client closes");
    if record_written {
        RecordingsFile::open_kept(&member_log_dir, 1)
            .expect("the record opens")
            .write(0, [listed(first, 0)])
            .expect("the record is written");
    }

    let mut member = ConsensusModule::start(&member_driver_dir, &member_log_dir, &members, 1)
        .expect("the member starts on the log it kept");
    assert_eq!(member.io.append_position(), two);
    assert_eq!(
        member.io.kept.iter().copied().collect::<Vec<_>>(),
        vec![Kept {
            recording_id: first,
            start: 0,
            stop: two,
        }]
    );
    assert_eq!(member.io.log_leadership_term_id(), 1);
    assert_eq!(record_in(&member_log_dir, 1), (0, 3, vec![(first, 0)]));
    // The start finished the truncation, counted once, and wrote the record only
    // where the truncation had not.
    assert_eq!(
        member.io.list_counts,
        ListCounts {
            listed: u64::from(!record_written),
            truncations_finished: 1,
            ..ListCounts::default()
        }
    );
    assert_eq!(
        TermsFile::open(&member_log_dir)
            .expect("the record of terms opens")
            .terms()
            .map(|entry| (entry.leadership_term_id, entry.term_base_log_position))
            .collect::<Vec<_>>(),
        vec![(0, 0), (1, two)]
    );
}

/// A log directory whose record of terms holds a term and that holds no record of
/// the log's recordings is one a version before ADR 0108 wrote, or one the file was
/// removed from. A record created for it now would name no recording, and the
/// member would start with no log and delete its terms. The start is refused with
/// what to do, and the directory is left as it was.
#[test]
fn a_member_whose_record_holds_a_term_and_no_record_of_its_recordings_is_refused() {
    let mut pair = pair("start-no-record");
    let (one, _first) = pair.follow_and_leave(0, 0, &[b"one"]);
    pair.member
        .io
        .record_term(0, 0)
        .expect("term 0 is recorded");
    assert_eq!(pair.member.io.append_position(), one);
    let Pair {
        leader: _leader,
        member,
        member_driver_dir,
        member_log_dir,
        members,
        _drivers,
        _dir,
        ..
    } = pair;
    member.close().expect("the member closes");
    let record = RecordingsFile::path_in(&member_log_dir);
    std::fs::remove_file(&record).expect("the record is removed");

    let refused = ConsensusModule::start(&member_driver_dir, &member_log_dir, &members, 1)
        .expect_err("a term and no record of the log's recordings");
    assert!(
        matches!(
            &refused,
            RaftError::Recordings(RecordingsError::Absent { path }) if *path == record
        ),
        "{refused}"
    );
    assert_eq!(refused.reason(), "recordings");
    let text = refused.to_string();
    assert!(
        text.contains("every member on a new log directory"),
        "{text}"
    );
    assert!(
        text.contains("Do not start this member alone on a new log directory"),
        "{text}"
    );
    assert!(
        text.contains("docs/reference/durable-log.md#log-recordings"),
        "{text}"
    );
    assert!(!record.exists(), "the refusal created a record");
    assert_eq!(
        TermsFile::open(&member_log_dir)
            .expect("the record of terms opens")
            .log_leadership_term_id(),
        0,
        "the record of terms is as it was"
    );
}

/// A log directory that holds a log and a record of terms with no term in it has
/// lost its `terms` file: removed, or replaced by an empty one. The member's votes
/// and the terms of its log were in it. A start on it would report a log of no
/// term, be told to cut it all, and vote meanwhile with no record of its votes, so
/// the start is refused with what to do.
#[test]
fn a_member_whose_log_has_lost_its_record_of_terms_is_refused() {
    let mut pair = pair("start-no-terms");
    let (one, _first) = pair.follow_and_leave(0, 0, &[b"one"]);
    pair.member
        .io
        .record_term(0, 0)
        .expect("term 0 is recorded");
    let Pair {
        leader: _leader,
        member,
        member_driver_dir,
        member_log_dir,
        members,
        _drivers,
        _dir,
        ..
    } = pair;
    member.close().expect("the member closes");
    std::fs::remove_file(TermsFile::path_in(&member_log_dir)).expect("the terms file is removed");

    // No `terms` file, and the catalog holds the member's recording of the log: the
    // start is refused before a file is made, so the directory is left as it was.
    let missing = ConsensusModule::start(&member_driver_dir, &member_log_dir, &members, 1)
        .expect_err("a log and no terms file");
    assert!(
        matches!(&missing, RaftError::TermsLost { recording_id, log_end: -1 } if *recording_id >= 0),
        "{missing}"
    );
    assert_eq!(missing.reason(), "terms-lost");
    let text = missing.to_string();
    assert!(text.contains("and no `terms` file"), "{text}");
    assert!(
        !TermsFile::path_in(&member_log_dir).exists(),
        "the refusal made no terms file"
    );

    // A `terms` file that holds no term, as one put in the place of the member's: the
    // member's record of its recordings still names its log, to where it ends.
    drop(TermsFile::open(&member_log_dir).expect("an empty terms file is made"));
    let refused = ConsensusModule::start(&member_driver_dir, &member_log_dir, &members, 1)
        .expect_err("a log and no term");
    assert!(
        matches!(&refused, RaftError::TermsLost { log_end, .. } if *log_end == one),
        "{refused}"
    );
    assert_eq!(refused.reason(), "terms-lost");
    let text = refused.to_string();
    assert!(text.contains(&format!("to position {one}")), "{text}");
    assert!(
        text.contains("Do not start this member alone on a new log directory"),
        "{text}"
    );
    assert!(
        text.contains("docs/reference/cluster.md#roles-and-the-election"),
        "{text}"
    );
    // Asked again, it is refused again: the file still holds no term.
    let again = ConsensusModule::start(&member_driver_dir, &member_log_dir, &members, 1)
        .expect_err("a log and no term");
    assert_eq!(again.reason(), "terms-lost");
}

/// A truncation that leaves no recording in the table still leaves the member
/// knowing where its log ends, and a start then refuses the log (ADR 0108). The
/// table holds three recordings that begin at 256 and above, having forgotten the
/// ones below (ADR 0091); the test puts them there, and the driver has none of
/// them, which is how a purge asked a second time finds a recording. Truncated to
/// 128, the table is empty, the log ends at 128, and the record names no recording
/// over a floor of 128: without the floor the record would read back as a log that
/// ends at 0. The member runs on. Started again, it is refused, with the position
/// its log begins at: a start replays the log from 0, which this member forgot.
/// Its directory is left as it was.
#[test]
fn a_truncation_that_empties_the_table_keeps_where_the_log_ends() {
    let pair = pair("truncate-all");
    let Pair {
        leader: _leader,
        mut member,
        member_driver_dir,
        member_log_dir,
        members,
        _drivers,
        _dir,
        ..
    } = pair;
    for (recording_id, start) in [(900, 256), (901, 320), (902, 384)] {
        member.io.keep(Kept {
            recording_id,
            start,
            stop: start + 64,
        });
    }
    member.io.stopped_at = 448;
    // The term of that log, based at 0: a log with no term is refused at a start.
    member.io.record_term(0, 0).expect("term 0 is recorded");
    let listed_before = member.io.list_counts.listed;
    assert_eq!(member.io.truncate_log(128).expect("truncated"), Some(0));
    assert!(member.io.kept.is_empty());
    assert_eq!(member.io.append_position(), 128);
    assert_eq!(record_in(&member_log_dir, 1), (128, 1, vec![]));
    assert_eq!(
        member.io.list_counts.listed,
        listed_before + 1,
        "the truncation's write of the record is counted"
    );

    member.close().expect("the member closes");
    let refused = ConsensusModule::start(&member_driver_dir, &member_log_dir, &members, 1)
        .expect_err("a log that does not begin at 0");
    assert!(
        matches!(
            &refused,
            RaftError::LogForgotten {
                floor_position: 128,
                recordings: 0
            }
        ),
        "{refused}"
    );
    let text = refused.to_string();
    assert!(
        text.contains("begins at position 128 and not at 0"),
        "{text}"
    );
    assert_eq!(record_in(&member_log_dir, 1), (128, 1, vec![]));
    let terms = TermsFile::open(&member_log_dir).expect("the record of terms");
    assert_eq!(terms.log_leadership_term_id(), 0, "no term was deleted");
}

/// A consensus message whose step faults does not take the rest of its poll with it:
/// a message taken from the stream and handed to nobody would be lost with no count
/// (invariant 4). A member of two canvasses alone. A peer's client sends it two
/// requests for a vote, and reads the member's stream beside it, so that the member's
/// next poll finds both. The first request is for a term below the candidate term the
/// member's `terms` file holds, which the test raised where the core does not see it:
/// the store the core asks for is refused, a fault. The second is for a term above it.
/// The duty cycle that reads them returns the first request's fault, and has handed
/// the second to the core all the same: the member stored its term.
#[test]
fn a_message_that_faults_leaves_the_rest_of_its_poll_to_the_core() {
    let dir = TestDir::new("fault-in-a-poll");
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
    let _driver = Driver::start(&driver_dir, &log_dir).expect("the driver starts");
    let mut module =
        ConsensusModule::start(&driver_dir, &log_dir, &members, 0).expect("the member starts");
    // Out of init, where a member reads no message, into canvass.
    wait_for("the member canvasses", || {
        module.do_work().expect("the member's duty cycle");
        (module.election_state() == ElectionState::Canvass).then_some(())
    });

    // Member 0's consensus endpoint is the second of its five.
    let channel = format!("cavalry:udp?endpoint=127.0.0.1:{}", ports[1]);
    let mut peer = Cavalry::connect_to(&driver_dir).expect("the peer's client");
    let mut requests = peer
        .add_publication(&channel, CONSENSUS_STREAM_ID)
        .expect("the peer publishes to the member");
    let mut beside = peer
        .add_subscription(&channel, CONSENSUS_STREAM_ID)
        .expect("the peer reads the member's stream");

    module
        .io
        .terms
        .set_candidate_term(9)
        .expect("the file takes a higher candidate term");
    let mut bytes = [0u8; 128];
    for candidate_term_id in [5, 12] {
        let request = Message::RequestVote {
            log_leadership_term_id: -1,
            log_position: 0,
            candidate_term_id,
            candidate_member_id: 1,
            protocol_version: 0,
        };
        let length = request.write(&mut bytes).expect("a request vote");
        wait_for("the request is taken", || {
            requests.offer(&bytes[..length]).ok()
        });
    }
    let mut arrived = 0;
    wait_for("both requests have arrived", || {
        arrived += beside.poll(|_, _| {}, 16).expect("the peer's poll");
        (arrived == 2).then_some(())
    });

    // The first cycle that reads the stream reads both, and returns the first one's
    // fault: the file refuses a candidate term below the one it holds.
    let fault = wait_for("the cycle that reads the requests", || {
        module.do_work().err()
    });
    assert!(matches!(fault, RaftError::Terms(_)), "{fault}");
    assert_eq!(
        module.io.terms.candidate_term(),
        12,
        "the second request reached the core in the cycle that returned the fault"
    );
    assert_eq!(module.messages_rejected, 0, "neither was rejected");
    // The fault was one message's: the next cycle has nothing to return.
    module.do_work().expect("the member's duty cycle");
    assert_eq!(module.election_state(), ElectionState::Canvass);
}

/// What a refusal of the leader's log publication is to the election, and what its
/// error says. A refusal that waiting mends appends nothing and is no error: the
/// leader appends the entry again. Every other one is [`RaftError::Append`], whose
/// message names the refusal, says what to do next, and names the page's section on
/// the log, where a reader of the message looks first.
#[test]
fn a_refusal_of_the_leaders_log_says_what_to_do_next_or_has_the_leader_append_again() {
    for wait in [
        Offer::BackPressured,
        Offer::NotConnected,
        Offer::AdminAction,
    ] {
        assert!(
            matches!(log_refusal(wait), Ok(false)),
            "{wait:?} is no error"
        );
    }
    for (refusal, does_next) in [
        (
            Offer::MaxPositionExceeded,
            "start the cluster again as a new cluster, every member on a new log directory",
        ),
        (
            Offer::Closed,
            "Run `cavalry doctor` on the member's driver directory",
        ),
        (
            Offer::TooLong {
                found: 2_000,
                limit: 1_408,
            },
            "report it, with this message",
        ),
    ] {
        let error = log_refusal(refusal).expect_err("a refusal that waiting does not mend");
        assert!(
            matches!(&error, RaftError::Append { refusal: found } if *found == refusal),
            "{error}"
        );
        assert_eq!(error.reason(), "append");
        let text = error.to_string();
        assert!(
            text.starts_with(&format!(
                "member stopped (append): the leader's log publication refused an entry, {}: ",
                refusal.reason()
            )),
            "{text}"
        );
        assert!(text.contains(does_next), "{text}");
        assert!(
            text.ends_with("; see docs/reference/cluster.md#the-log"),
            "{text}"
        );
    }
    let text = log_refusal(Offer::TooLong {
        found: 2_000,
        limit: 1_408,
    })
    .expect_err("too long")
    .to_string();
    assert!(
        text.contains("an entry of 2000 bytes where the publication takes 1408"),
        "{text}"
    );
}
