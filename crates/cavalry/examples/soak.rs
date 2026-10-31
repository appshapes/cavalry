//! The soak host's program (`docs/how-to/set-up-a-soak-host.md`; card 159): one process
//! attaches to the driver running on this host, publishes a steady stream at a fixed
//! rate and reads it back through a subscription, each message once and in order, for
//! days, and prints one line per report interval with what it has sent and received,
//! so that what CI sees for seconds is seen for a week. The recording of the stream is
//! not this program's: `scripts/soak.sh` starts, rotates, purges and deletes recordings
//! with `cavalry record` around it, so a rotation never touches the publisher. The
//! program stops when the stop file appears, closing its publication, its subscription
//! and its client, or at once with exit 1 on the first message out of order or a fault
//! on the image (invariant 1: a fault is reported, never read past). `CAVALRY_DIR` names
//! the driver's directory when it is not the default (`docs/reference/config.md`).
//!
//!     soak [--stream <id>] [--rate <messages per second>] [--body <bytes>]
//!          [--report <seconds>] [--seconds <n>] [--stop-file <path>]

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use cavalry::{Cavalry, Offer};

const USAGE: &str = "usage: soak [--stream <id>] [--rate <messages per second>] [--body <bytes>] \
                     [--report <seconds>] [--seconds <n>] [--stop-file <path>]";

/// What the command line asked for.
struct Options {
    stream_id: i32,
    /// Messages per second the publisher holds to; 0 is as fast as the subscriber reads.
    rate: u64,
    /// The body length, at least 8: the first eight bytes carry the sequence number.
    body: usize,
    /// Seconds between two report lines.
    report: u64,
    /// Seconds to run, 0 for until the stop file appears.
    seconds: u64,
    /// The file whose appearance stops the run.
    stop_file: Option<PathBuf>,
}

fn main() -> ExitCode {
    let options = match parse(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("soak: {error}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match run(&options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("soak: {error}");
            ExitCode::FAILURE
        }
    }
}

fn parse(args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut options = Options {
        stream_id: 1001,
        rate: 10_000,
        body: 32,
        report: 60,
        seconds: 0,
        stop_file: None,
    };
    let mut args = args.peekable();
    while let Some(name) = args.next() {
        let value = args.next().ok_or_else(|| format!("{name} needs a value"))?;
        let number = |what: &str| -> Result<u64, String> {
            value
                .parse::<u64>()
                .map_err(|_| format!("{name} needs {what}; got `{value}`"))
        };
        match name.as_str() {
            "--stream" => {
                options.stream_id = value
                    .parse::<i32>()
                    .map_err(|_| format!("--stream needs a stream id; got `{value}`"))?;
            }
            "--rate" => options.rate = number("messages per second")?,
            "--body" => {
                options.body =
                    usize::try_from(number("a byte count")?).map_err(|e| e.to_string())?;
                if options.body < 8 {
                    return Err("--body must be at least 8".to_string());
                }
            }
            "--report" => options.report = number("seconds")?.max(1),
            "--seconds" => options.seconds = number("seconds")?,
            "--stop-file" => options.stop_file = Some(PathBuf::from(value)),
            other => return Err(format!("unknown option {other}")),
        }
    }
    Ok(options)
}

/// The run: publish at the rate, read back in order, report, until the stop.
fn run(options: &Options) -> Result<(), Box<dyn std::error::Error>> {
    let started = Instant::now();
    let mut client = Cavalry::connect()?;
    let mut subscription = client.add_subscription("cavalry:ipc", options.stream_id)?;
    let mut publication = client.add_publication("cavalry:ipc", options.stream_id)?;
    println!(
        "soak: {} client {} on {} publishing stream {} session {} at {} messages/s, body {} \
         bytes, reporting every {} s{}",
        utc_now(),
        client.client_id(),
        client.dir().display(),
        options.stream_id,
        publication.session_id(),
        options.rate,
        options.body,
        options.report,
        options
            .stop_file
            .as_ref()
            .map_or(String::new(), |p| format!(
                ", until {} appears",
                p.display()
            ))
    );

    let mut sent: u64 = 0;
    let mut received: u64 = 0;
    let mut back_pressured: u64 = 0;
    let mut not_connected: u64 = 0;
    let mut body = vec![0u8; options.body];
    let report_every = Duration::from_secs(options.report);
    let mut next_report = started + report_every;
    let mut report_sent = 0u64;
    let mut report_received = 0u64;
    let mut idle_checks = 0u32;
    let mut stop_when_read = false;
    loop {
        // The pace: the message is due when its index over the rate has elapsed. At rate
        // 0 the publisher runs as fast as the subscriber reads.
        let due = options.rate == 0 || {
            let elapsed = started.elapsed().as_secs_f64();
            (sent as f64) < elapsed * options.rate as f64
        };
        let mut worked = false;
        if due && !stop_when_read {
            body[..8].copy_from_slice(&sent.to_le_bytes());
            match publication.try_claim(options.body) {
                Ok(mut claim) => {
                    claim.body().copy_from_slice(&body);
                    claim.commit();
                    sent += 1;
                    worked = true;
                }
                Err(Offer::NotConnected) => not_connected += 1,
                Err(Offer::BackPressured) => back_pressured += 1,
                Err(Offer::AdminAction) => {}
                Err(other) => return Err(other.into()),
            }
        }
        let read = subscription.poll(
            |body, _header| {
                let value = u64::from_le_bytes(body[..8].try_into().expect("8 bytes"));
                if value != received {
                    // Invariant 1: a message out of order or twice is reported, and the
                    // run ends here so the record keeps the position.
                    eprintln!(
                        "soak: {} message {value} arrived when {received} was next, after {sent} \
                         sent: stopping",
                        utc_now()
                    );
                    std::process::exit(1);
                }
                received += 1;
            },
            64,
        )?;
        worked |= read > 0;
        if !worked {
            // Nothing to send yet and nothing to read: a short sleep keeps the host
            // quiet, as a soak is not a latency measurement.
            std::thread::sleep(Duration::from_micros(50));
        }
        let now = Instant::now();
        if now >= next_report {
            next_report += report_every;
            println!(
                "soak: {} sent {sent} (+{}) received {received} (+{}) behind {} back-pressured \
                 {back_pressured} not-connected {not_connected} position {}",
                utc_now(),
                sent - report_sent,
                received - report_received,
                sent - received,
                publication.position().unwrap_or(-1)
            );
            report_sent = sent;
            report_received = received;
        }
        // The stop: the stop file, or the seconds, then everything sent is read.
        idle_checks = idle_checks.wrapping_add(1);
        if !stop_when_read && idle_checks.is_multiple_of(4096) {
            let by_seconds = options.seconds > 0 && started.elapsed().as_secs() >= options.seconds;
            let by_file = options.stop_file.as_ref().is_some_and(|p| p.exists());
            if by_seconds || by_file {
                stop_when_read = true;
                println!(
                    "soak: {} stopping after {sent} sent; reading the rest",
                    utc_now()
                );
            }
        }
        if stop_when_read && received >= sent {
            break;
        }
    }
    println!(
        "soak: {} done: {sent} sent, {received} received once and in order, {back_pressured} \
         back-pressured, {not_connected} not-connected, in {:.0} s",
        utc_now(),
        started.elapsed().as_secs_f64()
    );
    publication.close()?;
    subscription.close()?;
    client.close()?;
    Ok(())
}

/// The time as `YYYY-MM-DDTHH:MM:SSZ`, for the report lines, from the system clock.
fn utc_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = secs / 86_400;
    let rem = secs % 86_400;
    // Civil date from days since 1970-01-01, the proleptic Gregorian calendar.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}
