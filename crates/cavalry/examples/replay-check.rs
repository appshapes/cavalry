//! The replay gate's program (`just replay-check`; CLAUDE.md, gate 1; ADR 0030): one
//! process attaches to the driver running on this host, records a stream it publishes,
//! and reads the stream live through a subscription, printing every message as
//! `cavalry replay` prints it, `<position> <body as hex>`, the position from the
//! frame's header, so that the script can hash what the live subscriber saw and what
//! two replays print and compare the three. Each message's body is its number and a
//! hash chain over every body before it, so a byte out of place or a message missing
//! changes every line after it. A quarter of the way the publisher aborts a claim,
//! which leaves a padding frame in the recording that no line shows, so that every
//! position after it is past a frame a sum of the lines' lengths would miss. On stderr
//! it prints what the script needs: the recording id, the end position, an instant
//! taken half way, and the position the time index resolves that instant to.
//! `--messages <n>` sets the count, 100000 by default; `CAVALRY_DIR` names the driver's
//! directory when it is not the default (`docs/reference/config.md`).

use std::io::Write as _;
use std::process::ExitCode;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use cavalry::{Cavalry, Offer, Positions, index};

const STREAM_ID: i32 = 1094;

fn main() -> ExitCode {
    let messages = match parse(std::env::args().skip(1)) {
        Ok(messages) => messages,
        Err(error) => {
            eprintln!("replay-check: {error}");
            return ExitCode::from(2);
        }
    };
    match run(messages) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("replay-check: {error}");
            ExitCode::FAILURE
        }
    }
}

/// `--messages <n>` and nothing else.
fn parse(mut args: impl Iterator<Item = String>) -> Result<u64, String> {
    let mut messages = 100_000;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--messages" => {
                let value = args.next().ok_or("--messages needs a count")?;
                messages = value
                    .parse::<u64>()
                    .ok()
                    .filter(|&n| n > 0)
                    .ok_or_else(|| format!("--messages needs a count above 0; got `{value}`"))?;
            }
            other => return Err(format!("`{other}` is not an argument replay-check takes")),
        }
    }
    Ok(messages)
}

/// FNV-1a over `bytes`, continued from `hash`.
fn fnv1a(mut hash: u64, bytes: &[u8]) -> u64 {
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
    }
    hash
}

fn now_nanos() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_nanos()).unwrap_or(i64::MAX)
        })
}

fn run(messages: u64) -> Result<(), Box<dyn std::error::Error>> {
    let started = Instant::now();
    let mut client = Cavalry::connect()?;
    let log_dir = cavalry::Driver::default_log_dir()
        .ok_or("no default log directory on this host: set XDG_DATA_HOME or HOME")?;
    let mut publication = client.add_exclusive_publication("cavalry:ipc", STREAM_ID)?;
    let recording = client.start_recording("cavalry:ipc", STREAM_ID)?;
    let mut subscription = client.add_subscription("cavalry:ipc", STREAM_ID)?;
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());

    // The live subscriber's image, the publication's, joins at a poll: a poll with a
    // fragment limit of 0 applies it and reads no frame. Its positions name each
    // frame's position from its header, as `cavalry replay` does from the replay's
    // image; the recording started at 0 on a fresh publication, so they are the
    // recording's positions.
    let positions: Positions = {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            subscription.poll(|_, _| {}, 0)?;
            if let Some(image) = subscription.images().first() {
                break image.positions();
            }
            if Instant::now() > deadline {
                return Err("the live subscriber's image did not join".into());
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    };

    // The live subscriber: every message as `cavalry replay` prints it.
    let mut received: u64 = 0;
    let mut line = String::new();
    let mut poll = |subscription: &mut cavalry::Subscription,
                    out: &mut std::io::BufWriter<_>|
     -> Result<u64, Box<dyn std::error::Error>> {
        let mut lines: Vec<(i64, Vec<u8>)> = Vec::new();
        subscription.poll(
            |body, header| {
                let position = positions.position(header.term_id(), header.term_offset() as u32);
                lines.push((position, body.to_vec()));
            },
            256,
        )?;
        for (position, body) in &lines {
            use std::fmt::Write as _;
            line.clear();
            write!(line, "{position} ")?;
            for byte in body {
                write!(line, "{byte:02x}")?;
            }
            writeln!(out, "{line}")?;
        }
        Ok(lines.len() as u64)
    };

    // The messages: the number, then the chain over every body before.
    let mut chain: u64 = 0xcbf2_9ce4_8422_2325;
    let mut instant = 0i64;
    let mut half_position = 0i64;
    for n in 0..messages {
        let mut body = [0u8; 16];
        body[..8].copy_from_slice(&n.to_le_bytes());
        body[8..].copy_from_slice(&chain.to_le_bytes());
        chain = fnv1a(chain, &body);
        let after = loop {
            match publication.offer(&body) {
                Ok(after) => break after,
                Err(Offer::BackPressured | Offer::AdminAction) => {
                    received += poll(&mut subscription, &mut out)?;
                    std::hint::spin_loop();
                }
                Err(other) => return Err(format!("offer refused: {other}").into()),
            }
        };
        if n + 1 == messages / 4 {
            // A quarter of the way: a claim aborted is a padding frame in the recording
            // (`spec/log-buffer.md`, "Claim, commit and abort"), which the replayer
            // plays as padding and no handler sees, so every line after it is 64 bytes
            // past where a sum of the lines' lengths would put it; step 5 of the script
            // checks the first line's position against the time index's.
            loop {
                match publication.try_claim(32) {
                    Ok(claim) => {
                        let _ = claim.abort();
                        break;
                    }
                    Err(Offer::BackPressured | Offer::AdminAction) => {
                        received += poll(&mut subscription, &mut out)?;
                        std::hint::spin_loop();
                    }
                    Err(other) => return Err(format!("claim refused: {other}").into()),
                }
            }
        }
        if n + 1 == messages / 2 {
            // Half way: once the recorder holds every byte so far and a millisecond has
            // passed, an instant the index can place, for `--from <instant>`.
            half_position = after;
            let deadline = Instant::now() + Duration::from_secs(10);
            while recording.position() < half_position {
                if Instant::now() > deadline {
                    return Err("the recorder did not reach the half-way position".into());
                }
                received += poll(&mut subscription, &mut out)?;
                std::thread::sleep(Duration::from_millis(1));
            }
            std::thread::sleep(Duration::from_millis(2));
            instant = now_nanos();
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    let end = publication.position()?;
    let deadline = Instant::now() + Duration::from_secs(30);
    while received < messages {
        if Instant::now() > deadline {
            return Err(
                format!("the live subscriber got {received} of {messages} messages").into(),
            );
        }
        received += poll(&mut subscription, &mut out)?;
    }
    out.flush()?;
    while recording.synced_position() < end {
        if Instant::now() > deadline {
            return Err("the recording did not sync to the end".into());
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    client.stop_recording(recording.recording_id())?;
    let instant_position = index::position_at(&log_dir, recording.recording_id(), instant)?;
    eprintln!(
        "recording={} end={end} instant=@{instant} instant_position={instant_position} \
         half_position={half_position} messages={messages} seconds={}",
        recording.recording_id(),
        started.elapsed().as_secs_f64()
    );
    publication.close()?;
    subscription.close()?;
    client.close()?;
    Ok(())
}
