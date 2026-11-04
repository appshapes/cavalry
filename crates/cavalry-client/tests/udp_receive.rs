//! A `cavalry:udp` subscription against a real driver in this process, its stream from
//! a `cavalry:udp` publication on the same driver through a shim on the loopback that
//! drops, reorders and duplicates datagrams (`CLAUDE.md`, gate 3, the fault-injection
//! gate for the wire; `spec/udp.md`, "The receiver"; ADR 0042; invariant 1): every
//! message delivered once and in order through it, the gaps NAKed and mended, EOS and
//! the image's end delivered after loss; a late receiver that asks for the setup; and a
//! recording of an image on a second driver replayed byte for byte.
//! `tests/udp_receive_allocations.rs` is the allocation gate over the same path, alone
//! in its process.

use std::net::{SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use cavalry_client::{Cavalry, Offer, Publication, Subscription};
use cavalry_driver::{Driver, spec};
use cavalry_proto::frame::FrameType;
use cavalry_shm::counters::CountersReader;
use cavalry_shm::counters::generated::{
    IMAGE_CREATED, IMAGE_ENDED, IMAGE_REMOVED, RECEIVER_FRAMES_RECEIVED, RECEIVER_GAPS_DETECTED,
    RECEIVER_NAKS_SENT, RECEIVER_SETUPS_ELICITED, SENDER_RETRANSMITS_SENT,
};

/// A directory of its own per test, removed when the guard drops.
struct TestDir(PathBuf);

impl TestDir {
    fn new(test: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "cavalry-client-udp-receive-{}-{test}",
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

/// A port no socket holds at the moment it is read: bound and released.
fn free_port() -> u16 {
    UdpSocket::bind("127.0.0.1:0")
        .expect("a loopback socket")
        .local_addr()
        .expect("bound")
        .port()
}

fn read_counter(dir: &Path, id: cavalry_shm::counters::CounterId) -> i64 {
    CountersReader::open(dir)
        .expect("the counters file opens")
        .read_acquire(id.record())
        .expect("the counter is read")
}

/// Offers `message` until the publication takes it, within ten seconds: `not connected`
/// and `back pressured` are the publisher's to wait out.
fn offer(publication: &mut Publication, message: &[u8]) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match publication.offer(message) {
            Ok(_) => return,
            Err(Offer::NotConnected | Offer::BackPressured | Offer::AdminAction) => {
                assert!(
                    Instant::now() < deadline,
                    "the offer was refused for ten seconds; connected {}",
                    publication.is_connected()
                );
                std::thread::sleep(Duration::from_micros(50));
            }
            Err(other) => panic!("{other}"),
        }
    }
}

/// What the shim does to the data datagrams it forwards, by their count: every
/// `drop_every`th is dropped, every `duplicate_every`th is sent twice, and every
/// `hold_every`th is held and sent after the next one, so that datagrams arrive out of
/// order; and the first `drop_padding` datagrams that carry a padding frame, a 32-byte
/// header alone (`spec/udp.md`, "The datagram"; ADR 0045), are dropped, so that a
/// padding frame is NAKed and resent too. Control frames, setup frames and heartbeats go
/// through untouched, so that the stream connects and ends as it would on a clean wire;
/// what the faults cost is mended by the NAKs.
#[derive(Clone, Copy)]
struct Faults {
    drop_every: usize,
    duplicate_every: usize,
    hold_every: usize,
    drop_padding: usize,
}

/// A shim between the sender and the receiver on the loopback: the publication sends
/// to `facing_sender`, whose datagrams are forwarded from `facing_receiver` to the
/// receive endpoint with the faults applied, and the receiver's status messages, NAKs
/// and RTT answers, sent to `facing_receiver` as the datagrams' source, go back to the
/// sender's address, learned from its first datagram.
struct Shim {
    facing_sender: Arc<UdpSocket>,
    stop: Arc<AtomicBool>,
    forwarded: Arc<AtomicUsize>,
    dropped: Arc<AtomicUsize>,
    duplicated: Arc<AtomicUsize>,
    held: Arc<AtomicUsize>,
    padding_dropped: Arc<AtomicUsize>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl Shim {
    fn start(receive_endpoint: SocketAddr, faults: Faults) -> Self {
        let facing_sender = Arc::new(UdpSocket::bind("127.0.0.1:0").expect("a socket"));
        let facing_receiver = Arc::new(UdpSocket::bind("127.0.0.1:0").expect("a socket"));
        for socket in [&facing_sender, &facing_receiver] {
            socket
                .set_read_timeout(Some(Duration::from_millis(20)))
                .expect("a read timeout");
        }
        let stop = Arc::new(AtomicBool::new(false));
        let forwarded = Arc::new(AtomicUsize::new(0));
        let dropped = Arc::new(AtomicUsize::new(0));
        let duplicated = Arc::new(AtomicUsize::new(0));
        let held = Arc::new(AtomicUsize::new(0));
        let padding_dropped = Arc::new(AtomicUsize::new(0));
        // The sender's address, learned from its first datagram and shared with the
        // thread that forwards the receiver's answers back.
        let sender_address: Arc<std::sync::atomic::AtomicU64> =
            Arc::new(std::sync::atomic::AtomicU64::new(0));
        let forward = {
            let facing_sender = Arc::clone(&facing_sender);
            let facing_receiver = Arc::clone(&facing_receiver);
            let stop = Arc::clone(&stop);
            let forwarded = Arc::clone(&forwarded);
            let dropped = Arc::clone(&dropped);
            let duplicated = Arc::clone(&duplicated);
            let held = Arc::clone(&held);
            let padding_dropped = Arc::clone(&padding_dropped);
            let sender_address = Arc::clone(&sender_address);
            std::thread::spawn(move || {
                let mut buffer = vec![0u8; 65536];
                let mut data_count = 0usize;
                let mut holding: Option<Vec<u8>> = None;
                while !stop.load(Ordering::Acquire) {
                    let Ok((length, from)) = facing_sender.recv_from(&mut buffer) else {
                        continue;
                    };
                    if let SocketAddr::V4(v4) = from {
                        let packed = (u64::from(u32::from(*v4.ip())) << 16) | u64::from(v4.port());
                        sender_address.store(packed, Ordering::Release);
                    }
                    let datagram = &buffer[..length];
                    let is_data = datagram.len() >= 8
                        && i32::from_le_bytes([datagram[0], datagram[1], datagram[2], datagram[3]])
                            != 0
                        && matches!(
                            u16::from_le_bytes([datagram[6], datagram[7]]),
                            t if t == FrameType::Data as u16 || t == FrameType::Pad as u16
                        );
                    let send = |bytes: &[u8]| {
                        let _ = facing_receiver.send_to(bytes, receive_endpoint);
                    };
                    if !is_data {
                        send(datagram);
                        continue;
                    }
                    let is_padding =
                        u16::from_le_bytes([datagram[6], datagram[7]]) == FrameType::Pad as u16;
                    if is_padding && padding_dropped.load(Ordering::Acquire) < faults.drop_padding {
                        padding_dropped.fetch_add(1, Ordering::AcqRel);
                        continue;
                    }
                    data_count += 1;
                    if faults.drop_every > 0 && data_count.is_multiple_of(faults.drop_every) {
                        dropped.fetch_add(1, Ordering::AcqRel);
                        continue;
                    }
                    if faults.hold_every > 0
                        && data_count.is_multiple_of(faults.hold_every)
                        && holding.is_none()
                    {
                        holding = Some(datagram.to_vec());
                        held.fetch_add(1, Ordering::AcqRel);
                        continue;
                    }
                    send(datagram);
                    forwarded.fetch_add(1, Ordering::AcqRel);
                    if faults.duplicate_every > 0
                        && data_count.is_multiple_of(faults.duplicate_every)
                    {
                        send(datagram);
                        duplicated.fetch_add(1, Ordering::AcqRel);
                    }
                    if let Some(late) = holding.take() {
                        send(&late);
                        forwarded.fetch_add(1, Ordering::AcqRel);
                    }
                }
            })
        };
        let backward = {
            let facing_sender = Arc::clone(&facing_sender);
            let facing_receiver = Arc::clone(&facing_receiver);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                let mut buffer = vec![0u8; 2048];
                while !stop.load(Ordering::Acquire) {
                    let Ok((length, _)) = facing_receiver.recv_from(&mut buffer) else {
                        continue;
                    };
                    let packed = sender_address.load(Ordering::Acquire);
                    if packed == 0 {
                        continue;
                    }
                    let to = SocketAddr::from((
                        std::net::Ipv4Addr::from((packed >> 16) as u32),
                        (packed & 0xffff) as u16,
                    ));
                    let _ = facing_sender.send_to(&buffer[..length], to);
                }
            })
        };
        Self {
            facing_sender,
            stop,
            forwarded,
            dropped,
            duplicated,
            held,
            padding_dropped,
            threads: vec![forward, backward],
        }
    }

    /// The channel a publication sends on: the shim's sender-facing port.
    fn channel(&self) -> String {
        format!(
            "cavalry:udp?endpoint={}",
            self.facing_sender.local_addr().expect("bound")
        )
    }

    /// Stops the shim: how many data datagrams it forwarded, dropped, duplicated, held,
    /// and how many padding datagrams it dropped.
    fn stop(mut self) -> (usize, usize, usize, usize, usize) {
        self.stop.store(true, Ordering::Release);
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
        (
            self.forwarded.load(Ordering::Acquire),
            self.dropped.load(Ordering::Acquire),
            self.duplicated.load(Ordering::Acquire),
            self.held.load(Ordering::Acquire),
            self.padding_dropped.load(Ordering::Acquire),
        )
    }
}

/// Polls `subscription` until `count` messages have been delivered, each a body of at
/// least eight bytes whose first eight are its sequence number, checking that every one
/// arrives once and in order; returns how many were delivered.
fn drain_in_order(subscription: &mut Subscription, count: u64, timeout: Duration) -> u64 {
    let deadline = Instant::now() + timeout;
    let mut next = 0u64;
    while next < count {
        let delivered = subscription
            .poll(
                |body, _header| {
                    let mut sequence = [0u8; 8];
                    sequence.copy_from_slice(&body[..8]);
                    let sequence = u64::from_le_bytes(sequence);
                    assert_eq!(sequence, next, "a message out of order, lost or doubled");
                    next += 1;
                },
                64,
            )
            .expect("the poll reads");
        if delivered == 0 {
            assert!(
                Instant::now() < deadline,
                "the stream stopped at {next} of {count}"
            );
            std::thread::sleep(Duration::from_micros(100));
        }
    }
    next
}

/// The full case of gate 3 for the wire: through a shim that drops every seventh data
/// datagram, duplicates every fifth and holds every third until the next has gone,
/// every one of four thousand messages reaches the subscriber once and in order, the
/// gaps are NAKed and mended, and the close delivers EOS and the image's end after the
/// loss (invariant 1).
#[test]
fn every_message_arrives_once_and_in_order_through_a_lossy_reordering_shim() {
    let dir = TestDir::new("shim");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("the driver starts");
    let mut client = Cavalry::connect_to(dir.path()).expect("connects");
    let receive_port = free_port();
    let receive_channel = format!("cavalry:udp?endpoint=127.0.0.1:{receive_port}");
    let shim = Shim::start(
        SocketAddr::from(([127, 0, 0, 1], receive_port)),
        Faults {
            drop_every: 7,
            duplicate_every: 5,
            hold_every: 3,
            drop_padding: 0,
        },
    );
    let mut subscription = client
        .add_subscription(&receive_channel, 7)
        .expect("a subscription");
    let mut publication = client
        .add_publication(&shim.channel(), 7)
        .expect("a publication");
    assert!(
        wait_until(Duration::from_secs(5), || publication.is_connected()),
        "the receiver's status message connected the publication"
    );
    assert!(wait_until(Duration::from_secs(5), || {
        subscription.poll(|_, _| {}, 0).expect("the poll applies");
        subscription.image_count() == 1
    }));
    let image = &subscription.images()[0];
    assert_eq!(image.session_id(), publication.session_id());
    assert!(
        image.source_identity().starts_with("127.0.0.1:"),
        "{}",
        image.source_identity()
    );
    assert_eq!(read_counter(dir.path(), IMAGE_CREATED), 1);

    // Four thousand messages, published as fast as the window allows while the
    // subscriber drains, in order and once.
    const MESSAGES: u64 = 4000;
    let publisher = std::thread::spawn(move || {
        for i in 0..MESSAGES {
            let mut message = [0u8; 32];
            message[..8].copy_from_slice(&i.to_le_bytes());
            offer(&mut publication, &message);
        }
        publication
    });
    assert_eq!(
        drain_in_order(&mut subscription, MESSAGES, Duration::from_secs(30)),
        MESSAGES
    );
    let publication = publisher.join().expect("the publisher");
    assert!(
        read_counter(dir.path(), RECEIVER_GAPS_DETECTED) > 0,
        "loss was seen"
    );
    assert!(
        read_counter(dir.path(), RECEIVER_NAKS_SENT) > 0,
        "and NAKed"
    );
    assert!(
        read_counter(dir.path(), SENDER_RETRANSMITS_SENT) > 0,
        "and mended"
    );
    assert_eq!(
        read_counter(dir.path(), RECEIVER_FRAMES_RECEIVED),
        MESSAGES as i64,
        "a duplicate is not a frame received"
    );

    // Closed: the heartbeats carry EOS, the image ends at the last message, where the
    // subscriber already stands, and the subscription sees the image go. The image
    // may go before this thread looks, since the subscriber is at the end already;
    // when it is still there, its end of stream is the position the stream reached.
    let end = subscription.images()[0].position();
    publication.close().expect("closes");
    assert!(wait_until(Duration::from_secs(10), || {
        subscription.poll(|_, _| {}, 0).expect("the poll applies");
        match subscription.images().first() {
            Some(image) if image.is_end_of_stream() => {
                assert_eq!(image.position(), end, "the stream ends where it stopped");
                true
            }
            Some(_) => false,
            None => true,
        }
    }));
    assert!(wait_until(Duration::from_secs(10), || {
        subscription.poll(|_, _| {}, 0).expect("the poll applies");
        subscription.image_count() == 0
    }));
    assert_eq!(read_counter(dir.path(), IMAGE_ENDED), 1);
    assert_eq!(read_counter(dir.path(), IMAGE_REMOVED), 1);
    let (forwarded, dropped, duplicated, held, _) = shim.stop();
    assert!(dropped > 0 && duplicated > 0 && held > 0, "the faults ran");
    assert!(forwarded > 0);
    subscription.close().expect("closes");
    client.close().expect("closes");
    driver.close().expect("the driver closes");
}

/// Card 196: a body whose aligned frame does not divide the 16 MiB term leaves a padding
/// frame at the end of every term, which the sender carries as its 32-byte header
/// alone (`spec/udp.md`, "The datagram"; ADR 0045). A 100-byte body, 160-byte frames,
/// crosses two term ends through the lossy shim, which also drops the first padding
/// datagram it sees, so that the padding is NAKed and resent as any frame; every message
/// arrives once and in order, and the image's position is the stream's (invariant 1).
/// Before ADR 0045 the receiver rejected the padding header at the first term end,
/// message 104,857, and the stream stayed there.
#[test]
fn a_stream_whose_frames_do_not_divide_the_term_crosses_two_term_ends_through_the_shim() {
    const BODY: usize = 100;
    const FRAME: u64 = 160;
    const TERM: u64 = 16 * 1024 * 1024;
    // 104,857 frames fill a term, with 96 bytes of padding at its end; two term ends,
    // then a thousand more messages into the third term.
    const FRAMES_PER_TERM: u64 = TERM / FRAME;
    const MESSAGES: u64 = 2 * FRAMES_PER_TERM + 1000;
    let dir = TestDir::new("shim-padding");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("the driver starts");
    let mut client = Cavalry::connect_to(dir.path()).expect("connects");
    let receive_port = free_port();
    let receive_channel = format!("cavalry:udp?endpoint=127.0.0.1:{receive_port}");
    let shim = Shim::start(
        SocketAddr::from(([127, 0, 0, 1], receive_port)),
        // Lighter faults than the four-thousand-message case: a retransmit lingers
        // 10 ms, during which a NAK for another range is dropped (`spec/udp.md`, "The
        // sender", step 4), so a drop every seventh datagram paces two hundred thousand
        // messages to a minute; every seven-hundred-and-first keeps the test to seconds
        // and the faults still run.
        Faults {
            drop_every: 701,
            duplicate_every: 503,
            hold_every: 307,
            drop_padding: 1,
        },
    );
    let mut subscription = client
        .add_subscription(&receive_channel, 7)
        .expect("a subscription");
    let mut publication = client
        .add_publication(&shim.channel(), 7)
        .expect("a publication");
    assert!(
        wait_until(Duration::from_secs(5), || publication.is_connected()),
        "the receiver's status message connected the publication"
    );
    assert!(wait_until(Duration::from_secs(5), || {
        subscription.poll(|_, _| {}, 0).expect("the poll applies");
        subscription.image_count() == 1
    }));
    let publisher = std::thread::spawn(move || {
        for i in 0..MESSAGES {
            let mut message = [0u8; BODY];
            message[..8].copy_from_slice(&i.to_le_bytes());
            offer(&mut publication, &message);
        }
        publication
    });
    assert_eq!(
        drain_in_order(&mut subscription, MESSAGES, Duration::from_secs(180)),
        MESSAGES
    );
    let publication = publisher.join().expect("the publisher");
    assert_eq!(
        read_counter(dir.path(), RECEIVER_FRAMES_RECEIVED),
        MESSAGES as i64 + 2,
        "every message once, and the two padding frames, one per term end"
    );
    assert!(
        read_counter(dir.path(), RECEIVER_GAPS_DETECTED) > 0,
        "loss was seen"
    );
    assert!(
        read_counter(dir.path(), SENDER_RETRANSMITS_SENT) > 0,
        "and mended"
    );
    // The image stands where the stream does: two terms and a thousand frames in, the
    // padding counted by its aligned length.
    let frames_in_third_term = MESSAGES - 2 * FRAMES_PER_TERM;
    let stream_position = publication.position().expect("the position");
    assert_eq!(
        subscription.images()[0].position(),
        stream_position,
        "the subscriber reached the publication's position"
    );
    assert_eq!(
        stream_position,
        (2 * TERM + frames_in_third_term * FRAME) as i64,
        "two terms of padding frames at their ends, then the third term"
    );
    let (forwarded, dropped, _, _, padding_dropped) = shim.stop();
    assert!(forwarded > 0 && dropped > 0);
    assert_eq!(
        padding_dropped, 1,
        "the first padding frame was lost and resent"
    );
    publication.close().expect("closes");
    subscription.close().expect("closes");
    client.close().expect("closes");
    driver.close().expect("the driver closes");
}

/// A receiver that subscribes after the sender's setup frames stopped asks for the
/// setup with a status message carrying the send-setup flag and gets its image
/// (`spec/udp.md`, "The receiver", step 1): the publication is added and connected to
/// a receiver by hand first, then the real subscription joins.
#[test]
fn a_late_subscription_elicits_the_setup_and_joins_the_stream() {
    let dir = TestDir::new("late");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("the driver starts");
    let mut client = Cavalry::connect_to(dir.path()).expect("connects");
    // A receiver by hand holds the port, answers the setup with a status message so
    // the sender stops sending setups, then gives the port up.
    let by_hand = UdpSocket::bind("127.0.0.1:0").expect("a socket");
    by_hand
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("a read timeout");
    let port = by_hand.local_addr().expect("bound").port();
    let channel = format!("cavalry:udp?endpoint=127.0.0.1:{port}");
    let mut publication = client.add_publication(&channel, 7).expect("a publication");
    let mut buffer = vec![0u8; 2048];
    let (length, from) = by_hand.recv_from(&mut buffer).expect("the setup frame");
    let setup = match cavalry_proto::udp::parse(&buffer[..length], spec::UDP_TERM_LENGTH) {
        Ok(cavalry_proto::udp::ControlFrame::Setup(setup)) => setup,
        other => panic!("expected a setup frame, got {other:?}"),
    };
    let message = cavalry_proto::udp::StatusMessage {
        flags: 0,
        session_id: setup.session_id,
        stream_id: 7,
        consumption_term_id: setup.active_term_id,
        consumption_term_offset: setup.term_offset,
        receiver_window_length: 131072,
        receiver_id: 1,
        group_tag: None,
    };
    by_hand
        .send_to(&message.to_bytes()[..message.frame_length()], from)
        .expect("sent");
    assert!(wait_until(Duration::from_secs(5), || publication.is_connected()));
    for i in 0..10u64 {
        let mut message = [0u8; 32];
        message[..8].copy_from_slice(&i.to_le_bytes());
        offer(&mut publication, &message);
    }
    drop(by_hand);
    // The real subscription binds the port: the sender's data and heartbeats elicit
    // the setup, and the image joins at the sender's position.
    let mut subscription = client
        .add_subscription(&channel, 7)
        .expect("a subscription");
    assert!(wait_until(Duration::from_secs(10), || {
        subscription.poll(|_, _| {}, 0).expect("the poll applies");
        subscription.image_count() == 1
    }));
    assert!(read_counter(dir.path(), RECEIVER_SETUPS_ELICITED) >= 1);
    for i in 10..20u64 {
        let mut message = [0u8; 32];
        message[..8].copy_from_slice(&i.to_le_bytes());
        offer(&mut publication, &message);
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut seen = Vec::new();
    while seen.len() < 10 {
        subscription
            .poll(
                |body, _| {
                    let mut sequence = [0u8; 8];
                    sequence.copy_from_slice(&body[..8]);
                    seen.push(u64::from_le_bytes(sequence));
                },
                64,
            )
            .expect("the poll reads");
        assert!(
            Instant::now() < deadline,
            "the late image delivered {seen:?}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(
        seen,
        (10..20).collect::<Vec<u64>>(),
        "from the join on, in order"
    );
    publication.close().expect("closes");
    subscription.close().expect("closes");
    client.close().expect("closes");
    driver.close().expect("the driver closes");
}

/// A recording of an image on the receiving driver replays byte for byte: a second
/// driver on a directory of its own receives the stream, records the image through
/// `start recording` on the `cavalry:udp` channel, and a replay of that recording
/// delivers the same bodies in the same order at the same positions (invariant 1).
#[test]
fn a_recording_of_an_image_on_the_receiving_driver_replays_byte_for_byte() {
    let dir = TestDir::new("record");
    let sending = Driver::start(dir.path().join("send"), dir.path().join("send-log"))
        .expect("the sending driver starts");
    let receiving = Driver::start(dir.path().join("recv"), dir.path().join("recv-log"))
        .expect("the receiving driver starts");
    let mut publisher = Cavalry::connect_to(dir.path().join("send")).expect("connects");
    let mut subscriber = Cavalry::connect_to(dir.path().join("recv")).expect("connects");
    let port = free_port();
    let channel = format!("cavalry:udp?endpoint=127.0.0.1:{port}");
    let mut subscription = subscriber
        .add_subscription(&channel, 9)
        .expect("a subscription");
    let mut publication = publisher
        .add_publication(&channel, 9)
        .expect("a publication");
    assert!(wait_until(Duration::from_secs(5), || publication.is_connected()));
    assert!(wait_until(Duration::from_secs(5), || {
        subscription.poll(|_, _| {}, 0).expect("the poll applies");
        subscription.image_count() == 1
    }));
    // The recording starts at the image's rebuild position, before any message.
    let recording = subscriber
        .start_recording(&channel, 9)
        .expect("a recording of the image");
    assert_eq!(recording.session_id(), publication.session_id());
    const MESSAGES: u64 = 500;
    let mut live: Vec<(i64, Vec<u8>)> = Vec::new();
    for i in 0..MESSAGES {
        let mut message = [0u8; 32];
        message[..8].copy_from_slice(&i.to_le_bytes());
        message[8..].copy_from_slice(&[i as u8; 24]);
        offer(&mut publication, &message);
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    while (live.len() as u64) < MESSAGES {
        let positions = subscription.images()[0].positions();
        subscription
            .poll(
                |body, header| {
                    live.push((
                        positions.position(header.term_id(), header.term_offset() as u32),
                        body.to_vec(),
                    ));
                },
                64,
            )
            .expect("the poll reads");
        assert!(
            Instant::now() < deadline,
            "the live stream stopped at {}",
            live.len()
        );
        std::thread::sleep(Duration::from_micros(100));
    }
    let end = subscription.images()[0].position();
    assert!(wait_until(Duration::from_secs(10), || {
        recording.synced_position() >= end
    }));
    recording.stop(&mut subscriber).expect("stops");
    // The replay, on the receiving driver, into a subscription on the replay stream.
    let mut replayed_subscription = subscriber
        .add_subscription("cavalry:ipc", 91)
        .expect("a subscription on the replay stream");
    let replay = subscriber
        .start_replay(recording.recording_id(), -1, -1, 91, "cavalry:ipc")
        .expect("a replay");
    let mut replayed: Vec<(i64, Vec<u8>)> = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        replayed_subscription
            .poll(|_, _| {}, 0)
            .expect("the poll applies");
        let positions = replayed_subscription
            .images()
            .iter()
            .find(|image| image.session_id() == replay.session_id())
            .map(|image| image.positions());
        if let Some(positions) = positions {
            replayed_subscription
                .poll(
                    |body, header| {
                        replayed.push((
                            positions.position(header.term_id(), header.term_offset() as u32),
                            body.to_vec(),
                        ));
                    },
                    64,
                )
                .expect("the poll reads");
        }
        if replay.ended().is_some() && (replayed.len() as u64) >= MESSAGES {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the replay delivered {} of {MESSAGES}",
            replayed.len()
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(
        replayed, live,
        "the replay is the live stream, byte for byte"
    );
    publication.close().expect("closes");
    subscription.close().expect("closes");
    replayed_subscription.close().expect("closes");
    publisher.close().expect("closes");
    subscriber.close().expect("closes");
    sending.close().expect("the sending driver closes");
    receiving.close().expect("the receiving driver closes");
}
