//! A client against a real driver in this process: attaching, what is refused, and how
//! a client learns that the driver is gone (`spec/client-protocol.md`, "The driver
//! directory" and "Liveness and timeouts"; ADR 0021; CLAUDE.md, invariants 1 and 5).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use cavalry_client::{Cavalry, Closed};
use cavalry_driver::Driver;
use cavalry_shm::counters::CountersReader;
use cavalry_shm::counters::generated::CLIENT_TIMED_OUT;

/// A directory of its own per test, removed when the guard drops.
struct TestDir(PathBuf);

impl TestDir {
    fn new(test: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("cavalry-client-{}-{test}", std::process::id()));
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

fn wait_until(timeout: Duration, mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    done()
}

#[test]
fn a_client_connects_to_a_running_driver_and_closes() {
    let dir = TestDir::new("connect");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("the driver starts");
    let client = Cavalry::connect_to(dir.path()).expect("the client connects");
    assert!(
        client.client_id() >= 0,
        "a client id from the ring's correlation id counter, which starts at 0"
    );
    assert_eq!(client.dir(), dir.path());
    assert!(!client.is_closed());
    assert_eq!(client.closed(), None);
    assert!(format!("{client:?}").contains("Cavalry"));
    let second = Cavalry::connect_to(dir.path()).expect("a second client connects");
    assert!(
        second.client_id() > client.client_id(),
        "no two clients share an id: {} then {}",
        client.client_id(),
        second.client_id()
    );
    client.close().expect("closes");
    second.close().expect("closes");
    driver.close().expect("the driver closes");
}

#[test]
fn no_driver_is_not_found_and_a_closed_one_is_gone() {
    let dir = TestDir::new("absent");
    let error = Cavalry::connect_to(dir.path()).expect_err("no directory");
    assert_eq!(error.reason(), "not-found");
    assert!(error.to_string().contains("cavalry run"), "{error}");

    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("the driver starts");
    driver.close().expect("the driver closes");
    let error = Cavalry::connect_to(dir.path()).expect_err("a closed driver");
    assert_eq!(error.reason(), "driver-gone");
    assert!(error.to_string().contains("driver-closed"), "{error}");
}

/// The driver timeout of `spec/client-protocol.md`, "Liveness and timeouts": how long
/// a connecting client waits for a driver that is creating its files (ADR 0072).
const DRIVER_TIMEOUT: Duration = cavalry_driver::spec::DRIVER_TIMEOUT;

/// How long past the bound a refusal may arrive: the read after the bound and the
/// scheduling of a loaded host.
const SLACK: Duration = Duration::from_secs(5);

/// A file as its creator leaves it between two steps of creating it
/// (`spec/client-protocol.md`, "The driver directory"; ADR 0072).
#[derive(Clone, Copy, Debug)]
enum Unfinished {
    /// Not created yet.
    Missing,
    /// Created and not yet sized: no bytes.
    Empty,
    /// Sized and its magic not yet written: eight zero bytes where the magic is, which
    /// every page of `spec/` calls `not-ready`.
    NotReady,
}

impl Unfinished {
    /// Puts the state at `path`, where no file is.
    fn put(self, path: &Path) {
        match self {
            Unfinished::Missing => {}
            Unfinished::Empty => {
                std::fs::File::create_new(path).expect("the empty file");
            }
            Unfinished::NotReady => {
                // Longer than any header or trailer of the three pages, all zero.
                std::fs::write(path, vec![0u8; 4096]).expect("the zero file");
            }
        }
    }
}

/// The full case first: a creator that died leaves its file unfinished for good, and a
/// client must not wait for it for ever (CLAUDE.md, invariant 3). Three directories,
/// read at once since each takes the driver timeout:
///
/// - a command ring left `not-ready` and nothing else, which is a dead driver's to a
///   starting driver too: `driver-timeout`;
/// - a ring that was finished and a broadcast buffer that was not, by a creator that
///   then died, so nothing stores the heartbeat again: `driver-timeout`. The ring's
///   heartbeat is put 5 s ahead of the clock, which is what a wall clock set back by
///   5 s during the wait leaves: the heartbeat's age never reaches the driver
///   timeout, and the client must still tell the dead creator from a running driver,
///   by the heartbeat that never moves;
/// - a live driver whose broadcast buffer reads `not-ready`, which no driver leaves
///   and only a damaged directory shows: the buffer's own check, as before the wait.
#[test]
fn a_file_left_unfinished_is_refused_at_the_driver_timeout() {
    let ring_alone = TestDir::new("unfinished-ring");
    std::fs::create_dir_all(ring_alone.path()).expect("the directory");
    Unfinished::NotReady.put(&ring_alone.path().join("command-ring"));

    let ring_done = TestDir::new("unfinished-broadcast");
    std::fs::create_dir_all(ring_done.path()).expect("the directory");
    let _counters = cavalry_shm::counters::CountersFile::create(
        ring_done.path(),
        cavalry_driver::spec::COUNTERS_CAPACITY,
    )
    .expect("the counters");
    let ring = cavalry_shm::command_ring::CommandRing::create(
        ring_done.path(),
        cavalry_driver::spec::COMMAND_RING_CAPACITY,
    )
    .expect("the ring");
    let ahead = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after 1970")
        + Duration::from_secs(5);
    ring.consumer(DRIVER_TIMEOUT)
        .expect("the ring's consumer")
        .store_heartbeat(i64::try_from(ahead.as_nanos()).expect("before 2262"));
    Unfinished::NotReady.put(&ring_done.path().join("broadcast-buffer"));

    let damaged = TestDir::new("unfinished-damaged");
    let driver =
        Driver::start(damaged.path(), damaged.path().join("log")).expect("the driver starts");
    std::fs::remove_file(damaged.path().join("broadcast-buffer")).expect("the buffer goes");
    Unfinished::NotReady.put(&damaged.path().join("broadcast-buffer"));

    // The error and how long the call took.
    let refused = |dir: &Path| {
        let started = Instant::now();
        let error = Cavalry::connect_to(dir).expect_err("an unfinished file");
        (error, started.elapsed())
    };
    let (ring_alone, ring_done, damaged) = std::thread::scope(|scope| {
        let ring_alone = scope.spawn(|| refused(ring_alone.path()));
        let ring_done = scope.spawn(|| refused(ring_done.path()));
        let damaged = scope.spawn(|| refused(damaged.path()));
        (
            ring_alone.join().expect("joins"),
            ring_done.join().expect("joins"),
            damaged.join().expect("joins"),
        )
    });
    driver.close().expect("the driver closes");

    for (what, (error, waited)) in [("ring alone", &ring_alone), ("ring done", &ring_done)] {
        assert_eq!(error.reason(), "driver-gone", "{what}: {error}");
        let message = error.to_string();
        assert!(message.contains("driver-timeout"), "{what}: {message}");
        assert!(message.contains("cavalry run"), "{what}: {message}");
        assert!(
            message.contains("docs/reference/client-protocol.md#the-driver-directory"),
            "{what}: {message}"
        );
        assert!(
            *waited >= DRIVER_TIMEOUT && *waited < DRIVER_TIMEOUT + SLACK,
            "{what}: waited {waited:?}, the bound is {DRIVER_TIMEOUT:?}"
        );
    }
    let (error, waited) = &damaged;
    assert_eq!(error.reason(), "broadcast", "{error}");
    let message = error.to_string();
    assert!(message.contains("not-ready"), "{message}");
    assert!(
        message.contains("docs/reference/broadcast-buffer.md#"),
        "{message}"
    );
    assert!(
        *waited >= DRIVER_TIMEOUT && *waited < DRIVER_TIMEOUT + SLACK,
        "waited {waited:?}, the bound is {DRIVER_TIMEOUT:?}"
    );
}

/// Each file of the three in each state its creator leaves it in on the way, shown to
/// a connecting client for a while and then finished: the client waits and connects
/// (`spec/client-protocol.md`, "The driver directory"; ADR 0072). The driver is a real
/// one, and its own file is moved aside and back, so that the client reads what a
/// driver wrote once the stand-in is gone. A missing command ring is not here: that is
/// no driver, `not-found` at once, as `no_driver_is_not_found_and_a_closed_one_is_gone`
/// holds.
#[test]
fn a_client_waits_for_a_file_the_driver_has_not_finished() {
    let cases = [
        ("command-ring", Unfinished::Empty),
        ("command-ring", Unfinished::NotReady),
        ("broadcast-buffer", Unfinished::Missing),
        ("broadcast-buffer", Unfinished::Empty),
        ("broadcast-buffer", Unfinished::NotReady),
        ("counters", Unfinished::Missing),
        ("counters", Unfinished::Empty),
        ("counters", Unfinished::NotReady),
    ];
    let dir = TestDir::new("unfinished-then-ready");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("the driver starts");
    for (file, state) in cases {
        let path = dir.path().join(file);
        let aside = dir.path().join(format!("{file}.aside"));
        std::fs::rename(&path, &aside).expect("the driver's file moves aside");
        state.put(&path);
        let started = Instant::now();
        let client = std::thread::scope(|scope| {
            let connecting = scope.spawn(|| Cavalry::connect_to(dir.path()));
            std::thread::sleep(Duration::from_millis(150));
            assert!(
                !connecting.is_finished(),
                "{file} {state:?}: the client did not wait"
            );
            // One step, as a creator's last store is: the next read finds the file whole.
            std::fs::rename(&aside, &path).expect("the driver's file moves back");
            connecting.join().expect("joins")
        });
        let client = client.unwrap_or_else(|error| panic!("{file} {state:?}: {error}"));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{file} {state:?}: connected {:?} after the file was whole",
            started.elapsed()
        );
        assert_eq!(client.closed(), None, "{file} {state:?}");
        client.close().expect("closes");
    }
    driver.close().expect("the driver closes");
}

/// The thing itself: a client that connects the moment the command ring appears,
/// while the driver is still creating its files, connects. `Driver::start` returns
/// when the files are made, so the client races it from this thread.
#[test]
fn a_client_connects_while_the_driver_creates_its_files() {
    for round in 0..40 {
        let dir = TestDir::new(&format!("starting-{round}"));
        let (driver, client) = std::thread::scope(|scope| {
            let starting = scope.spawn(|| Driver::start(dir.path(), dir.path().join("log")));
            let ring = dir.path().join("command-ring");
            let deadline = Instant::now() + Duration::from_secs(10);
            while !ring.is_file() {
                assert!(Instant::now() < deadline, "round {round}: no command ring");
                std::hint::spin_loop();
            }
            let client = Cavalry::connect_to(dir.path());
            (starting.join().expect("joins"), client)
        });
        let driver = driver.expect("the driver starts");
        let client = client.unwrap_or_else(|error| panic!("round {round}: {error}"));
        assert_eq!(client.closed(), None, "round {round}");
        client.close().expect("closes");
        driver.close().expect("the driver closes");
    }
}

#[test]
fn a_driver_that_closes_closes_the_client() {
    let dir = TestDir::new("driver-closes");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("the driver starts");
    let client = Cavalry::connect_to(dir.path()).expect("the client connects");
    driver.close().expect("the driver closes");
    assert!(
        wait_until(Duration::from_secs(3), || client.is_closed()),
        "the conductor saw the heartbeat at -1"
    );
    assert_eq!(client.closed(), Some(Closed::DriverClosed));
    let error = client.close().err();
    assert!(
        error.is_none(),
        "closing a closed client is not an error: {error:?}"
    );
}

#[test]
fn a_quiet_client_stays_alive_past_the_liveness_timeout() {
    let dir = TestDir::new("quiet");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("the driver starts");
    let client = Cavalry::connect_to(dir.path()).expect("the client connects");
    // The client liveness timeout is 10 s: a client that sent nothing for that long is
    // timed out and told. The conductor's keepalives keep this one alive.
    std::thread::sleep(Duration::from_millis(11_500));
    assert!(!client.is_closed(), "closed: {:?}", client.closed());
    let timed_out = CountersReader::open(dir.path())
        .expect("the counters open")
        .read(CLIENT_TIMED_OUT.record())
        .expect("the counter reads");
    assert_eq!(timed_out, 0, "the driver timed no client out");
    client.close().expect("closes");
    driver.close().expect("the driver closes");
}

#[test]
fn the_default_directory_is_cavalry_dir_or_the_drivers_default() {
    // The variable is read at the call. Setting it in-process is `unsafe` since edition
    // 2024 and the crate denies `unsafe`, so this test reads what the environment has:
    // `cavalry`'s CLI tests set the variable on a child process and cover that side.
    let dir = Cavalry::default_dir();
    match std::env::var_os("CAVALRY_DIR").filter(|value| !value.is_empty()) {
        Some(value) => assert_eq!(dir, PathBuf::from(value)),
        None => {
            let name = dir
                .file_name()
                .expect("a name")
                .to_string_lossy()
                .into_owned();
            assert!(name.starts_with("cavalry-"), "{name}");
            assert_eq!(dir, Driver::default_dir(), "the driver's default");
        }
    }
}
