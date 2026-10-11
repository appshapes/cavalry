//! `cavalry-bench`: the latency and throughput harness `just bench` runs (CLAUDE.md,
//! gate 2; ADR 0015). It drives `cavalry-shm` directly, with no driver process yet: a
//! publisher thread claims and commits into a real log buffer, a subscriber thread reads
//! it, and a conductor thread does the driver's duties, the limit and the cleaning,
//! through a real counters file. Every sample is one message, timed with `Instant`, and
//! every number reported is a percentile or a max of an HdrHistogram, never a mean. The
//! measured loops run under a counting allocator and the run fails when they allocate.
//!
//! `cavalry-bench --help` lists the options; `docs/how-to/run-the-bench.md` is the page.

use std::alloc::System;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use cavalry_shm::counters::CountersFile;
use cavalry_shm::counters::generated::{
    PUBLICATION_LIMIT, PUBLICATION_POSITION, SUBSCRIPTION_POSITION,
};
use cavalry_shm::log::{ClaimError, HeaderFields, LogBuffers, LogType};
use hdrhistogram::Histogram;
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

const SCHEMA: u32 = 0;
const CORRELATION_ID: i64 = 7;
const SESSION_ID: i32 = 1;
const STREAM_ID: i32 = 1001;
const MTU: u32 = 1408;
/// The histogram's range: 1 ns to 60 s, three significant figures. A value past the
/// range saturates rather than resizing, so recording never allocates.
const MAX_NS: u64 = 60_000_000_000;

struct Options {
    messages: u64,
    warmup: u64,
    body: usize,
    term_length: u32,
    profile: String,
    profile_suffix: String,
    out: Option<PathBuf>,
}

const USAGE: &str = "\
cavalry-bench: the latency harness of `just bench` (CLAUDE.md, gate 2; ADR 0015).

Options:
  --messages N      messages per benchmark after the warm-up (default 1000000)
  --warmup N        messages before recording starts (default 100000)
  --body N          body bytes per message, at least 8 for the timestamp (default 32)
  --term-length N   term length of the log buffer, a power of two from 65536 (default 4194304)
  --profile NAME    the baseline profile; default $CAVALRY_BENCH_PROFILE or local-<os>-<arch>
  --profile-suffix S  appended to the profile; `just bench-unchecked` passes -unchecked
  --out DIR         where the result JSON goes (default bench/results); --no-write writes none
  --help            this text

Exit code 1 when a measured loop allocated, or a benchmark could not run.";

fn parse(args: &[String]) -> Result<Options, String> {
    let mut options = Options {
        messages: 1_000_000,
        warmup: 100_000,
        body: 32,
        term_length: 4 * 1024 * 1024,
        profile: std::env::var("CAVALRY_BENCH_PROFILE").unwrap_or_else(|_| {
            format!("local-{}-{}", std::env::consts::OS, std::env::consts::ARCH)
        }),
        profile_suffix: String::new(),
        out: Some(PathBuf::from("bench/results")),
    };
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let mut value = |name: &str| {
            iter.next()
                .ok_or_else(|| format!("{name} needs a value\n\n{USAGE}"))
        };
        match arg.as_str() {
            "--messages" => {
                options.messages = value("--messages")?
                    .parse()
                    .map_err(|e| format!("--messages: {e}"))?
            }
            "--warmup" => {
                options.warmup = value("--warmup")?
                    .parse()
                    .map_err(|e| format!("--warmup: {e}"))?
            }
            "--body" => {
                options.body = value("--body")?
                    .parse()
                    .map_err(|e| format!("--body: {e}"))?
            }
            "--term-length" => {
                options.term_length = value("--term-length")?
                    .parse()
                    .map_err(|e| format!("--term-length: {e}"))?;
            }
            "--profile" => options.profile = value("--profile")?.clone(),
            "--profile-suffix" => options.profile_suffix = value("--profile-suffix")?.clone(),
            "--out" => options.out = Some(PathBuf::from(value("--out")?)),
            "--no-write" => options.out = None,
            "--help" | "-h" => return Err(USAGE.to_string()),
            other => return Err(format!("unknown option {other}\n\n{USAGE}")),
        }
    }
    if options.body < 8 {
        return Err(
            "--body must be at least 8: the first eight bytes carry the timestamp".to_string(),
        );
    }
    if options.messages == 0 {
        return Err("--messages must be at least 1".to_string());
    }
    options.profile.push_str(&options.profile_suffix);
    Ok(options)
}

/// One benchmark's result: the percentiles the gate reads, and the rate.
#[derive(Clone, Debug)]
struct Summary {
    count: u64,
    p50_ns: u64,
    p99_ns: u64,
    p99_99_ns: u64,
    max_ns: u64,
    rate_per_s: f64,
}

fn histogram() -> Histogram<u64> {
    Histogram::new_with_bounds(1, MAX_NS, 3).expect("the fixed bounds are valid")
}

fn summarize(hist: &Histogram<u64>, elapsed: Duration) -> Summary {
    let count = hist.len();
    Summary {
        count,
        p50_ns: hist.value_at_quantile(0.5),
        p99_ns: hist.value_at_quantile(0.99),
        p99_99_ns: hist.value_at_quantile(0.9999),
        max_ns: hist.max(),
        rate_per_s: if elapsed.is_zero() {
            0.0
        } else {
            count as f64 / elapsed.as_secs_f64()
        },
    }
}

fn nanos(epoch: Instant) -> u64 {
    u64::try_from(epoch.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

fn span_ns(from: Instant, to: Instant) -> u64 {
    u64::try_from(to.duration_since(from).as_nanos()).unwrap_or(u64::MAX)
}

/// Two `Instant::now()` back to back: the harness's own overhead, present in every
/// other sample.
fn bench_timer(options: &Options) -> Summary {
    let mut hist = histogram();
    let start = Instant::now();
    for i in 0..options.warmup + options.messages {
        let t0 = Instant::now();
        let t1 = Instant::now();
        if i >= options.warmup {
            hist.saturating_record(span_ns(t0, t1));
        }
    }
    summarize(&hist, start.elapsed())
}

/// What the log benchmark produced for one log type.
struct LogRun {
    claim_commit: Summary,
    one_way: Summary,
    retries: u64,
    allocations: usize,
}

/// A publisher, a subscriber and a conductor over a real log buffer and counters file
/// under the system's temporary directory.
fn bench_log(options: &Options, log_type: LogType) -> Result<LogRun, String> {
    let dir = std::env::temp_dir().join(format!(
        "cavalry-bench-{}-{:?}",
        std::process::id(),
        log_type
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let result = run_log(options, log_type, &dir);
    let _ = std::fs::remove_dir_all(&dir);
    result
}

fn run_log(options: &Options, log_type: LogType, dir: &std::path::Path) -> Result<LogRun, String> {
    let window = options.term_length / 2;
    let fields = HeaderFields::new(
        log_type,
        CORRELATION_ID,
        0,
        options.term_length,
        MTU,
        window,
        SESSION_ID,
        STREAM_ID,
    )
    .map_err(|e| e.to_string())?;
    if options.body > fields.max_payload_length() as usize {
        return Err(format!(
            "--body {} is over the MTU's payload of {}",
            options.body,
            fields.max_payload_length()
        ));
    }
    let mut counters = CountersFile::create(dir, 64).map_err(|e| e.to_string())?;
    let mut key = [0u8; 16];
    key[..8].copy_from_slice(&CORRELATION_ID.to_le_bytes());
    key[8..12].copy_from_slice(&SESSION_ID.to_le_bytes());
    key[12..].copy_from_slice(&STREAM_ID.to_le_bytes());
    let label = |name: &str| format!("{name} {CORRELATION_ID} {SESSION_ID} {STREAM_ID}");
    let limit = counters
        .allocate(PUBLICATION_LIMIT, &key, &label("publication.limit"))
        .map_err(|e| e.to_string())?;
    let publisher_position = counters
        .allocate(PUBLICATION_POSITION, &key, &label("publication.position"))
        .map_err(|e| e.to_string())?;
    let subscriber_position = counters
        .allocate(SUBSCRIPTION_POSITION, &key, &label("subscription.position"))
        .map_err(|e| e.to_string())?;
    let log = Arc::new(LogBuffers::create(dir, &fields).map_err(|e| e.to_string())?);
    log.set_connected(true).map_err(|e| e.to_string())?;
    limit.store_release(i64::from(window));

    let total = options.warmup + options.messages;
    let epoch = Instant::now();
    let done = Arc::new(AtomicBool::new(false));

    // The subscriber: a read-only mapping of its own, as a client has.
    let subscriber = {
        let dir = dir.to_path_buf();
        let position = subscriber_position.clone();
        let warmup = options.warmup;
        thread::Builder::new()
            .name("subscriber".into())
            .spawn(move || -> Result<Histogram<u64>, String> {
                let log =
                    LogBuffers::open(&dir, CORRELATION_ID, false).map_err(|e| e.to_string())?;
                let mut reader = log.reader(0).map_err(|e| e.to_string())?;
                let mut hist = histogram();
                let mut received: u64 = 0;
                while received < total {
                    let poll = reader
                        .poll(1024, |body, _| {
                            let stamp = u64::from_le_bytes(body[..8].try_into().expect("8 bytes"));
                            if received >= warmup {
                                hist.saturating_record(nanos(epoch).saturating_sub(stamp));
                            }
                            received += 1;
                        })
                        .map_err(|e| e.to_string())?;
                    position.store_release(poll.position);
                    if poll.fragments == 0 {
                        std::hint::spin_loop();
                    }
                }
                Ok(hist)
            })
            .map_err(|e| e.to_string())?
    };

    // The conductor: the driver's duty cycle, the limit and the cleaning.
    let conductor = {
        let log = Arc::clone(&log);
        let done = Arc::clone(&done);
        let limit = limit.clone();
        let subscriber_position = subscriber_position.clone();
        let publisher_position = publisher_position.clone();
        let window = i64::from(window);
        thread::Builder::new()
            .name("conductor".into())
            .spawn(move || -> Result<(), String> {
                let mut clean_position: i64 = 0;
                let mut limit_now = window;
                let trip_gain = window / 8;
                while !done.load(Ordering::Acquire) {
                    let consumed = subscriber_position.load_acquire();
                    if consumed + window >= limit_now + trip_gain {
                        while clean_position < consumed {
                            clean_position = log
                                .clean(clean_position, consumed)
                                .map_err(|e| e.to_string())?;
                        }
                        limit_now = consumed + window;
                        limit.store_release(limit_now);
                    }
                    publisher_position
                        .store_release(log.producer_position().map_err(|e| e.to_string())?);
                    std::hint::spin_loop();
                }
                Ok(())
            })
            .map_err(|e| e.to_string())?
    };

    // The publisher, on this thread, with its own writable mapping.
    let publisher_log = LogBuffers::open(dir, CORRELATION_ID, true).map_err(|e| e.to_string())?;
    let mut appender = publisher_log.appender().map_err(|e| e.to_string())?;
    let mut hist = histogram();
    let mut retries: u64 = 0;
    let mut region: Option<Region<'static, System>> = None;
    let mut start = Instant::now();
    for i in 0..total {
        if i == options.warmup {
            region = Some(Region::new(GLOBAL));
            start = Instant::now();
        }
        let stamp = nanos(epoch);
        let t0 = Instant::now();
        loop {
            match appender.try_claim(options.body, limit.load_acquire()) {
                Ok(mut claim) => {
                    claim.body()[..8].copy_from_slice(&stamp.to_le_bytes());
                    claim.commit();
                    break;
                }
                Err(ClaimError::BackPressured | ClaimError::AdminAction) => {
                    retries += 1;
                    std::hint::spin_loop();
                }
                Err(other) => return Err(other.to_string()),
            }
        }
        if i >= options.warmup {
            hist.saturating_record(span_ns(t0, Instant::now()));
        }
    }
    let elapsed = start.elapsed();
    let allocations = region.as_ref().map_or(0, |r| r.change().allocations);
    let one_way = subscriber
        .join()
        .map_err(|_| "the subscriber thread panicked".to_string())??;
    done.store(true, Ordering::Release);
    conductor
        .join()
        .map_err(|_| "the conductor thread panicked".to_string())??;
    Ok(LogRun {
        claim_commit: summarize(&hist, elapsed),
        one_way: summarize(&one_way, elapsed),
        retries,
        allocations,
    })
}

/// Thirty-two words, 256 bytes, so two of them can sit 0, 64 or 128 bytes apart.
#[repr(C, align(256))]
struct Words {
    words: [AtomicI64; 32],
}

/// One thread stores to word 0 and times each store, as a subscriber stores its
/// position; another loads the word `spacing` bytes away and times each load, as a
/// publisher loads its limit, until the stores are done. 0 shares the cache line, 64
/// shares the 128-byte pair the prefetcher moves, 128 shares nothing (ADR 0011). Returns
/// the load and the store summaries.
fn bench_slot(options: &Options, spacing: usize) -> (Summary, Summary) {
    let words = Arc::new(Words {
        words: std::array::from_fn(|_| AtomicI64::new(0)),
    });
    let done = Arc::new(AtomicBool::new(false));
    let total = options.warmup + options.messages;
    let writer = {
        let words = Arc::clone(&words);
        let done = Arc::clone(&done);
        let warmup = options.warmup;
        thread::spawn(move || {
            let mut hist = histogram();
            let mut start = Instant::now();
            for i in 0..total {
                if i == warmup {
                    start = Instant::now();
                }
                let t0 = Instant::now();
                words.words[0].store(i as i64, Ordering::Release);
                let t1 = Instant::now();
                if i >= warmup {
                    hist.saturating_record(span_ns(t0, t1));
                }
            }
            let elapsed = start.elapsed();
            done.store(true, Ordering::Release);
            summarize(&hist, elapsed)
        })
    };
    let index = spacing / 8;
    let mut hist = histogram();
    let mut sink: i64 = 0;
    let mut i: u64 = 0;
    let mut start = Instant::now();
    while !done.load(Ordering::Acquire) {
        if i == options.warmup {
            start = Instant::now();
        }
        let t0 = Instant::now();
        sink = sink.wrapping_add(words.words[index].load(Ordering::Acquire));
        let t1 = Instant::now();
        if i >= options.warmup {
            hist.saturating_record(span_ns(t0, t1));
        }
        i += 1;
    }
    let elapsed = start.elapsed();
    let stores = writer.join().expect("the writer thread");
    std::hint::black_box(sink);
    (summarize(&hist, elapsed), stores)
}

fn git_sha() -> String {
    if let Ok(sha) = std::env::var("GITHUB_SHA") {
        return sha;
    }
    std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn json_summary(out: &mut String, name: &str, s: &Summary) {
    let _ = write!(
        out,
        "    \"{name}\": {{\"count\": {}, \"p50_ns\": {}, \"p99_ns\": {}, \"p99_99_ns\": {}, \"max_ns\": {}, \"rate_per_s\": {:.0}}}",
        s.count, s.p50_ns, s.p99_ns, s.p99_99_ns, s.max_ns, s.rate_per_s
    );
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let options = match parse(&args) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::FAILURE;
        }
    };
    let cpus = thread::available_parallelism().map_or(0, |n| n.get());
    if cpus < 3 {
        eprintln!(
            "cavalry-bench: {cpus} cores available; the log benchmarks run three spinning threads and will be noisy"
        );
    }
    let sha = git_sha();
    let date_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    println!(
        "cavalry-bench: profile {}, git {sha}, {} {}, {cpus} cores, {} messages after {} warm-up, body {} bytes, term {} bytes",
        options.profile,
        std::env::consts::OS,
        std::env::consts::ARCH,
        options.messages,
        options.warmup,
        options.body,
        options.term_length
    );

    let mut benchmarks: BTreeMap<String, Summary> = BTreeMap::new();
    let mut retries: BTreeMap<String, u64> = BTreeMap::new();
    let mut allocations: usize = 0;

    benchmarks.insert("timer".into(), bench_timer(&options));
    for (log_type, name) in [
        (LogType::Exclusive, "exclusive"),
        (LogType::Concurrent, "concurrent"),
    ] {
        match bench_log(&options, log_type) {
            Ok(run) => {
                benchmarks.insert(
                    format!("claim_commit_{}_{name}", options.body),
                    run.claim_commit,
                );
                benchmarks.insert(format!("one_way_{}_{name}", options.body), run.one_way);
                retries.insert(name.into(), run.retries);
                allocations += run.allocations;
            }
            Err(message) => {
                eprintln!("cavalry-bench: the {name} log benchmark failed: {message}");
                return ExitCode::FAILURE;
            }
        }
    }
    for spacing in [0usize, 64, 128] {
        let (loads, stores) = bench_slot(&options, spacing);
        benchmarks.insert(format!("slot_padding_{spacing}_load"), loads);
        benchmarks.insert(format!("slot_padding_{spacing}_store"), stores);
    }

    let messages_measured = 2 * options.messages;
    let allocations_per_message = allocations as f64 / messages_measured as f64;

    println!();
    println!(
        "  {:<28} {:>10} {:>10} {:>10} {:>10} {:>12} {:>14}",
        "benchmark", "count", "p50 ns", "p99 ns", "p99.99 ns", "max ns", "per second"
    );
    for (name, s) in &benchmarks {
        println!(
            "  {:<28} {:>10} {:>10} {:>10} {:>10} {:>12} {:>14.0}",
            name, s.count, s.p50_ns, s.p99_ns, s.p99_99_ns, s.max_ns, s.rate_per_s
        );
    }
    println!();
    println!(
        "  publisher retries (back pressure or rotation): exclusive {}, concurrent {}",
        retries.get("exclusive").copied().unwrap_or(0),
        retries.get("concurrent").copied().unwrap_or(0)
    );
    println!(
        "  allocations per message: {allocations_per_message} ({allocations} in {messages_measured} messages)"
    );

    let mut out = String::new();
    let _ = writeln!(out, "{{");
    let _ = writeln!(out, "  \"schema\": {SCHEMA},");
    let _ = writeln!(out, "  \"profile\": \"{}\",", options.profile);
    let _ = writeln!(out, "  \"git\": \"{sha}\",");
    let _ = writeln!(out, "  \"date_unix\": {date_unix},");
    let _ = writeln!(out, "  \"os\": \"{}\",", std::env::consts::OS);
    let _ = writeln!(out, "  \"arch\": \"{}\",", std::env::consts::ARCH);
    let _ = writeln!(out, "  \"cpus\": {cpus},");
    let _ = writeln!(out, "  \"messages\": {},", options.messages);
    let _ = writeln!(out, "  \"warmup\": {},", options.warmup);
    let _ = writeln!(out, "  \"body\": {},", options.body);
    let _ = writeln!(out, "  \"term_length\": {},", options.term_length);
    let _ = writeln!(
        out,
        "  \"publisher_retries\": {{\"exclusive\": {}, \"concurrent\": {}}},",
        retries.get("exclusive").copied().unwrap_or(0),
        retries.get("concurrent").copied().unwrap_or(0)
    );
    let _ = writeln!(
        out,
        "  \"allocations_per_message\": {allocations_per_message},"
    );
    let _ = writeln!(out, "  \"benchmarks\": {{");
    let entries: Vec<String> = benchmarks
        .iter()
        .map(|(name, s)| {
            let mut line = String::new();
            json_summary(&mut line, name, s);
            line
        })
        .collect();
    let _ = writeln!(out, "{}", entries.join(",\n"));
    let _ = writeln!(out, "  }}");
    let _ = writeln!(out, "}}");

    if let Some(dir) = &options.out {
        if let Err(e) = std::fs::create_dir_all(dir) {
            eprintln!("cavalry-bench: creating {}: {e}", dir.display());
            return ExitCode::FAILURE;
        }
        let path = dir.join(format!("{}-{date_unix}.json", options.profile));
        if let Err(e) = std::fs::write(&path, &out) {
            eprintln!("cavalry-bench: writing {}: {e}", path.display());
            return ExitCode::FAILURE;
        }
        println!("  written to {}", path.display());
    }

    if allocations > 0 {
        eprintln!(
            "cavalry-bench: the measured loops allocated {allocations} times; the gate requires zero allocations \
             per message (CLAUDE.md, gate 2; .claude/rules/data-plane.md)"
        );
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
