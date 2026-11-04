//! The ten-minute path's program (README, "The ten-minute path"; `just quickstart`): one
//! process attaches to the driver running on this host, records the stream it is about
//! to publish on, publishes 1,000 messages on it and reads them back through a
//! subscription, each once and in order, waits until the recorder has written up to the
//! publication's position, stops the recording and prints what it saw and the
//! recording's id, so that `cavalry replay` plays the messages back from the durable
//! log (card 95, the second slice's acceptance test); then publishes the same 1,000
//! messages on the same stream over `cavalry:udp`, a publication and a subscription
//! whose endpoint is a port on this host, through the same API, so that every message
//! goes through the driver's sender, the loopback and the driver's receiver, and reads
//! them back once and in order (card 182, the third slice's acceptance test; ADR
//! 0044). The wait is the client's: `stop recording` stops at the position the recorder
//! has written (`spec/client-protocol.md`), and the recorder is a thread of the
//! driver's, so a stop sent the moment the subscriber has read the last message would
//! race it. In Aeron, a client waits the same way before `stopRecording`. `CAVALRY_DIR`
//! names the driver's directory when it is not the default
//! (`docs/reference/config.md`).

use std::process::ExitCode;
use std::time::{Duration, Instant};

use cavalry::{Cavalry, Offer, Publication, Subscription};

const STREAM_ID: i32 = 1001;
const MESSAGES: u64 = 1_000;
/// The UDP step's channel: a port of this host, the one the repository's tests use.
/// Between two hosts only the host changes, to the subscriber's.
const UDP_CHANNEL: &str = "cavalry:udp?endpoint=127.0.0.1:40123";

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

    publish_and_read(&mut publication, &mut subscription)?;
    println!(
        "quickstart: {MESSAGES} messages published on stream {STREAM_ID} by session {} and \
         received once, in order, in {:.1} ms",
        publication.session_id(),
        started.elapsed().as_secs_f64() * 1e3
    );
    // The recorder is a thread of the driver's, behind the publisher by up to the
    // publication window: a stop stops at what it has written so far. Wait until that
    // is the publication's position, so that the recording holds every message.
    let end = publication.position()?;
    let deadline = Instant::now() + Duration::from_secs(10);
    while recording.position() < end {
        if Instant::now() > deadline {
            return Err(format!(
                "the recorder wrote up to position {} of {end} in 10 s",
                recording.position()
            )
            .into());
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    recording.stop(&mut client)?;
    println!(
        "quickstart: recording {} of stream {STREAM_ID} stopped; cavalry replay --recording {} \
         --from 0 plays it back",
        recording.recording_id(),
        recording.recording_id()
    );
    publication.close()?;
    subscription.close()?;

    // The same stream over UDP, through the same API: a publication whose endpoint is
    // a port on this host and a subscription on the same channel. Every message goes
    // through the driver's sender, the loopback and the driver's receiver, which
    // rebuilds the stream into an image the subscription reads as it read the log
    // above; between two hosts only the host in the channel changes.
    let udp_started = Instant::now();
    let mut udp_subscription = client.add_subscription(UDP_CHANNEL, STREAM_ID)?;
    let mut udp_publication = client.add_publication(UDP_CHANNEL, STREAM_ID)?;
    publish_and_read(&mut udp_publication, &mut udp_subscription)?;
    println!(
        "quickstart: {MESSAGES} messages published on stream {STREAM_ID} over {UDP_CHANNEL} by \
         session {} and received once, in order, through the driver's sender and receiver, \
         in {:.1} ms",
        udp_publication.session_id(),
        udp_started.elapsed().as_secs_f64() * 1e3
    );
    udp_publication.close()?;
    udp_subscription.close()?;
    client.close()?;
    Ok(())
}

/// Publishes `MESSAGES` messages on `publication`, each its number as eight bytes, and
/// reads them back through `subscription`, each once and in order, polling between
/// claims. Over `cavalry:ipc` and over `cavalry:udp` alike.
fn publish_and_read(
    publication: &mut Publication,
    subscription: &mut Subscription,
) -> Result<(), Box<dyn std::error::Error>> {
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
    Ok(())
}
