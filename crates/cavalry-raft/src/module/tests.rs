//! The module's own tests: what `ConsensusModule` cannot be asked for from outside.
//! A member leads a term whose base is not position 0 only after an election, which
//! this version's appointed leader never holds in one process (`spec/cluster.md`, "The
//! appointed leader"), so the test here drives the module's [`Io`] itself, over a
//! driver.

use std::net::UdpSocket;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

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

/// Loopback ports for a member's endpoints, from 20000 to 31999, each free when it is
/// chosen and chosen once in this process: below the range the operating system gives
/// a socket bound to port 0, as `tests/cluster.rs` chooses its own and for its reason.
fn free_ports(count: usize) -> Vec<u16> {
    const FIRST: u32 = 20_000;
    const SPAN: u32 = 12_000;
    static NEXT: AtomicU32 = AtomicU32::new(0);
    // Apart from where `tests/cluster.rs`, another process, starts its own choice.
    let start = (std::process::id() % 60) * 200 + 100;
    let mut sockets = Vec::with_capacity(count);
    for _ in 0..SPAN {
        if sockets.len() == count {
            break;
        }
        let offset = (start + NEXT.fetch_add(1, Ordering::Relaxed)) % SPAN;
        let port = u16::try_from(FIRST + offset).expect("below 32000");
        if let Ok(socket) = UdpSocket::bind(("127.0.0.1", port)) {
            sockets.push(socket);
        }
    }
    assert_eq!(sockets.len(), count, "free loopback ports from {FIRST}");
    sockets
        .iter()
        .map(|socket| socket.local_addr().expect("bound").port())
        .collect()
}

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
    drop(module);
    drop(follower);
    drop(driver);
}
