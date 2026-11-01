//! The allocation gate over the UDP send path (CLAUDE.md, invariant 2 and gate 2; ADR
//! 0041): the publisher, the driver's four threads and a receiver by hand in this
//! process, under the counting allocator the bench uses, with ten thousand messages
//! crossing the loopback and no allocation at all. Alone in its binary, so that no other
//! test's threads are counted (`cargo test` runs a binary's tests in parallel).

use std::alloc::System;
use std::net::{SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use cavalry_client::{Cavalry, Publication};
use cavalry_driver::{Driver, spec};
use cavalry_proto::udp::{ControlFrame, Datagram, SetupFrame, StatusMessage, parse};
use nix::sys::socket::{getsockopt, setsockopt, sockopt};
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

/// The messages the gate sends: ten rounds of a thousand, each 32 bytes in a 64-byte
/// frame, with a status message after every round.
const ROUNDS: usize = 10;
const MESSAGES_PER_ROUND: usize = 1000;
const FRAME_LENGTH: usize = 64;

/// How long any wait in this test may last before it fails with a message: a
/// regression fails in seconds, never at the job's `timeout-minutes`.
const DEADLINE: Duration = Duration::from_secs(5);

/// The receive buffer the receiver by hand asks of its socket. The sender puts frames
/// on the wire as the publisher commits them, in datagrams of whatever is committed at
/// each duty cycle, so a round of a thousand frames published before any is read may
/// arrive as a thousand datagrams; the kernel charges each against the socket's buffer
/// at its own size, about two kibibytes for a small datagram on Linux and macOS, and
/// drops what does not fit. Linux's default buffer, 212992 bytes, holds a round only
/// when the sender gathers most frames into full datagrams, which is a race the sender
/// loses on a slow publisher; this receiver has no NAK, so a dropped datagram is a gap
/// it cannot close. The host may give less than asked (Linux caps at
/// `net.core.rmem_max`), so [`Receiver::bind`] reads back what it gave and the test
/// publishes no more than that holds before it reads.
const RECEIVE_BUFFER: usize = 4 * 1024 * 1024;

/// What the kernel charges one datagram against the receive buffer, at most: Linux
/// rounds a small datagram's buffer up to a kibibyte or two of `skb->truesize`, macOS
/// to a two-kibibyte mbuf cluster.
const KERNEL_BYTES_PER_DATAGRAM: usize = 2048;

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

/// Offers `message` until the publication takes it, within [`DEADLINE`]: a refusal is
/// retried, since `back-pressured` and `not-connected` are the publisher's to wait out,
/// and one that lasts fails with its reason and the publication's state, never hangs.
/// Nothing here allocates while the offer is taken or refused.
fn offer(publication: &mut Publication, message: &[u8]) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        match publication.offer(message) {
            Ok(_) => return,
            Err(refusal) => {
                assert!(
                    Instant::now() < deadline,
                    "the offer was refused for {DEADLINE:?} ({refusal}); connected {}, limit {}, \
                     position {:?}",
                    publication.is_connected(),
                    publication.limit(),
                    publication.position()
                );
                std::thread::sleep(Duration::from_micros(50));
            }
        }
    }
}

/// A receiver by hand: a loopback socket that reads what the sender puts on the wire and
/// answers as `spec/udp.md`, "The receiver" says a receiver answers. It allocates
/// nothing after `bind`.
struct Receiver {
    socket: UdpSocket,
    buffer: Vec<u8>,
    peer: Option<SocketAddr>,
    /// How many frames may be on the wire unread without the socket dropping one: the
    /// receive buffer the host gave, at one frame per datagram and
    /// [`KERNEL_BYTES_PER_DATAGRAM`] each.
    unread_frames: usize,
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
        // What the host gives is the bound, not what was asked: a capped buffer is read
        // back here and shrinks how much a round publishes before it is read.
        let _ = setsockopt(&socket, sockopt::RcvBuf, &RECEIVE_BUFFER);
        let given = getsockopt(&socket, sockopt::RcvBuf).expect("the receive buffer is read back");
        let unread_frames = (given / KERNEL_BYTES_PER_DATAGRAM).clamp(1, MESSAGES_PER_ROUND);
        Self {
            socket,
            buffer: vec![0u8; 2048],
            peer: None,
            unread_frames,
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
        let deadline = Instant::now() + DEADLINE;
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
    /// The deadline is checked on every datagram, not only at a read timeout: once the
    /// data stops, the sender's heartbeats keep arriving every 100 ms, under the read
    /// timeout, and a check at the timeout alone would never run.
    fn take_data(&mut self, from: u32, bytes: usize) -> usize {
        let deadline = Instant::now() + DEADLINE;
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
                None => {}
            }
            assert!(
                Instant::now() < deadline,
                "the data stopped at {expected} of {end}"
            );
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
    assert!(wait_until(DEADLINE, || publication.is_connected()));
    // Warm up: the first burst, the first heartbeat, every thread's first cycle, and
    // a quiet second for the client's first keepalive and every first-time path.
    for _ in 0..100 {
        offer(&mut publication, &[1u8; 32]);
    }
    assert_eq!(receiver.take_data(0, 100 * FRAME_LENGTH), 100);
    std::thread::sleep(Duration::from_millis(1200));

    // Ten rounds of a thousand messages, each read once it is published and never more
    // than the receiver's buffer holds unread, with a status message after every round
    // to keep the window open; nothing in the region allocates: not the publisher, not
    // the conductor, not the sender's bursts, and not this receiver.
    let region = Region::new(GLOBAL);
    let mut offset = (100 * FRAME_LENGTH) as u32;
    for _ in 0..ROUNDS {
        let mut published = 0;
        while published < MESSAGES_PER_ROUND {
            let batch = receiver.unread_frames.min(MESSAGES_PER_ROUND - published);
            for _ in 0..batch {
                offer(&mut publication, &[2u8; 32]);
            }
            assert_eq!(receiver.take_data(offset, batch * FRAME_LENGTH), batch);
            offset += (batch * FRAME_LENGTH) as u32;
            published += batch;
        }
        receiver.send_status_message(&status_message(&setup, offset as i32, 131072, 0));
    }
    std::thread::sleep(Duration::from_millis(150));
    let change = region.change();
    assert_eq!(
        change.allocations,
        0,
        "allocations on the send path over {} messages: {change:?}",
        ROUNDS * MESSAGES_PER_ROUND
    );
    publication.close().expect("closes");
    client.close().expect("closes");
    driver.close().expect("the driver closes");
}
