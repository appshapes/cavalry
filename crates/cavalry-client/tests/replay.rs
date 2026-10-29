//! Replay through the client API against a real driver in this process: a recording
//! made and stopped, then played into a subscription whose image's positions are the
//! recording's; the handle's position and end; a replay following a live recording; a
//! stop from the client; dropping the handle stopping nothing; and every error code
//! (`spec/client-protocol.md`, "Commands"; ADR 0030). The full case comes first.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use cavalry_client::{
    Cavalry, ClientError, FOLLOW_LENGTH, Flags, NULL_LENGTH, NULL_POSITION, Replay,
    ReplayStopReason, Subscription,
};
use cavalry_driver::Driver;
use cavalry_proto::client_protocol::ErrorCode;

/// A directory of its own per test, removed when the guard drops.
struct TestDir(PathBuf);

impl TestDir {
    fn new(test: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "cavalry-client-replay-{}-{test}",
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

/// A driver, a client, and recording 0 of `count` messages on stream 71, each 32 bytes
/// numbered in its first eight, stopped at its end.
fn recorded(dir: &TestDir, count: u8) -> (Driver, Cavalry, i64) {
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
    let mut after = 0;
    for i in 0..count {
        let mut message = [0u8; 32];
        message[..8].copy_from_slice(&i64::from(i).to_le_bytes());
        wait("the offer goes through", || {
            match publication.offer(&message) {
                Ok(position) => {
                    after = position;
                    true
                }
                Err(_) => false,
            }
        });
    }
    wait("the recording is synced", || {
        recording.synced_position() == after
    });
    client.stop_recording(0).expect("stops");
    publication.close().expect("closes");
    (driver, client, after)
}

/// What a drained replay delivered: every frame's position and body in order, the end
/// position and the reason.
type Drained = (Vec<(i64, Vec<u8>)>, i64, Option<ReplayStopReason>);

/// Polls `subscription` until `replay` has ended and its image was read to the end;
/// returns every frame's position and body in order.
fn drain(subscription: &mut Subscription, replay: &Replay) -> Drained {
    let mut frames = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let mut batch = Vec::new();
        let delivered = subscription
            .poll(
                |body, header| batch.push((header.term_offset(), header.term_id(), body.to_vec())),
                64,
            )
            .expect("the poll reads");
        for (term_offset, term_id, body) in batch {
            let image = subscription.images().first().expect("the image");
            let header = cavalry_client::DataFrameHeader::new(
                cavalry_client::FrameType::Data,
                Flags::UNFRAGMENTED,
                32 + body.len(),
            )
            .expect("a header")
            .with_term_offset(term_offset)
            .with_term_id(term_id);
            frames.push((image.position_of(&header), body));
        }
        let read_to = subscription
            .images()
            .first()
            .map_or(i64::MAX, |image| image.position());
        if let Some((end, reason)) = replay.ended()
            && (read_to >= end || subscription.images().is_empty())
        {
            return (frames, end, reason);
        }
        assert!(Instant::now() < deadline, "the replay did not end");
        if delivered == 0 {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

/// The full case (invariant 1): a recording of 100 messages played from its start to
/// its extent into a subscription added first; `replay ready` fills the handle; the
/// image's positions are the recording's; the handle's position reaches the end and
/// `ended` says reason 0 at the stop position; the image ends there.
#[test]
fn a_replay_plays_a_recording_into_a_subscription_at_the_recordings_positions() {
    let dir = TestDir::new("plays");
    let (driver, mut client, end) = recorded(&dir, 100);
    assert_eq!(end, 6400);
    let mut subscription = client
        .add_subscription("cavalry:ipc", 2071)
        .expect("a subscription");
    let replay = client
        .start_replay(0, NULL_POSITION, NULL_LENGTH, 2071, "cavalry:ipc")
        .expect("a replay");
    assert_eq!(replay.recording_id(), 0);
    assert_eq!(replay.start_position(), 0);
    assert_eq!(
        (replay.stream_id(), replay.channel()),
        (2071, "cavalry:ipc")
    );
    assert!(replay.replay_session_id() > 0);
    assert_eq!(replay.ended(), None);

    let (frames, ended_at, reason) = drain(&mut subscription, &replay);
    assert_eq!((ended_at, reason), (6400, Some(ReplayStopReason::Ended)));
    assert_eq!(frames.len(), 100);
    for (i, (position, body)) in frames.iter().enumerate() {
        assert_eq!(
            *position,
            i as i64 * 64,
            "frame {i}: the recording's position"
        );
        assert_eq!(&body[..8], &(i as i64).to_le_bytes(), "frame {i}: body");
    }
    assert_eq!(replay.position(), 6400);
    let image = subscription.images().first().expect("the image");
    assert_eq!(image.session_id(), replay.session_id());
    assert_eq!(image.correlation_id(), replay.replay_session_id());
    assert!(image.is_end_of_stream());
    assert_eq!(image.position(), 6400);
    println!("{replay:?}");

    // A stop of an ended replay is success with nothing done.
    replay
        .stop(&mut client)
        .expect("a stopped replay's stop succeeds");
    client.close().expect("closes");
    driver.close().expect("closes");
}

/// A replay of a part, from a position to a length; one that follows the recording
/// live, moving as the recorder syncs and ending when it stops; and one whose handle
/// is dropped, which stops nothing.
#[test]
fn a_replay_takes_a_position_and_a_length_or_follows_a_live_recording() {
    let dir = TestDir::new("follows");
    let driver_dir = dir.path().join("driver");
    let log_dir = dir.path().join("log");
    let driver = Driver::start(&driver_dir, &log_dir).expect("the driver starts");
    let mut client = Cavalry::connect_to(&driver_dir).expect("connects");
    let mut publication = client
        .add_exclusive_publication("cavalry:ipc", 72)
        .expect("a publication");
    let recording = client
        .start_recording("cavalry:ipc", 72)
        .expect("a recording");
    let offer = |publication: &mut cavalry_client::Publication, from: u8, count: u8| {
        let mut after = 0;
        for i in from..from + count {
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
    };
    let after = offer(&mut publication, 0, 10);
    wait("synced", || recording.synced_position() == after);

    // From frame 2 for three frames: positions 128 to 320.
    let mut part = client
        .add_subscription("cavalry:ipc", 2072)
        .expect("a subscription");
    let replay = client
        .start_replay(0, 128, 192, 2072, "cavalry:ipc")
        .expect("a replay");
    assert_eq!(replay.start_position(), 128);
    let (frames, ended_at, reason) = drain(&mut part, &replay);
    assert_eq!((ended_at, reason), (320, Some(ReplayStopReason::Ended)));
    assert_eq!(
        frames
            .iter()
            .map(|(position, _)| *position)
            .collect::<Vec<_>>(),
        vec![128, 192, 256]
    );
    assert_eq!(frames[0].1, vec![2u8; 32]);
    drop(part);

    // A follow: the synced bytes first, more as they are synced, the end at the stop.
    let mut follow = client
        .add_subscription("cavalry:ipc", 2073)
        .expect("a subscription");
    let replay = client
        .start_replay(0, NULL_POSITION, FOLLOW_LENGTH, 2073, "cavalry:ipc")
        .expect("a replay");
    wait("the synced bytes are played", || replay.position() == after);
    let later = offer(&mut publication, 10, 10);
    wait("the new bytes are synced and played", || {
        replay.position() == later
    });
    assert_eq!(
        replay.ended(),
        None,
        "a follow goes on while the recording lives"
    );
    // The handle dropped stops nothing: another handle on the same replay sees it end.
    let replay_session_id = replay.replay_session_id();
    drop(replay);
    client.stop_recording(0).expect("stops");
    // The image ends at the stop, and goes once the driver sees it was read to the end,
    // which can be before this thread looks: either is the end.
    let mut delivered = 0;
    wait(
        "the follow ends at the recording's stop and the image ends",
        || {
            delivered += follow.poll(|_, _| {}, 64).expect("the poll reads");
            delivered == 20
                && follow
                    .images()
                    .first()
                    .is_none_or(|image| image.is_end_of_stream() && image.position() == later)
        },
    );
    assert_eq!(delivered, 20);
    client
        .stop_replay(replay_session_id)
        .expect("a stop of an ended replay succeeds");
    publication.close().expect("closes");
    client.close().expect("closes");
    driver.close().expect("closes");
}

/// `stop replay` from the client ends a replay where it is, reason 1.
#[test]
fn a_replay_stops_on_request_at_the_position_reached() {
    let dir = TestDir::new("stops");
    let (driver, mut client, _) = recorded(&dir, 100);
    // No subscription yet: the replay does not move, and the stop finds it at the start.
    let replay = client
        .start_replay(0, NULL_POSITION, NULL_LENGTH, 2074, "cavalry:ipc")
        .expect("a replay");
    assert_eq!(replay.position(), 0);
    replay.stop(&mut client).expect("stops");
    wait("replay stopped arrives", || replay.ended().is_some());
    assert_eq!(replay.ended(), Some((0, Some(ReplayStopReason::Stopped))));
    client.close().expect("closes");
    driver.close().expect("closes");
}

/// Every error code of `start replay` through the client: a recording the catalog
/// does not hold (21), a position out of range (23), a length below −1 (7), and a
/// channel that is not `cavalry:ipc` (1).
#[test]
fn start_replay_errors_carry_the_drivers_codes() {
    let dir = TestDir::new("errors");
    let (driver, mut client, end) = recorded(&dir, 4);
    refused(
        client.start_replay(7, NULL_POSITION, NULL_LENGTH, 2075, "cavalry:ipc"),
        ErrorCode::UnknownRecording,
    );
    refused(
        client.start_replay(0, end + 32, NULL_LENGTH, 2075, "cavalry:ipc"),
        ErrorCode::PositionOutOfRange,
    );
    refused(
        client.start_replay(0, 40, NULL_LENGTH, 2075, "cavalry:ipc"),
        ErrorCode::PositionOutOfRange,
    );
    refused(
        client.start_replay(0, NULL_POSITION, -2, 2075, "cavalry:ipc"),
        ErrorCode::MalformedCommand,
    );
    refused(
        client.start_replay(
            0,
            NULL_POSITION,
            NULL_LENGTH,
            2075,
            "cavalry:udp?endpoint=x",
        ),
        ErrorCode::InvalidChannel,
    );
    match client.start_replay(0, 100, NULL_LENGTH, 2075, "cavalry:ipc") {
        Err(ClientError::Refused { message, .. }) => {
            assert!(message.contains("from 0 to 256"), "{message}");
            assert!(
                message.ends_with("docs/reference/client-protocol.md#error-codes"),
                "{message}"
            );
        }
        other => panic!("{other:?}"),
    }
    client.close().expect("closes");
    driver.close().expect("closes");
}
