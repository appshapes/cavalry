//! Publications against a real driver in this process: the full case first, back-pressure
//! at the window with a subscriber that never reads, then the shared and exclusive
//! sessions, the refusals with the spec's error codes, and closing, a claim after the
//! client's close among it
//! (`spec/log-buffer.md`, "Claim, commit and abort"; `spec/client-protocol.md`,
//! "Commands" and "Error codes"; ADR 0021; CLAUDE.md, invariants 1 and 3).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use cavalry_client::{Cavalry, ClientError, Offer};
use cavalry_driver::{Driver, spec};
use cavalry_proto::client_protocol::{Command, ErrorCode, Event, IPC_CHANNEL};
use cavalry_shm::broadcast::BroadcastBuffer;
use cavalry_shm::command_ring::CommandRing;
use cavalry_shm::counters::generated::DRIVER_LOG_CREATE_FAILED;

// The one port these tests choose, for `a_driver_runs_where_files_are_limited`'s
// publication over UDP, from the one file that says how ports are taken and why, which
// is `cavalry-raft`'s: `crates/cavalry-raft/tests/ports/mod.rs`.
#[path = "../../cavalry-raft/tests/ports/mod.rs"]
mod ports;

/// A directory of its own per test, removed when the guard drops.
struct TestDir(PathBuf);

impl TestDir {
    fn new(test: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("cavalry-client-pub-{}-{test}", std::process::id()));
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

fn now_nanos() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos()),
    )
    .unwrap_or(i64::MAX)
}

/// A subscriber by hand, through the protocol, until the next commit brings
/// `add_subscription`: it adds a subscription and never polls, so its image's position
/// stays where it joined and the publisher meets the window.
fn add_idle_subscription(dir: &Path, stream_id: i32) {
    let ring = CommandRing::open(dir, true).expect("the ring opens");
    let mut producer = ring.producer(spec::DRIVER_TIMEOUT).expect("a producer");
    let broadcast = BroadcastBuffer::open(dir, false).expect("the buffer opens");
    let mut receiver = broadcast.receiver().expect("a receiver");
    let client_id = producer.next_correlation_id();
    let correlation_id = producer.next_correlation_id();
    let command = Command::AddSubscription {
        client_id,
        correlation_id,
        stream_id,
        channel: IPC_CHANNEL,
    };
    let mut buffer = vec![0u8; 256];
    let length = command.write(&mut buffer).expect("encodes");
    producer
        .write(
            command.command_type() as i32,
            &buffer[..length],
            now_nanos(),
        )
        .expect("written");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let mut ready = false;
        receiver
            .receive(|type_id, message| {
                if let Ok(Event::SubscriptionReady {
                    correlation_id: id, ..
                }) = Event::parse(type_id, message)
                    && id == correlation_id
                {
                    ready = true;
                }
            })
            .expect("receives");
        if ready {
            return;
        }
        assert!(Instant::now() < deadline, "no subscription ready");
        std::thread::sleep(Duration::from_millis(1));
    }
    // The subscriber's keepalives are not needed: the test is shorter than the liveness
    // timeout, and an image whose client is timed out would still hold its position.
}

fn wait_until(timeout: Duration, mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    done()
}

#[test]
fn a_publisher_is_back_pressured_at_the_window_and_nothing_is_lost() {
    let dir = TestDir::new("window");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("the driver starts");
    let mut client = Cavalry::connect_to(dir.path()).expect("connects");
    let mut publication = client
        .add_publication("cavalry:ipc", 1001)
        .expect("a publication");

    // No subscriber yet: not connected, and the limit holds the publisher at 0.
    assert!(!publication.is_connected());
    assert_eq!(publication.try_claim(8).unwrap_err(), Offer::NotConnected);
    assert_eq!(publication.position().expect("position"), 0);

    add_idle_subscription(dir.path(), 1001);
    assert!(
        wait_until(Duration::from_secs(5), || publication.is_connected()),
        "the driver connects the publication on the subscriber's image"
    );
    assert!(
        wait_until(Duration::from_secs(5), || publication.limit()
            >= i64::from(spec::IPC_PUBLICATION_WINDOW)),
        "the limit is the subscriber's position plus the window"
    );

    // Fill the window: 64-byte frames (32-byte header, 32-byte body) until the window
    // is full. Every refusal before that is a rotation, never a lost message.
    let body = [7u8; 32];
    let mut written: u64 = 0;
    let mut rotations = 0;
    let back_pressured = loop {
        match publication.offer(&body) {
            Ok(position) => {
                written += 1;
                assert_eq!(position, i64::try_from(written * 64).expect("fits"));
            }
            Err(Offer::AdminAction) => rotations += 1,
            Err(Offer::BackPressured) => break true,
            Err(other) => panic!("{other}"),
        }
        assert!(written < 10_000_000, "never back-pressured");
    };
    assert!(back_pressured);
    assert_eq!(
        written * 64,
        u64::from(spec::IPC_PUBLICATION_WINDOW),
        "exactly the window was written, {rotations} rotations"
    );
    assert_eq!(
        publication.offer(&body).unwrap_err(),
        Offer::BackPressured,
        "and it stays back-pressured while the subscriber does not read"
    );
    assert_eq!(publication.try_claim(32).unwrap_err(), Offer::BackPressured);
    assert_eq!(
        publication.position().expect("position"),
        i64::from(spec::IPC_PUBLICATION_WINDOW)
    );

    publication.close().expect("closes");
    client.close().expect("closes");
    driver.close().expect("the driver closes");
}

#[test]
fn claim_writes_in_place_and_too_long_is_refused_before_anything_is_reserved() {
    let dir = TestDir::new("claim");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("the driver starts");
    let mut client = Cavalry::connect_to(dir.path()).expect("connects");
    let mut publication = client
        .add_exclusive_publication("cavalry:ipc", 1002)
        .expect("a publication");
    assert!(publication.is_exclusive());
    add_idle_subscription(dir.path(), 1002);
    assert!(wait_until(Duration::from_secs(5), || publication.is_connected()));

    let max_payload = publication.max_payload_length();
    let max_message = publication.max_message_length();
    assert_eq!(max_payload, spec::IPC_MTU_LENGTH as usize - 32);
    assert!(max_message > max_payload);
    assert_eq!(
        publication.try_claim(max_payload + 1).unwrap_err(),
        Offer::TooLong {
            found: max_payload + 1,
            limit: max_payload
        }
    );
    let too_long = vec![0u8; max_message + 1];
    assert_eq!(
        publication.offer(&too_long).unwrap_err(),
        Offer::TooLong {
            found: max_message + 1,
            limit: max_message
        }
    );
    assert_eq!(
        publication.position().expect("position"),
        0,
        "nothing reserved"
    );

    let mut claim = publication.try_claim(5).expect("claims");
    claim.body().copy_from_slice(b"hello");
    let after = claim.commit();
    assert_eq!(after, 64, "a 5-byte body is one 64-byte frame");
    assert_eq!(publication.position().expect("position"), 64);

    // An offer of a message larger than one payload fragments it.
    let big = vec![1u8; max_payload * 2 + 1];
    let position = publication.offer(&big).expect("offers");
    assert!(position > 64 + i64::try_from(big.len()).expect("fits"));

    publication.close().expect("closes");
    client.close().expect("closes");
    driver.close().expect("the driver closes");
}

#[test]
fn shared_publications_share_a_session_and_exclusive_ones_do_not() {
    let dir = TestDir::new("sessions");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("the driver starts");
    let mut client = Cavalry::connect_to(dir.path()).expect("connects");
    let first = client
        .add_publication("cavalry:ipc", 1003)
        .expect("a publication");
    let second = client
        .add_publication("cavalry:ipc", 1003)
        .expect("a publication");
    assert_eq!(first.session_id(), second.session_id(), "shared");
    assert_eq!(first.registration_id(), second.registration_id());
    assert_eq!(first.stream_id(), 1003);
    assert_eq!(first.channel(), "cavalry:ipc");
    assert!(!first.is_exclusive());
    let exclusive = client
        .add_exclusive_publication("cavalry:ipc", 1003)
        .expect("a publication");
    assert_ne!(
        exclusive.session_id(),
        first.session_id(),
        "a session of its own"
    );
    assert_ne!(exclusive.registration_id(), first.registration_id());
    assert!(format!("{first:?}").contains("Publication"));
    drop(first);
    drop(second);
    drop(exclusive);
    client.close().expect("closes");
    driver.close().expect("the driver closes");
}

#[test]
fn a_refused_command_carries_the_drivers_code_and_message() {
    let dir = TestDir::new("refused");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("the driver starts");
    let mut client = Cavalry::connect_to(dir.path()).expect("connects");
    // A publication sends on a cavalry:udp channel (ADR 0041), a subscription
    // receives on one (ADR 0042) and a replay plays on one (ADR 0043): a replay of a
    // recording the catalog does not hold passes the channel check and is refused by
    // the replayer with code 21, the message naming the recording.
    let error = client
        .start_replay(0, -1, -1, 1004, "cavalry:udp?endpoint=localhost:40123")
        .expect_err("no recording 0 to replay");
    match &error {
        ClientError::Refused {
            code,
            raw_code,
            message,
        } => {
            assert_eq!(*code, Some(ErrorCode::UnknownRecording));
            assert_eq!(*raw_code, 21);
            assert!(message.contains('0'), "{message}");
        }
        other => panic!("{other}"),
    }
    assert_eq!(error.reason(), "refused");
    assert!(error.to_string().contains("error code 21"), "{error}");
    // The client is still fine after a refusal.
    let publication = client
        .add_publication("cavalry:ipc", 1004)
        .expect("a publication");
    publication.close().expect("closes");
    client.close().expect("closes");
    driver.close().expect("the driver closes");
}

#[test]
fn a_closed_publications_log_goes_and_a_closed_clients_publication_refuses() {
    let dir = TestDir::new("closed");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("the driver starts");
    let mut client = Cavalry::connect_to(dir.path()).expect("connects");
    let closing = client
        .add_publication("cavalry:ipc", 1005)
        .expect("a publication");
    let log = dir.path().join(format!(
        "publications/{}.logbuffer",
        closing.registration_id()
    ));
    assert!(log.is_file());
    closing.close().expect("closes");
    // The driver removes the log after its linger, since no subscriber holds it.
    assert!(
        wait_until(Duration::from_secs(8), || !log.is_file()),
        "the log lingered past 5 s"
    );

    // A publication still held when its client closes: open, it is not connected; once
    // the driver has gone and the client has seen it, every refusal is `closed`.
    let mut held = client
        .add_exclusive_publication("cavalry:ipc", 1006)
        .expect("a publication");
    assert!(!held.is_closed());
    assert_eq!(held.offer(b"before"), Err(Offer::NotConnected));
    driver.close().expect("the driver closes");
    assert!(wait_until(Duration::from_secs(3), || client.is_closed()));
    assert!(held.is_closed());
    assert!(!held.is_connected());
    assert_eq!(held.offer(b"after"), Err(Offer::Closed));
    let claim = held.try_claim(8).map(|_| ());
    assert_eq!(claim, Err(Offer::Closed));
    let refusal = Offer::Closed.to_string();
    assert!(refusal.contains("client is closed"), "{refusal}");
    assert!(format!("{held:?}").contains("closed: true"));

    let error = client
        .add_publication("cavalry:ipc", 1005)
        .expect_err("the client is closed");
    assert_eq!(error.reason(), "closed");
    assert!(error.to_string().contains("driver-closed"), "{error}");
}

#[test]
fn a_claim_or_an_offer_after_the_clients_close_is_closed_while_the_limit_has_room() {
    let dir = TestDir::new("closed-in-window");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("the driver starts");
    let mut client = Cavalry::connect_to(dir.path()).expect("connects");
    let mut held = client
        .add_publication("cavalry:ipc", 1007)
        .expect("a publication");
    add_idle_subscription(dir.path(), 1007);
    assert!(
        wait_until(Duration::from_secs(5), || held.limit()
            >= i64::from(spec::IPC_PUBLICATION_WINDOW)),
        "the limit is the subscriber's position plus the window"
    );
    let claim = held.try_claim(8).expect("a claim within the window");
    assert!(claim.commit() > 0);

    // The window still has room, and the driver may not have read `client close` yet,
    // but it ends the stream at the producer position it loads then: a frame reserved
    // after the close could land past that end, and no subscriber would be told of
    // it. So every claim and offer from the close on is refused before anything is
    // reserved (spec/log-buffer.md, "Closing"; spec/c-abi.md, "Lifetimes").
    client.close().expect("closes");
    assert!(held.is_closed());
    let position = held.position().expect("position");
    assert_eq!(held.try_claim(8).map(|_| ()), Err(Offer::Closed));
    assert_eq!(held.offer(b"after"), Err(Offer::Closed));
    assert_eq!(
        held.position().expect("position"),
        position,
        "nothing was reserved after the close"
    );
    drop(held);
    driver.close().expect("the driver closes");
}

/// Where the child of
/// [`a_driver_that_cannot_create_a_log_refuses_it_and_leaves_no_file`] runs its driver.
const LIMITED_DIR: &str = "CAVALRY_CLIENT_TEST_LIMITED_DIR";

/// The log files under a driver's directory: its publications' and its images'.
fn log_files(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for kind in ["publications", "images"] {
        let Ok(entries) = std::fs::read_dir(dir.join(kind)) else {
            continue;
        };
        for entry in entries {
            found.push(entry.expect("an entry").path());
        }
    }
    found.sort();
    found
}

/// A driver that cannot create a publication's log refuses the add with error code
/// 12, counts it, and leaves no file in its directory, however often it is asked
/// (invariant 3; `spec/log-buffer.md`, "File"; ADR 0171; work item 492; GitHub
/// issue 238). Each ask has a correlation id of its own, and so a file name of its
/// own: before the removal each left a file as long as the log, and the first kept
/// what room a full directory had left. The creation is made to fail for real, after
/// its file exists, in a child process whose files the operating system limits to
/// less than a `cavalry:ipc` log: the driver starts, and sizing a log of 192 MiB
/// fails, as it does where the directory has no room.
#[test]
fn a_driver_that_cannot_create_a_log_refuses_it_and_leaves_no_file() {
    let dir = TestDir::new("log-not-created");
    // The limit is in blocks of 512 or 1,024 bytes, by the shell: 32 or 64 MiB, more
    // than any file the driver makes for itself and less than the log's 192 MiB. The
    // signal a write past the limit sends is ignored, so that the sizing returns its
    // error.
    let child = std::process::Command::new("sh")
        .arg("-c")
        .arg(
            "trap '' XFSZ; ulimit -f 65536 && exec \"$0\" a_driver_runs_where_files_are_limited \
             --exact --ignored --nocapture --test-threads=1",
        )
        .arg(std::env::current_exe().expect("this program's path"))
        .env(LIMITED_DIR, dir.path())
        .output()
        .expect("the child runs");
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&child.stdout),
        String::from_utf8_lossy(&child.stderr)
    );
    assert!(child.status.success(), "{said}");
    assert!(said.contains("refused five times"), "{said}");
}

/// The child of [`a_driver_that_cannot_create_a_log_refuses_it_and_leaves_no_file`],
/// which runs it in a process whose files are limited: on its own it has nothing to
/// do.
#[test]
#[ignore = "the child of a_driver_that_cannot_create_a_log_refuses_it_and_leaves_no_file"]
fn a_driver_runs_where_files_are_limited() {
    let Some(dir) = std::env::var_os(LIMITED_DIR) else {
        return;
    };
    let dir = PathBuf::from(dir);
    let driver = Driver::start(&dir, dir.join("log")).expect("the driver starts under the limit");
    let mut client = Cavalry::connect_to(&dir).expect("the client connects");
    let failed = |client: &Cavalry| {
        let counters = client.counters();
        (0..counters.capacity())
            .find(|id| {
                counters
                    .record(*id)
                    .ok()
                    .flatten()
                    .is_some_and(|record| record.type_id() == DRIVER_LOG_CREATE_FAILED.0)
            })
            .map(|id| counters.read(id).expect("the counter reads"))
            .expect("the driver's counter")
    };
    assert_eq!(failed(&client), 0);
    for ask in 1..=5 {
        match client.add_exclusive_publication("cavalry:ipc", 10 + ask) {
            Err(ClientError::Refused {
                code: Some(ErrorCode::StorageSpace),
                ..
            }) => {}
            Err(other) => panic!("ask {ask}: {other}"),
            Ok(_) => panic!("ask {ask}: the log was created under the limit"),
        }
        assert_eq!(failed(&client), i64::from(ask), "one for each ask");
        assert_eq!(log_files(&dir), Vec::<PathBuf>::new(), "after ask {ask}");
    }
    // The driver goes on: a log under the limit is created, and is the one file.
    let channel = format!(
        "cavalry:udp?endpoint=127.0.0.1:{}|term-length=64k",
        ports::free_port()
    );
    let publication = client
        .add_exclusive_publication(&channel, 10)
        .expect("a publication whose log fits");
    assert_eq!(log_files(&dir).len(), 1, "{:?}", log_files(&dir));
    assert_eq!(failed(&client), 5);
    drop(publication);
    client.close().expect("the client closes");
    driver.close().expect("the driver closes");
    println!("refused five times");
}

/// The directory of [`a_driver_whose_directory_has_no_room_leaves_no_log_file`]'s
/// driver: one on a filesystem with less room than a `cavalry:ipc` log.
const FULL_DIR: &str = "CAVALRY_CLIENT_TEST_FULL_DIR";

/// The kibibytes that `df` reads as available on the filesystem that holds `dir`.
fn available_kib(dir: &Path) -> u64 {
    let said = std::process::Command::new("df")
        .arg("-Pk")
        .arg(dir)
        .output()
        .expect("df runs");
    let said = String::from_utf8_lossy(&said.stdout).into_owned();
    said.lines()
        .nth(1)
        .and_then(|line| line.split_whitespace().nth(3))
        .and_then(|available| available.parse().ok())
        .unwrap_or_else(|| panic!("df's fourth column of its second line: {said}"))
}

/// The case GitHub issue 238 measured, and the one that matters: a driver whose
/// directory is on a filesystem with no room for a log, where making the log's pages
/// resident is what fails (ADR 0027), after the file exists and holds whatever room
/// was left. The driver refuses each ask with error code 12, leaves no file, and the
/// filesystem has afterwards the room it had before (ADR 0171; work item 492).
///
/// It needs such a directory, which a test cannot make for itself without the right
/// to mount, so `just test` passes it over and `just shm-exhaustion <directory>` runs
/// it: in CI's Linux job on a filesystem of 64 MiB that the job mounts, and by hand
/// in a container whose `/dev/shm` is Docker's default, 64 MiB, less than the 192 MiB
/// of a `cavalry:ipc` log:
///
/// ```sh
/// docker run --rm -v "$PWD":/src:ro -e CARGO_TARGET_DIR=/target -w /src \
///   -e CAVALRY_CLIENT_TEST_FULL_DIR=/dev/shm/cavalry rust:1 \
///   cargo test -p cavalry-client --test publication \
///   a_driver_whose_directory_has_no_room -- --ignored --nocapture
/// ```
#[test]
#[ignore = "needs a driver directory on a filesystem with no room for a log; see its comment"]
fn a_driver_whose_directory_has_no_room_leaves_no_log_file() {
    let Some(dir) = std::env::var_os(FULL_DIR) else {
        panic!("set {FULL_DIR} to a directory on a filesystem with less room than 192 MiB");
    };
    let dir = PathBuf::from(dir);
    let log_dir = TestDir::new("no-room-log");
    let driver = Driver::start(&dir, log_dir.path()).expect("the driver starts");
    let mut client = Cavalry::connect_to(&dir).expect("the client connects");
    let before = available_kib(&dir);
    assert!(
        before < 192 * 1024,
        "{} has {before} KiB available, room for a cavalry:ipc log",
        dir.display()
    );
    for ask in 1..=5 {
        match client.add_exclusive_publication("cavalry:ipc", 10 + ask) {
            Err(ClientError::Refused {
                code: Some(ErrorCode::StorageSpace),
                message,
                ..
            }) => println!("ask {ask}: refused: {message}"),
            Err(other) => panic!("ask {ask}: {other}"),
            Ok(_) => panic!("ask {ask}: the log was created"),
        }
        let (files, available) = (log_files(&dir), available_kib(&dir));
        println!(
            "after ask {ask}: {} log files, {available} KiB available where {before} were",
            files.len()
        );
        assert_eq!(files, Vec::<PathBuf>::new(), "after ask {ask}");
        assert_eq!(available, before, "after ask {ask}");
    }
    client.close().expect("the client closes");
    driver.close().expect("the driver closes");
}
