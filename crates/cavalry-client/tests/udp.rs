//! A `cavalry:udp` publication against a real driver in this process and a receiver by
//! hand on a loopback socket (`spec/udp.md`, "The sender"; ADR 0041; CLAUDE.md,
//! invariants 1, 2 and 3): setup frames until a status message answers, the stream in
//! order, a NAK answered by a retransmit, the sender held at the receiver window, a
//! heartbeat when idle and EOS after the close; `tests/udp_allocations.rs` is the
//! allocation gate over the same path, alone in its process.

use std::net::{SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use cavalry_client::Cavalry;
use cavalry_driver::{Driver, spec};
use cavalry_proto::frame::Flags;
use cavalry_proto::udp::{
    ControlFrame, Datagram, Nak, STATUS_MESSAGE_END_OF_STREAM, SetupFrame, StatusMessage, parse,
};
use cavalry_shm::counters::CountersReader;
use cavalry_shm::counters::generated::{
    CHANNEL_CLOSED, SENDER_FLOW_CONTROL_LIMITED, SENDER_HEARTBEATS_SENT, SENDER_RETRANSMITS_SENT,
    SENDER_RETRANSMITTED_BYTES,
};

/// A directory of its own per test, removed when the guard drops.
struct TestDir(PathBuf);

impl TestDir {
    fn new(test: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("cavalry-client-udp-{}-{test}", std::process::id()));
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

    fn send_nak(&self, nak: &Nak) {
        self.socket
            .send_to(
                &nak.to_bytes(),
                self.peer.expect("a setup frame arrived first"),
            )
            .expect("the NAK is sent");
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

fn read_counter(dir: &Path, id: cavalry_shm::counters::CounterId) -> i64 {
    CountersReader::open(dir)
        .expect("the counters file opens")
        .read_acquire(id.record())
        .expect("the counter is read")
}

/// The full case: setup frames until the status message, then the stream in order, a
/// NAK answered from the term, a heartbeat once idle, and EOS after the close until the
/// receiver acknowledges it.
#[test]
fn a_udp_publication_connects_on_a_status_message_and_sends_its_stream_in_order() {
    let dir = TestDir::new("stream");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("the driver starts");
    let mut client = Cavalry::connect_to(dir.path()).expect("connects");
    let mut receiver = Receiver::bind();
    let mut publication = client
        .add_publication(&receiver.channel(), 7)
        .expect("a publication");
    assert_eq!(publication.channel(), receiver.channel());
    assert!(
        !publication.is_connected(),
        "connected by a status message, not before"
    );
    assert_eq!(
        publication.offer(b"too early").unwrap_err().reason(),
        "not-connected"
    );

    // Setup frames go until the first status message without the send-setup flag.
    let setup = receiver.setup();
    assert_eq!(
        (setup.stream_id, setup.session_id),
        (7, publication.session_id())
    );
    let again = receiver.setup();
    assert_eq!(again, setup, "the same setup frame, every setup interval");
    receiver.send_status_message(&status_message(&setup, 0, 131072, 0));
    assert!(wait_until(Duration::from_secs(5), || publication.is_connected()));

    // One hundred messages of 32 bytes: 64-byte frames, in order, in datagrams of
    // whole frames.
    for i in 0..100u64 {
        let mut message = [0u8; 32];
        message[..8].copy_from_slice(&i.to_le_bytes());
        publication.offer(&message).expect("offers");
    }
    assert_eq!(receiver.take_data(0, 6400), 100);

    // A NAK for frames 10 to 19 is answered with those frames, from the term.
    receiver.send_nak(&Nak {
        flags: 0,
        session_id: setup.session_id,
        stream_id: 7,
        term_id: setup.active_term_id,
        term_offset: 640,
        length: 640,
    });
    assert_eq!(receiver.take_data(640, 640), 10);
    assert!(wait_until(Duration::from_secs(2), || {
        read_counter(dir.path(), SENDER_RETRANSMITS_SENT) == 1
    }));
    assert_eq!(read_counter(dir.path(), SENDER_RETRANSMITTED_BYTES), 640);

    // Idle for the heartbeat interval: a heartbeat at the sender position, no EOS.
    let deadline = Instant::now() + Duration::from_secs(5);
    let heartbeat = loop {
        assert!(Instant::now() < deadline, "no heartbeat arrived");
        match receiver.next() {
            Some(Received::Control(ControlFrame::Heartbeat(heartbeat))) => break heartbeat,
            Some(Received::Data { .. }) => panic!("data after the stream"),
            _ => {}
        }
    };
    assert_eq!(heartbeat.term_offset, 6400);
    assert!(!heartbeat.is_end_of_stream());
    assert!(read_counter(dir.path(), SENDER_HEARTBEATS_SENT) >= 1);

    // Closed: heartbeats carry EOS; the receiver's acknowledgement ends the linger, the
    // log goes, and the endpoint closes with its last publication.
    let registration_id = publication.registration_id();
    publication.close().expect("closes");
    let log = dir
        .path()
        .join(format!("publications/{registration_id}.logbuffer"));
    let deadline = Instant::now() + Duration::from_secs(5);
    let eos = loop {
        assert!(Instant::now() < deadline, "no heartbeat with EOS arrived");
        match receiver.next() {
            Some(Received::Control(ControlFrame::Heartbeat(heartbeat)))
                if heartbeat.is_end_of_stream() =>
            {
                break heartbeat;
            }
            _ => {}
        }
    };
    assert_eq!(eos.term_offset, 6400);
    assert!(eos.flags & Flags::EOS.bits() != 0);
    receiver.send_status_message(&status_message(
        &setup,
        6400,
        131072,
        STATUS_MESSAGE_END_OF_STREAM,
    ));
    assert!(
        wait_until(Duration::from_secs(5), || !log.exists()),
        "the log is removed once the receiver acknowledged the end of stream"
    );
    assert!(wait_until(Duration::from_secs(5), || {
        read_counter(dir.path(), CHANNEL_CLOSED) == 1
    }));
    client.close().expect("closes");
    driver.close().expect("the driver closes");
}

/// The sender limit is the consumption position plus the receiver window: the wire
/// stops there, `sender.flow_control_limited` counts the entry, and the next status
/// message opens the window again.
#[test]
fn the_sender_is_held_at_the_receiver_window_until_the_next_status_message() {
    let dir = TestDir::new("window");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("the driver starts");
    let mut client = Cavalry::connect_to(dir.path()).expect("connects");
    let mut receiver = Receiver::bind();
    let mut publication = client
        .add_publication(&receiver.channel(), 7)
        .expect("a publication");
    let setup = receiver.setup();
    receiver.send_status_message(&status_message(&setup, 0, 4096, 0));
    assert!(wait_until(Duration::from_secs(5), || publication.is_connected()));

    // 128 frames of 64 bytes: the publisher's window is the publication window, so
    // every offer lands; the wire carries 4096 bytes and stops.
    for _ in 0..128 {
        publication.offer(&[7u8; 32]).expect("offers");
    }
    assert_eq!(receiver.take_data(0, 4096), 64);
    assert!(
        receiver
            .next()
            .is_none_or(|received| !matches!(received, Received::Data { .. }))
    );
    assert!(wait_until(Duration::from_secs(2), || {
        read_counter(dir.path(), SENDER_FLOW_CONTROL_LIMITED) == 1
    }));

    // The receiver consumed: the rest follows.
    receiver.send_status_message(&status_message(&setup, 4096, 4096, 0));
    assert_eq!(receiver.take_data(4096, 4096), 64);
    publication.close().expect("closes");
    client.close().expect("closes");
    driver.close().expect("the driver closes");
}
