//! The allocation gate over the UDP send path (CLAUDE.md, invariant 2 and gate 2; ADR
//! 0041): the publisher, the driver's four threads and a receiver by hand in this
//! process, under the counting allocator the bench uses, with ten thousand messages
//! crossing the loopback and no allocation at all. Alone in its binary, so that no other
//! test's threads are counted (`cargo test` runs a binary's tests in parallel).

use std::alloc::System;
use std::net::{SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use cavalry_client::Cavalry;
use cavalry_driver::{Driver, spec};
use cavalry_proto::udp::{ControlFrame, Datagram, SetupFrame, StatusMessage, parse};
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

/// A directory of its own per test, removed when the guard drops.
struct TestDir(PathBuf);

impl TestDir {
    fn new(test: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "cavalry-client-udp-alloc-{}-{test}",
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

fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while !condition() {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    true
}

/// A receiver by hand: a loopback socket that reads what the sender puts on the wire and
/// answers as `spec/udp.md`, "The receiver" says a receiver answers. It allocates
/// nothing after `bind`.
struct Receiver {
    socket: UdpSocket,
    buffer: Vec<u8>,
    peer: Option<SocketAddr>,
}

/// What one datagram carried.
#[derive(Debug)]
enum Received {
    Control(ControlFrame),
    /// The data frames' term offsets and bodies' first eight bytes, in order, and the
    /// datagram's bytes.
    Data {
        first_offset: u32,
        frames: usize,
        bytes: usize,
    },
    Other,
}

impl Receiver {
    fn bind() -> Self {
        let socket = UdpSocket::bind("127.0.0.1:0").expect("a loopback socket");
        socket
            .set_read_timeout(Some(Duration::from_millis(300)))
            .expect("a read timeout");
        Self {
            socket,
            buffer: vec![0u8; 2048],
            peer: None,
        }
    }

    fn channel(&self) -> String {
        format!(
            "cavalry:udp?endpoint={}",
            self.socket.local_addr().expect("bound")
        )
    }

    /// The next datagram, or `None` at the read timeout.
    fn next(&mut self) -> Option<Received> {
        let (length, from) = self.socket.recv_from(&mut self.buffer).ok()?;
        self.peer = Some(from);
        let bytes = &self.buffer[..length];
        match parse(bytes, spec::UDP_TERM_LENGTH) {
            Ok(ControlFrame::Data) => match Datagram::check(bytes, spec::UDP_TERM_LENGTH) {
                Ok(datagram) => Some(Received::Data {
                    first_offset: datagram.first_term_offset(),
                    frames: datagram.frame_count(),
                    bytes: length,
                }),
                Err(_) => Some(Received::Other),
            },
            Ok(frame) => Some(Received::Control(frame)),
            Err(_) => Some(Received::Other),
        }
    }

    /// Takes datagrams until a setup frame arrives.
    fn setup(&mut self) -> SetupFrame {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(Received::Control(ControlFrame::Setup(setup))) = self.next() {
                return setup;
            }
            assert!(Instant::now() < deadline, "no setup frame arrived");
        }
    }

    fn send_status_message(&self, message: &StatusMessage) {
        let bytes = message.to_bytes();
        self.socket
            .send_to(
                &bytes[..message.frame_length()],
                self.peer.expect("a setup frame arrived first"),
            )
            .expect("the status message is sent");
    }

    /// Takes data datagrams until `bytes` of frames have arrived in order from `from`,
    /// and returns how many frames they held; a control frame on the way is skipped.
    fn take_data(&mut self, from: u32, bytes: usize) -> usize {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut expected = from;
        let mut frames = 0;
        let end = from as usize + bytes;
        while (expected as usize) < end {
            match self.next() {
                Some(Received::Data {
                    first_offset,
                    frames: count,
                    bytes: length,
                }) => {
                    assert_eq!(first_offset, expected, "the frames arrive in order");
                    expected += length as u32;
                    frames += count;
                }
                Some(Received::Control(_)) => {}
                Some(Received::Other) => panic!("a datagram failed the check"),
                None => assert!(Instant::now() < deadline, "the data stopped at {expected}"),
            }
        }
        assert_eq!(
            expected as usize, end,
            "the last datagram ends at the limit"
        );
        frames
    }
}

fn status_message(setup: &SetupFrame, offset: i32, window: i32, flags: u8) -> StatusMessage {
    StatusMessage {
        flags,
        session_id: setup.session_id,
        stream_id: setup.stream_id,
        consumption_term_id: setup.active_term_id,
        consumption_term_offset: offset,
        receiver_window_length: window,
        receiver_id: 9,
        group_tag: None,
    }
}

/// Nothing on the send path allocates (CLAUDE.md, invariant 2): with the publisher, the
/// driver's four threads and the receiver in this process, ten thousand messages cross
/// the loopback under the counting allocator with no allocation at all.
#[test]
fn the_send_path_allocates_nothing_per_message() {
    let dir = TestDir::new("allocations");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("the driver starts");
    let mut client = Cavalry::connect_to(dir.path()).expect("connects");
    let mut receiver = Receiver::bind();
    let mut publication = client
        .add_publication(&receiver.channel(), 7)
        .expect("a publication");
    let setup = receiver.setup();
    receiver.send_status_message(&status_message(&setup, 0, 131072, 0));
    assert!(wait_until(Duration::from_secs(5), || publication.is_connected()));
    // Warm up: the first burst, the first heartbeat, every thread's first cycle, and
    // a quiet second for the client's first keepalive and every first-time path.
    for _ in 0..100 {
        publication.offer(&[1u8; 32]).expect("offers");
    }
    assert_eq!(receiver.take_data(0, 6400), 100);
    std::thread::sleep(Duration::from_millis(1200));

    // The window is kept open as the receiver reads, with a status message every
    // thousand frames; nothing in the region allocates: not the publisher, not the
    // conductor, not the sender's bursts, and not this receiver.
    let region = Region::new(GLOBAL);
    let mut offset = 6400u32;
    for _ in 0..10 {
        for _ in 0..1000 {
            while publication.offer(&[2u8; 32]).is_err() {
                std::thread::sleep(Duration::from_micros(50));
            }
        }
        assert_eq!(receiver.take_data(offset, 64_000), 1000);
        offset += 64_000;
        receiver.send_status_message(&status_message(&setup, offset as i32, 131072, 0));
    }
    std::thread::sleep(Duration::from_millis(150));
    let change = region.change();
    assert_eq!(
        change.allocations, 0,
        "allocations on the send path over 10,000 messages: {change:?}"
    );
    publication.close().expect("closes");
    client.close().expect("closes");
    driver.close().expect("the driver closes");
}
