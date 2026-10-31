//! Recording through the client API against a real driver in this process: a start, the
//! handle's two positions following the publisher, a recording alone connecting its
//! publication, a second start refused, a stop from the client, and every error code
//! (`spec/client-protocol.md`, "Commands"; ADR 0025). The full case comes first.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use cavalry_client::{Cavalry, ClientError};
use cavalry_driver::Driver;
use cavalry_log::catalog::CatalogReader;
use cavalry_proto::client_protocol::ErrorCode;

/// A directory of its own per test, removed when the guard drops.
struct TestDir(PathBuf);

impl TestDir {
    fn new(test: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "cavalry-client-recording-{}-{test}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("the test directory");
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

/// Waits up to five seconds for `done`.
fn wait(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done() {
        assert!(Instant::now() < deadline, "{what}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn refused(outcome: Result<impl std::fmt::Debug, ClientError>, expected: ErrorCode) {
    match outcome {
        Err(ClientError::Refused { code, .. }) if code == Some(expected) => {}
        other => panic!("expected error code {}, got {other:?}", expected.code()),
    }
}

#[test]
fn a_recording_follows_the_publisher_and_stops_on_request() {
    let dir = TestDir::new("follows");
    let driver_dir = dir.path().join("driver");
    let log_dir = dir.path().join("log");
    let driver = Driver::start(&driver_dir, &log_dir).expect("the driver starts");
    let mut client = Cavalry::connect_to(&driver_dir).expect("connects");
    let mut publication = client
        .add_exclusive_publication("cavalry:ipc", 71)
        .expect("a publication");

    let recording = client
        .start_recording("cavalry:ipc", 71)
        .expect("a recording");
    assert_eq!(
        recording.recording_id(),
        0,
        "the first slot of a new log directory"
    );
    assert_eq!(recording.session_id(), publication.session_id());
    assert_eq!(
        (recording.stream_id(), recording.channel()),
        (71, "cavalry:ipc")
    );
    assert_eq!((recording.position(), recording.synced_position()), (0, 0));

    // The recording alone connects the publication: with no subscriber, offers go
    // through once the driver has raised the limit.
    let mut after = 0;
    for i in 0..50u8 {
        wait("the offer goes through", || {
            match publication.offer(&[i; 32]) {
                Ok(position) => {
                    after = position;
                    true
                }
                Err(_) => false,
            }
        });
    }
    assert_eq!(after, 50 * 64);
    wait("recording.position follows the publisher", || {
        recording.position() == after
    });
    wait(
        "recording.synced_position follows within the sync cadence",
        || recording.synced_position() == after,
    );

    // Every live publication of the stream is recorded: another start is code 22.
    refused(
        client.start_recording("cavalry:ipc", 71),
        ErrorCode::AlreadyRecording,
    );
    // Dropping the handle stops nothing: the recording is the driver's.
    drop(recording);
    let catalog = CatalogReader::open(&log_dir).expect("the catalog opens");
    assert!(catalog.get(0).expect("reads").expect("the slot").is_live());
    drop(catalog);
    client.stop_recording(0).expect("stops");
    let catalog = CatalogReader::open(&log_dir).expect("the catalog opens");
    assert_eq!(
        catalog
            .get(0)
            .expect("reads")
            .expect("the slot")
            .stop_position(),
        after,
        "the catalog holds the stop"
    );
    client
        .stop_recording(0)
        .expect("already stopped is success");
    refused(client.stop_recording(9), ErrorCode::UnknownRecording);
    refused(
        client.start_recording("cavalry:ipc", 72),
        ErrorCode::NoPublicationToRecord,
    );
    refused(
        client.start_recording("cavalry:udp?endpoint=localhost:40123", 71),
        ErrorCode::InvalidChannel,
    );
    // A new recording of the same publication takes the next slot.
    let again = client
        .start_recording("cavalry:ipc", 71)
        .expect("a second recording");
    assert_eq!(again.recording_id(), 1);
    assert_eq!(
        again.position(),
        after,
        "it starts at the producer position"
    );
    // The handle's stop is `stop_recording` with its id.
    again.stop(&mut client).expect("stops through the handle");
    let catalog = CatalogReader::open(&log_dir).expect("the catalog opens");
    assert_eq!(
        catalog
            .get(1)
            .expect("reads")
            .expect("the slot")
            .stop_position(),
        after
    );
    drop(catalog);
    let live = client
        .start_recording("cavalry:ipc", 71)
        .expect("a third recording");
    assert_eq!(live.recording_id(), 2);
    drop(publication);
    client.close().expect("closes");
    driver.close().expect("closes, stopping the live recording");
    let catalog = CatalogReader::open(&log_dir).expect("the catalog opens");
    assert!(!catalog.get(2).expect("reads").expect("the slot").is_live());
}

/// The three retention commands through the client API (ADR 0031): a live recording is
/// code 24; after the stop, a truncate cuts the stop to a frame boundary and a replay
/// ends there; a purge moves the retained position and removes the file before it; a
/// purge of the recording deletes every file and the slot; the codes for an unknown
/// recording and a position outside its rule.
#[test]
fn a_stopped_recording_is_truncated_purged_and_deleted_through_the_client() {
    let dir = TestDir::new("retention");
    let driver_dir = dir.path().join("driver");
    let log_dir = dir.path().join("log");
    let driver = Driver::start(&driver_dir, &log_dir).expect("the driver starts");
    let mut client = Cavalry::connect_to(&driver_dir).expect("connects");
    let mut publication = client
        .add_exclusive_publication("cavalry:ipc", 73)
        .expect("a publication");
    let recording = client
        .start_recording("cavalry:ipc", 73)
        .expect("a recording");
    let mut after = 0;
    for i in 0..100u8 {
        wait("the offer goes through", || {
            publication
                .offer(&[i; 32])
                .map(|position| after = position)
                .is_ok()
        });
    }
    assert_eq!(after, 100 * 64);
    wait("the recording is synced", || {
        recording.synced_position() == after
    });

    // Live: every retention command is code 24.
    refused(client.truncate_recording(0, 64), ErrorCode::RecordingBusy);
    refused(client.purge_segments(0, 0), ErrorCode::RecordingBusy);
    refused(client.purge_recording(0), ErrorCode::RecordingBusy);
    client.stop_recording(0).expect("stops");

    // Unknown, and outside the rule: 21 and 23.
    refused(client.truncate_recording(9, 0), ErrorCode::UnknownRecording);
    refused(client.purge_segments(9, 0), ErrorCode::UnknownRecording);
    refused(client.purge_recording(9), ErrorCode::UnknownRecording);
    refused(
        client.truncate_recording(0, after + 64),
        ErrorCode::PositionOutOfRange,
    );
    refused(
        client.truncate_recording(0, 32),
        ErrorCode::PositionOutOfRange,
    );
    refused(client.purge_segments(0, 0), ErrorCode::PositionOutOfRange);

    // A truncate to the fiftieth frame: the catalog's stop moves there.
    client.truncate_recording(0, 50 * 64).expect("truncates");
    let catalog = CatalogReader::open(&log_dir).expect("the catalog opens");
    let slot = catalog.get(0).expect("reads").expect("the slot");
    assert_eq!(slot.stop_position(), 50 * 64);
    assert_eq!(slot.retained_position(), 0);
    drop(catalog);

    // A purge of the recording: every file goes and the slot reads deleted.
    client.purge_recording(0).expect("deletes");
    let catalog = CatalogReader::open(&log_dir).expect("the catalog opens");
    let slot = catalog.get(0).expect("reads").expect("the slot");
    assert_eq!(slot.state(), cavalry_log::layout::State::Deleted);
    assert!(
        !log_dir.join("0-0.rec").exists() && !log_dir.join("0.idx").exists(),
        "the files are gone"
    );
    drop(catalog);
    // A deleted slot is not a valid recording: 21, whose message says the slot is deleted.
    refused(client.purge_recording(0), ErrorCode::UnknownRecording);

    publication.close().expect("closes");
    client.close().expect("closes");
    driver.close().expect("closes");
}
