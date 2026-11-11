//! Remove then add a subscription on one `cavalry:udp` channel in one client, at once
//! (card 194; ADR 0049): the second add waits in the driver for the receiver to close
//! the first endpoint and then binds it again, so the client sees `subscription ready`
//! and no refusal, and `channel.opened` counts 2.
use std::path::{Path, PathBuf};

use cavalry_client::Cavalry;
use cavalry_driver::Driver;
use cavalry_shm::counters::CountersReader;
use cavalry_shm::counters::generated::{CHANNEL_CLOSED, CHANNEL_OPENED};

// This test's ports, from the one file that says how they are taken and why, which is
// `cavalry-raft`'s: `crates/cavalry-raft/tests/ports/mod.rs`.
#[path = "../../cavalry-raft/tests/ports/mod.rs"]
mod ports;

/// A driver directory of its own, with its log directory beside it, removed when the
/// guard drops.
struct TestDir(PathBuf);

impl TestDir {
    fn new(test: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "cavalry-client-resubscribe-{}-{test}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        let _ = std::fs::remove_dir_all(path.with_extension("log"));
        TestDir(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn log_dir(&self) -> PathBuf {
        self.0.with_extension("log")
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
        let _ = std::fs::remove_dir_all(self.log_dir());
    }
}

#[test]
fn a_subscription_removed_and_added_on_one_udp_channel_needs_no_retry() {
    let dir = TestDir::new("once");
    let driver = Driver::start(dir.path(), dir.log_dir()).expect("the driver");
    let port = ports::free_port();
    let channel = format!("cavalry:udp?endpoint=127.0.0.1:{port}");
    let mut client = Cavalry::connect_to(dir.path()).expect("a client");
    let first = client
        .add_subscription(&channel, 1001)
        .expect("the first subscription");
    first.close().expect("the first subscription closes");
    // At once: the driver's receiver may not have closed the endpoint yet. The add
    // waits for it inside the driver and answers `subscription ready`, never code 4.
    let second = client
        .add_subscription(&channel, 1001)
        .expect("the second subscription, with no retry");
    let reader = CountersReader::open(dir.path()).expect("the counters file");
    let read = |id: cavalry_shm::counters::CounterId| {
        reader.read_acquire(id.record()).expect("a static counter")
    };
    assert_eq!(read(CHANNEL_OPENED), 2, "the endpoint was bound twice");
    assert_eq!(read(CHANNEL_CLOSED), 1, "and closed once in between");
    second.close().expect("the second subscription closes");
    client.close().expect("the client closes");
    driver.close().expect("the driver closes");
}
