//! Recording through the client API against a real driver in this process: a start, the
//! handle's two positions following the publisher, a recording alone connecting its
//! publication, a second start refused, a stop from the client, and every error code
//! (`spec/client-protocol.md`, "Commands"; ADR 0025). The full case comes first.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use cavalry_client::{Cavalry, ClientError};
use cavalry_driver::{Driver, spec};
use cavalry_log::catalog::CatalogReader;
use cavalry_proto::client_protocol::ErrorCode;
use cavalry_shm::counters::generated::{RECORDING_POSITION, RECORDING_SYNCED_POSITION};

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
    // What the catalog is stamped with is the epoch clock, which a later run reads; the
    // recorder measures its intervals on its nano clock, whose readings are no time of
    // day (ADR 0109).
    let slot = catalog.get(0).expect("reads").expect("the slot");
    let wall = cavalry_shm::clock::epoch_time();
    for stamp in [slot.start_timestamp(), slot.stop_timestamp()] {
        assert!(
            (wall - stamp).abs() < 60_000_000_000,
            "{stamp} is not within a minute of the host's clock, {wall}"
        );
    }
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
    // A cavalry:udp channel records what sends or is received on it (ADR 0042):
    // nothing does, so code 20.
    refused(
        client.start_recording("cavalry:udp?endpoint=localhost:40123", 71),
        ErrorCode::NoPublicationToRecord,
    );
    refused(
        client.start_recording("cavalry:udp?endpoint=localhost", 71),
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

/// Offers `count` messages of 32 bytes to `publication` and returns the position after
/// the last.
fn offer(publication: &mut cavalry_client::Publication, count: u8) -> i64 {
    let mut after = 0;
    for i in 0..count {
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
    after
}

/// `sync recording` through the client (`spec/client-protocol.md`, "Commands"; ADR
/// 0069). Twenty rounds of ten messages: once the recorder has written a round,
/// `Recording::sync` returns with the synced position at the position, with no wait
/// after the call. The recorder's own sync runs once a second, so it could make a
/// round or two pass by itself and never twenty in a row. Then the answers: an id the
/// catalog does not hold is code 21, and a recording that has stopped is success with
/// nothing done, through either call.
#[test]
fn a_sync_on_request_covers_what_the_recorder_has_written_when_it_returns() {
    let dir = TestDir::new("sync");
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
    recording
        .sync(&mut client)
        .expect("nothing unsynced is success");
    assert_eq!(recording.synced_position(), 0);

    for round in 1..=20 {
        let after = offer(&mut publication, 10);
        assert_eq!(after, round * 10 * 64);
        wait("recording.position follows the publisher", || {
            recording.position() == after
        });
        recording.sync(&mut client).expect("syncs");
        assert_eq!(
            recording.synced_position(),
            after,
            "round {round}: the answer came before the sync covered what was written"
        );
    }

    refused(client.sync_recording(9), ErrorCode::UnknownRecording);
    refused(client.sync_recording(-1), ErrorCode::UnknownRecording);
    recording.stop(&mut client).expect("stops");
    wait("the handle reads the stop", || {
        recording.stop_position().is_some()
    });
    recording
        .sync(&mut client)
        .expect("a stopped recording is success with nothing done");
    client
        .sync_recording(recording.recording_id())
        .expect("by its id too");
    recording
        .request_sync(&mut client)
        .expect("nothing is sent for a recording the handle has seen stop");
    assert_eq!(recording.synced_position(), 20 * 10 * 64);
    drop(publication);
    client.close().expect("closes");
    driver.close().expect("closes");
}

/// `Recording::request_sync` writes the command and does not wait, and the driver
/// acts on it (ADR 0069). Two recordings, one that takes the messages and one that
/// stays idle. In each of twenty rounds the first is asked to sync without waiting,
/// and then the second is synced with the call that waits: the recorder handles its
/// requests in the order the conductor handed them over, so when the second call
/// returns, the first request has been handled, and the first recording's synced
/// position is at its position. Nothing here waits on the clock, and the recorder's
/// own sync, once a second, could not make twenty rounds in a row pass.
#[test]
fn a_sync_requested_without_waiting_is_handled_by_the_recorder_in_order() {
    let dir = TestDir::new("request-sync");
    let driver_dir = dir.path().join("driver");
    let log_dir = dir.path().join("log");
    let driver = Driver::start(&driver_dir, &log_dir).expect("the driver starts");
    let mut client = Cavalry::connect_to(&driver_dir).expect("connects");
    let mut publication = client
        .add_exclusive_publication("cavalry:ipc", 71)
        .expect("a publication");
    let idle_publication = client
        .add_exclusive_publication("cavalry:ipc", 72)
        .expect("a second publication");
    let recording = client
        .start_recording("cavalry:ipc", 71)
        .expect("a recording");
    let idle = client
        .start_recording("cavalry:ipc", 72)
        .expect("a second recording");

    for round in 1..=20 {
        let after = offer(&mut publication, 10);
        wait("recording.position follows the publisher", || {
            recording.position() == after
        });
        recording
            .request_sync(&mut client)
            .expect("the command is written");
        idle.sync(&mut client)
            .expect("the request behind it is answered");
        assert_eq!(
            recording.synced_position(),
            after,
            "round {round}: the request without a wait was not handled"
        );
    }
    assert_eq!(idle.synced_position(), 0, "nothing was published to it");

    drop(publication);
    drop(idle_publication);
    client.close().expect("closes");
    driver.close().expect("closes");
}

/// The record of the counters file that holds `type_id` for recording `recording_id`,
/// whose key is the recording id (`spec/client-protocol.md`, "Counters").
fn recording_counter(client: &Cavalry, type_id: i32, recording_id: i64) -> u32 {
    let counters = client.counters();
    (0..counters.capacity())
        .find(|&id| {
            counters.record(id).ok().flatten().is_some_and(|record| {
                record.type_id() == type_id && record.key()[..8] == recording_id.to_le_bytes()
            })
        })
        .expect("the recording's counter is allocated")
}

/// The driver frees a stopped recording's counters and gives their records to other
/// counters after the counter reuse timeout (`spec/counters-file.md`, "Metadata
/// record"). A handle that went on reading them would report another counter's value
/// as the recording's position, so the handle learns the stop from `recording stopped`
/// and reads no counter after it.
#[test]
fn a_stopped_recordings_handle_reads_its_stop_and_never_a_counter_given_away() {
    let dir = TestDir::new("stop");
    let driver_dir = dir.path().join("driver");
    let log_dir = dir.path().join("log");
    let driver = Driver::start(&driver_dir, &log_dir).expect("the driver starts");
    let mut client = Cavalry::connect_to(&driver_dir).expect("connects");
    let mut publication = client
        .add_exclusive_publication("cavalry:ipc", 81)
        .expect("a publication");
    let recording = client
        .start_recording("cavalry:ipc", 81)
        .expect("a recording");
    assert_eq!(recording.stop_position(), None, "live");
    let mut after = 0;
    for i in 0..10u8 {
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
    wait("recording.position follows the publisher", || {
        recording.position() == after
    });
    let position_record = recording_counter(&client, RECORDING_POSITION.0, 0);
    let synced_record = recording_counter(&client, RECORDING_SYNCED_POSITION.0, 0);

    // The stop: the handle learns it from the event, and both positions are the stop
    // position from then on, what the catalog holds.
    recording.stop(&mut client).expect("stops");
    wait("the handle learns the stop", || {
        recording.stop_position().is_some()
    });
    assert_eq!(recording.stop_position(), Some(after));
    assert_eq!(
        (recording.position(), recording.synced_position()),
        (after, after)
    );

    // Past the reuse timeout the driver gives the freed records to the next counters
    // it allocates, the lowest free record first (`CountersFile::allocate`): a second
    // publication's and its recording's, which allocate at least the counters the stop
    // freed, so both records are taken, whichever of the new counters takes each.
    std::thread::sleep(spec::COUNTER_REUSE_TIMEOUT + Duration::from_millis(200));
    let mut other = client
        .add_exclusive_publication("cavalry:ipc", 82)
        .expect("a second publication");
    let second = client
        .start_recording("cavalry:ipc", 82)
        .expect("a second recording");
    let mut moved = 0;
    for i in 0..40u8 {
        wait("the offer goes through", || match other.offer(&[i; 32]) {
            Ok(position) => {
                moved = position;
                true
            }
            Err(_) => false,
        });
    }
    wait("the second recording follows its publisher", || {
        second.synced_position() == moved
    });
    assert_ne!(moved, after);
    // The first recording's two records are allocated to other counters now: allocated
    // again, not merely freed, and not the first recording's.
    let counters = client.counters();
    for (record, type_id) in [
        (position_record, RECORDING_POSITION.0),
        (synced_record, RECORDING_SYNCED_POSITION.0),
    ] {
        let Some(now) = counters.record(record).expect("the record reads") else {
            panic!("record {record} was freed and not allocated again");
        };
        assert!(
            !(now.type_id() == type_id && now.key()[..8] == 0i64.to_le_bytes()),
            "record {record} is still the first recording's"
        );
        // What the handle read before it followed the stop: the record's value now,
        // another counter's, which is not where the first recording stopped.
        let value = match counters.read(record) {
            Ok(value) => value,
            Err(error) => panic!("record {record} holds no counter: {error}"),
        };
        assert_ne!(
            value, after,
            "record {record} holds the first recording's stop"
        );
    }
    // And the handle still says where its recording stopped, whatever they hold.
    assert_eq!(
        (recording.position(), recording.synced_position()),
        (after, after)
    );
    assert_eq!(recording.stop_position(), Some(after));
    // A recording that stops when its publication closes is learnt the same way.
    drop(other);
    wait("the second recording stops at its end of stream", || {
        second.stop_position() == Some(moved)
    });
    assert_eq!(second.position(), moved);
    drop(publication);
    client.close().expect("closes");
    driver.close().expect("closes");
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
