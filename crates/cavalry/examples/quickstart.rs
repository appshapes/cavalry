//! The ten-minute path's program (README, "The ten-minute path"; `just quickstart`): one
//! process attaches to the driver running on this host, records the stream it is about
//! to publish on, publishes 1,000 messages on it and reads them back through a
//! subscription, each once and in order, stops the recording and prints what it saw and
//! the recording's id, so that `cavalry replay` plays the messages back from the durable
//! log (card 95, the slice's acceptance test). `CAVALRY_DIR` names the driver's
//! directory when it is not the default (`docs/reference/config.md`).

use std::process::ExitCode;
use std::time::Instant;

use cavalry::{Cavalry, Offer};

const STREAM_ID: i32 = 1001;
const MESSAGES: u64 = 1_000;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("quickstart: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let started = Instant::now();
    let mut client = Cavalry::connect()?;
    println!(
        "quickstart: client {} attached to the driver at {}",
        client.client_id(),
        client.dir().display()
    );
    let mut subscription = client.add_subscription("cavalry:ipc", STREAM_ID)?;
    let mut publication = client.add_publication("cavalry:ipc", STREAM_ID)?;
    // Record the stream from here, position 0 of a publication that has published
    // nothing yet: the driver's recorder writes every committed frame to the durable
    // log under the log directory, and the recording outlives this process.
    let recording = client.start_recording("cavalry:ipc", STREAM_ID)?;

    let mut sent: u64 = 0;
    let mut next: u64 = 0;
    while next < MESSAGES {
        if sent < MESSAGES {
            // Claim a frame, write the message in place, commit: nothing is copied and
            // nothing is allocated. Until the subscription's image joins, the driver
            // answers `not-connected`; a full window is `back-pressured`; both mean
            // "poll and try again".
            match publication.try_claim(8) {
                Ok(mut claim) => {
                    claim.body().copy_from_slice(&sent.to_le_bytes());
                    claim.commit();
                    sent += 1;
                }
                Err(Offer::NotConnected | Offer::BackPressured | Offer::AdminAction) => {}
                Err(other) => return Err(other.into()),
            }
        }
        // Each fragment comes to the handler as a view into the log, with its header.
        subscription.poll(
            |body, _header| {
                let value = u64::from_le_bytes(body.try_into().expect("8 bytes"));
                assert_eq!(value, next, "a message out of order or twice");
                next += 1;
            },
            64,
        )?;
    }
    println!(
        "quickstart: {MESSAGES} messages published on stream {STREAM_ID} by session {} and \
         received once, in order, in {:.1} ms",
        publication.session_id(),
        started.elapsed().as_secs_f64() * 1e3
    );
    recording.stop(&mut client)?;
    println!(
        "quickstart: recording {} of stream {STREAM_ID} stopped; cavalry replay --recording {} \
         --from 0 plays it back",
        recording.recording_id(),
        recording.recording_id()
    );
    publication.close()?;
    subscription.close()?;
    client.close()?;
    Ok(())
}
