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
    let driver = Driver::start(dir.path()).expect("the driver starts");
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

    let driver = Driver::start(dir.path()).expect("the driver starts");
    driver.close().expect("the driver closes");
    let error = Cavalry::connect_to(dir.path()).expect_err("a closed driver");
    assert_eq!(error.reason(), "driver-gone");
    assert!(error.to_string().contains("driver-closed"), "{error}");
}

#[test]
fn a_driver_that_closes_closes_the_client() {
    let dir = TestDir::new("driver-closes");
    let driver = Driver::start(dir.path()).expect("the driver starts");
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
    let driver = Driver::start(dir.path()).expect("the driver starts");
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
