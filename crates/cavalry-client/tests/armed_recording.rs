//! An armed recording through the client, against two real drivers over loopback UDP:
//! `Cavalry::arm_recording` and the `ArmedRecording` it returns
//! (`spec/client-protocol.md`, "Commands", start recording; ADR 0059). The order is a
//! cluster follower's: subscribe, arm, and only then does the publisher's image arrive.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use cavalry_client::{Cavalry, ClientError, Offer, Publication, Recording};
use cavalry_driver::Driver;
use cavalry_log::catalog::CatalogReader;
use cavalry_proto::client_protocol::ErrorCode;

// This test's ports, from the one file that says how they are taken and why, which is
// `cavalry-raft`'s: `crates/cavalry-raft/tests/ports/mod.rs`.
#[path = "../../cavalry-raft/tests/ports/mod.rs"]
mod ports;
use ports::free_port;

struct TestDir(PathBuf);

impl TestDir {
    fn new(test: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "cavalry-client-armed-recording-{}-{test}",
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

fn wait(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "{what}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn offer(publication: &mut Publication, message: &[u8]) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match publication.offer(message) {
            Ok(_) => return,
            Err(Offer::NotConnected | Offer::BackPressured | Offer::AdminAction) => {
                assert!(Instant::now() < deadline, "the offer was refused for 10 s");
                std::thread::sleep(Duration::from_micros(50));
            }
            Err(other) => panic!("{other}"),
        }
    }
}

fn refused(outcome: Result<impl std::fmt::Debug, ClientError>, expected: ErrorCode) {
    match outcome {
        Err(ClientError::Refused { code, .. }) if code == Some(expected) => {}
        other => panic!("expected error code {}, got {other:?}", expected.code()),
    }
}

/// The recording a follower arms before its leader's first frame: armed while the
/// stream has a subscription and no image, started by the driver when the image
/// arrives, at the image's join position, and handed over by the first poll after. The
/// handle follows the recording from there as any `Recording` does.
#[test]
fn an_armed_recording_starts_with_the_image_and_is_handed_over_once() {
    let dir = TestDir::new("starts");
    let sending = Driver::start(dir.path().join("send"), dir.path().join("send-log"))
        .expect("the sending driver starts");
    let receiving = Driver::start(dir.path().join("recv"), dir.path().join("recv-log"))
        .expect("the receiving driver starts");
    let mut publisher = Cavalry::connect_to(dir.path().join("send")).expect("connects");
    let mut subscriber = Cavalry::connect_to(dir.path().join("recv")).expect("connects");
    let channel = format!("cavalry:udp?endpoint=127.0.0.1:{}", free_port());

    // Nothing to arm on: no publication, no image and no subscription is code 20.
    refused(
        subscriber.arm_recording(&channel, 9),
        ErrorCode::NoPublicationToRecord,
    );

    let mut subscription = subscriber
        .add_subscription(&channel, 9)
        .expect("a subscription");
    let mut armed = subscriber
        .arm_recording(&channel, 9)
        .expect("the driver arms the recording");
    assert_eq!((armed.stream_id(), armed.channel()), (9, channel.as_str()));
    assert!(armed.poll().expect("armed").is_none());
    assert!(!armed.is_done());
    // One armed recording per stream and endpoint: a second is code 22, from either
    // call, and the first stays armed.
    refused(
        subscriber.arm_recording(&channel, 9),
        ErrorCode::AlreadyRecording,
    );
    refused(
        subscriber.start_recording(&channel, 9),
        ErrorCode::AlreadyRecording,
    );
    assert!(armed.poll().expect("still armed").is_none());

    // The publisher starts. The image arrives and the recording with it.
    let mut publication = publisher
        .add_publication(&channel, 9)
        .expect("a publication");
    let mut recording: Option<Recording> = None;
    wait("the armed recording started with the image", || {
        subscription.poll(|_, _| {}, 0).expect("the poll applies");
        if recording.is_none() {
            recording = armed.poll().expect("the second answer is the recording");
        }
        recording.is_some() && subscription.image_count() == 1
    });
    let recording = recording.expect("handed over");
    assert_eq!(recording.session_id(), publication.session_id());
    assert_eq!(recording.stream_id(), 9);
    assert!(armed.is_done());
    assert!(armed.poll().expect("spent").is_none(), "handed over once");

    // Every frame of the image is in the recording: it began at the join position,
    // before the first message, and follows the image to its end.
    let start = CatalogReader::open(&dir.path().join("recv-log"))
        .expect("the catalog opens")
        .get(recording.recording_id())
        .expect("reads")
        .expect("the slot")
        .start_position();
    assert_eq!(start, 0, "the image's join position");
    const MESSAGES: usize = 200;
    for i in 0..MESSAGES {
        offer(&mut publication, &[i as u8; 32]);
    }
    let mut received = 0;
    wait("the subscription read every message", || {
        received += subscription.poll(|_, _| {}, 64).expect("the poll reads");
        received == MESSAGES
    });
    let end = subscription.images()[0].position();
    assert!(end > 0);
    wait("the recording synced the image's end", || {
        recording.synced_position() >= end
    });
    assert_eq!(recording.stop_position(), None);
    recording.stop(&mut subscriber).expect("stops");
    wait("the handle read its stop", || {
        recording.stop_position().is_some()
    });
    assert_eq!(recording.stop_position(), Some(end));

    drop(publication);
    drop(subscription);
    subscriber.close().expect("closes");
    publisher.close().expect("closes");
    receiving.close().expect("the receiving driver closes");
    sending.close().expect("the sending driver closes");
}

/// The two ways an arming call learns there will be no recording to hold. A
/// `start_recording` that the driver answers by arming returns `recording-armed`, since
/// it returns a started recording or nothing, and the driver holds the armed recording
/// all the same. An armed recording whose stream loses its last subscription is
/// dropped by the driver, and the handle's poll returns the driver's error, code 20,
/// once.
#[test]
fn an_armed_recording_is_named_to_start_recording_and_its_drop_reaches_the_handle() {
    let dir = TestDir::new("dropped");
    let driver = Driver::start(dir.path().join("driver"), dir.path().join("log"))
        .expect("the driver starts");
    let mut client = Cavalry::connect_to(dir.path().join("driver")).expect("connects");
    let channel = format!("cavalry:udp?endpoint=127.0.0.1:{}", free_port());

    let first = client
        .add_subscription(&channel, 21)
        .expect("a subscription");
    match client.start_recording(&channel, 21) {
        Err(error @ ClientError::RecordingArmed { stream_id: 21 }) => {
            assert_eq!(error.reason(), "recording-armed");
            let text = error.to_string();
            assert!(text.contains("Cavalry::arm_recording"), "{text}");
            assert!(
                text.contains("docs/reference/client-protocol.md#commands"),
                "{text}"
            );
        }
        other => panic!("expected recording-armed, got {other:?}"),
    }
    // The driver holds it armed, with no handle on it here.
    refused(
        client.arm_recording(&channel, 21),
        ErrorCode::AlreadyRecording,
    );
    drop(first);

    let second = client
        .add_subscription(&channel, 22)
        .expect("a subscription");
    let mut armed = client
        .arm_recording(&channel, 22)
        .expect("the driver arms the recording");
    assert!(armed.poll().expect("armed").is_none());
    // The stream's last subscription goes: the driver drops the armed recording and
    // says so under the arming command's correlation id.
    second.close().expect("the subscription closes");
    let mut outcome = None;
    wait(
        "the driver's second answer reached the handle",
        || match armed.poll() {
            Ok(None) => false,
            other => {
                outcome = Some(other);
                true
            }
        },
    );
    refused(
        outcome.expect("an answer"),
        ErrorCode::NoPublicationToRecord,
    );
    assert!(armed.is_done());
    assert!(armed.poll().expect("spent").is_none(), "the error once");

    client.close().expect("closes");
    driver.close().expect("the driver closes");
}
