//! `cavalry-bench`: the latency and throughput harness `just bench` runs (CLAUDE.md,
//! gate 2; ADR 0015). The log rows drive `cavalry-shm` directly: a publisher thread
//! claims and commits into a real log buffer, a subscriber thread reads it, and a
//! conductor thread does the driver's duties, the limit and the cleaning, through a real
//! counters file. The client rows take the path a user takes: `cavalry-client` against a
//! real `cavalry-driver` in this process, publication to subscription (ADR 0021). Every
//! sample is one message, timed with `Instant`, and every number reported is a percentile
//! or a max of an HdrHistogram, never a mean. The measured loops run under a counting
//! allocator and the run fails when they allocate.
//!
//! `cavalry-bench --help` lists the options; `docs/how-to/run-the-bench.md` is the page.

use std::alloc::System;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use cavalry_client::{Cavalry, Offer};
use cavalry_driver::{Driver, DriverThread, IdleStrategy, ThreadStart, spec};
use cavalry_log::dir::LogDir;
use cavalry_log::layout::Recording;
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
/// Spins of the conductor's loop between two loads of the producer position: about a
/// millisecond on the bench host, the driver's cadence (ADR 0022;
/// `cavalry_driver::spec::PRODUCER_POSITION_INTERVAL`), counted rather than timed
/// because a clock read on every spin costs the other two threads at p99.99.
const PRODUCER_POSITION_SPINS: u32 = 10_000;

const CORRELATION_ID: i64 = 7;
const SESSION_ID: i32 = 1;
const STREAM_ID: i32 = 1001;
const MTU: u32 = 1408;
/// The histogram's range: 1 ns to 60 s, three significant figures. A value past the
/// range saturates rather than resizing, so recording never allocates.
const MAX_NS: u64 = 60_000_000_000;

struct Options {
    messages: u64,
    /// `--warmup`, or `None` for the default of one rotation (`Options::warmup()`).
    warmup_option: Option<u64>,
    body: usize,
    term_length: u32,
    profile: String,
    profile_suffix: String,
    out: Option<PathBuf>,
    /// The CPUs of the publisher, the subscriber and the driver's conductor, from
    /// `--pin`; the driver's recorder and the client's conductor run on the lowest CPU
    /// outside them.
    pin: Option<[usize; 3]>,
    /// That lowest CPU outside the pins, from the process's allowed set as it was before
    /// this thread pinned itself: the affinity a thread reads is its own.
    housekeeping: Option<usize>,
    /// How many times every benchmark runs; the reported run is the median by p99.99,
    /// and the result lists every run's.
    repeat: u32,
}

const USAGE: &str = "\
cavalry-bench: the latency harness of `just bench` (CLAUDE.md, gate 2; ADR 0015).

Options:
  --messages N      messages per benchmark after the warm-up (default 1000000)
  --warmup N        messages before recording starts; the default is one rotation through
                    all three terms, so no measured message first-touches a page (196608
                    for the default body and term)
  --body N          body bytes per message, at least 8 for the timestamp (default 32)
  --term-length N   term length of the log buffer, a power of two from 65536 (default 4194304)
  --profile NAME    the baseline profile; default $CAVALRY_BENCH_PROFILE or local-<os>-<arch>
  --profile-suffix S  appended to the profile; `just bench-unchecked` passes -unchecked
  --out DIR         where the result JSON goes (default bench/results); --no-write writes none
  --pin P,S,C       pin the publisher, the subscriber and the driver's conductor to CPUs P,
                    S and C, three distinct CPUs, Linux only, and the driver's recorder and
                    the client's conductor to the lowest CPU outside them; the bench host
                    passes 8,10,12 (ADR 0016)
  --repeat N        run every benchmark N times, report each one's median run by p99.99,
                    and list every run's p99.99 in the result (default 1; CI passes 21)
  --help            this text

Exit code 1 when a measured loop allocated, or a benchmark could not run.";

fn parse(args: &[String]) -> Result<Options, String> {
    let mut options = Options {
        messages: 1_000_000,
        warmup_option: None,
        body: 32,
        term_length: 4 * 1024 * 1024,
        profile: std::env::var("CAVALRY_BENCH_PROFILE").unwrap_or_else(|_| {
            format!("local-{}-{}", std::env::consts::OS, std::env::consts::ARCH)
        }),
        profile_suffix: String::new(),
        out: Some(PathBuf::from("bench/results")),
        pin: None,
        housekeeping: None,
        repeat: 1,
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
                options.warmup_option = Some(
                    value("--warmup")?
                        .parse()
                        .map_err(|e| format!("--warmup: {e}"))?,
                )
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
            "--pin" => options.pin = Some(parse_pin(value("--pin")?)?),
            "--repeat" => {
                options.repeat = value("--repeat")?
                    .parse()
                    .map_err(|e| format!("--repeat: {e}"))?
            }
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
    if options.repeat == 0 {
        return Err("--repeat must be at least 1".to_string());
    }
    if options.pin.is_some() && !cfg!(target_os = "linux") {
        return Err(
            "--pin needs Linux: macOS has no call that binds a thread to a CPU (ADR 0015)"
                .to_string(),
        );
    }
    options.profile.push_str(&options.profile_suffix);
    // Read here, before `main` pins this thread to the publisher's CPU, since the
    // allowed set a thread reads is its own.
    options.housekeeping = options.pin.map(housekeeping_cpu).transpose()?;
    Ok(options)
}

impl Options {
    /// Messages before recording starts: `--warmup`, or enough to rotate once through all
    /// three terms, so that every page of the log is mapped and dirty before a sample is
    /// taken. A first touch of a page is a fault of microseconds, and 1,500 of them in a
    /// 5,000,000-message run are the whole p99.99; a driver pre-touches its log at
    /// creation, so the gate measures what comes after.
    fn warmup(&self) -> u64 {
        self.warmup_for(self.term_length)
    }

    /// The same warm-up for a log of three `term_length` terms that `--term-length` does
    /// not size, the driver's for the client rows.
    fn warmup_for(&self, term_length: u32) -> u64 {
        const DEFAULT: u64 = 100_000;
        const HEADER: usize = 32;
        let frame = (HEADER + self.body).next_multiple_of(32) as u64;
        self.warmup_option
            .unwrap_or_else(|| DEFAULT.max(3 * u64::from(term_length) / frame.max(1)))
    }
}

/// Where the log buffer and the counters file go: `/dev/shm` on Linux, the memory-backed
/// file system a driver's log buffers live in, so that no write-back ever write-protects
/// a page under the publisher; the system's temporary directory elsewhere.
fn bench_dir() -> PathBuf {
    let shm = PathBuf::from("/dev/shm");
    if cfg!(target_os = "linux") && shm.is_dir() {
        shm
    } else {
        std::env::temp_dir()
    }
}

/// `--pin P,S,C`: three distinct CPU numbers.
fn parse_pin(text: &str) -> Result<[usize; 3], String> {
    let cpus: Vec<usize> = text
        .split(',')
        .map(|part| {
            part.trim()
                .parse::<usize>()
                .map_err(|e| format!("--pin: {part:?}: {e}"))
        })
        .collect::<Result<_, _>>()?;
    let [publisher, subscriber, conductor] = cpus[..] else {
        return Err(format!(
            "--pin takes three CPUs, publisher,subscriber,conductor; got {}",
            cpus.len()
        ));
    };
    if publisher == subscriber || publisher == conductor || subscriber == conductor {
        return Err(
            "--pin: the three CPUs must differ; two spinning threads on one CPU starve each other"
                .to_string(),
        );
    }
    Ok([publisher, subscriber, conductor])
}

/// Binds the calling thread to one CPU when `--pin` named one, and does nothing
/// otherwise. A thread the host refuses to bind is an error, not a noisier number: the
/// bench host's CPUs are isolated for this (`bench/host/check.sh`), and a number taken
/// off them is not comparable with the baseline.
fn pin_to(cpu: Option<usize>) -> Result<(), String> {
    match cpu {
        None => Ok(()),
        Some(id) if core_affinity::set_for_current(core_affinity::CoreId { id }) => Ok(()),
        Some(id) => Err(format!(
            "--pin: could not bind the thread to CPU {id}; is it online? (bench/host/check.sh)"
        )),
    }
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

/// A histogram with the fixed bounds, every page of its counts resident. The counts are
/// allocated zeroed and lazily, so a rare value's first landing in a new bucket took a
/// page fault inside the measured loop, on the sample that was rare by construction:
/// 55 to 68 faults per histogram on the second bench host, in the p99.99 (card 136).
/// Every sub-bucket is recorded once before the loop, which writes every page of the
/// counts, and the histogram is reset; the references through `black_box` keep the
/// compiler from folding those writes into the zeroed allocation, which it did to a
/// plain `reset` (the faults stayed, spread over the loop, in the trace on kip).
fn histogram() -> Histogram<u64> {
    let mut hist = Histogram::new_with_bounds(1, MAX_NS, 3).expect("the fixed bounds are valid");
    // Three significant digits is 2048 sub-buckets per doubling: a step of value ÷ 2048
    // lands in every one of them, about 60,000 records in all, once.
    let mut value: u64 = 1;
    while value <= MAX_NS {
        hist.saturating_record(value);
        value += (value >> 11).max(1);
    }
    std::hint::black_box(&mut hist);
    hist.reset();
    std::hint::black_box(&mut hist);
    hist
}

/// The lowest CPU in the calling thread's allowed set outside the three pins: where the
/// driver's recorder and the client's conductor run under `--pin`, so that the third
/// pin holds the driver's conductor alone (card 136). Read before any thread pins
/// itself: the bench host's shells and runner start with the housekeeping CPUs alone
/// in their set, and a thread already pinned to one CPU reads a set of one.
fn housekeeping_cpu(pin: [usize; 3]) -> Result<usize, String> {
    core_affinity::get_core_ids()
        .unwrap_or_default()
        .into_iter()
        .map(|core| core.id)
        .filter(|id| !pin.contains(id))
        .min()
        .ok_or_else(|| {
            format!(
                "--pin: no CPU outside {},{},{} in the process's allowed set for the \
                 driver's recorder and the client's conductor; run the harness from a shell \
                 whose allowed set holds a housekeeping CPU as well: `taskset -cp $$` prints \
                 the shell's, and `taskset -c 0-7 just bench --pin 8,10,12` runs with CPUs 0 \
                 to 7 in it (docs/how-to/run-the-bench.md#when-it-does-not-work)",
                pin[0], pin[1], pin[2]
            )
        })
}

/// What each driver thread could not pin under `--pin`, one slot per thread: the hook
/// returns nothing, so a thread whose `pin_to` failed stores its CPU plus one here, 0 for
/// none, and the publisher reads a slot once it is ordered after that thread's hook.
struct DriverPins {
    conductor: AtomicUsize,
    recorder: AtomicUsize,
}

impl DriverPins {
    fn new() -> Self {
        DriverPins {
            conductor: AtomicUsize::new(0),
            recorder: AtomicUsize::new(0),
        }
    }

    /// The slot of `thread`.
    fn slot(&self, thread: DriverThread) -> &AtomicUsize {
        match thread {
            DriverThread::Conductor => &self.conductor,
            DriverThread::Recorder => &self.recorder,
        }
    }

    /// The error a failed pin of `thread` fails the run with, or nothing. Sound only
    /// where the caller is ordered after that thread's hook: after an answer from the
    /// driver's conductor for the conductor, after the driver has closed for both.
    fn check(&self, thread: DriverThread) -> Result<(), String> {
        match self.slot(thread).load(Ordering::Acquire) {
            0 => Ok(()),
            failed => Err(format!(
                "--pin: could not bind the driver's {} to CPU {}; is it online? \
                 `cat /sys/devices/system/cpu/online` says which CPUs exist \
                 (docs/how-to/run-the-bench.md#when-it-does-not-work)",
                match thread {
                    DriverThread::Conductor => "conductor",
                    DriverThread::Recorder => "recorder",
                },
                failed - 1
            )),
        }
    }
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
    let warmup = options.warmup();
    let start = Instant::now();
    for i in 0..warmup + options.messages {
        let t0 = Instant::now();
        let t1 = Instant::now();
        if i >= warmup {
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
    let dir = bench_dir().join(format!(
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

    let warmup = options.warmup();
    let total = warmup + options.messages;
    let epoch = Instant::now();
    let done = Arc::new(AtomicBool::new(false));

    // The subscriber: a read-only mapping of its own, as a client has.
    let subscriber = {
        let dir = dir.to_path_buf();
        let position = subscriber_position.clone();
        let cpu = options.pin.map(|pin| pin[1]);
        thread::Builder::new()
            .name("subscriber".into())
            .spawn(move || -> Result<Histogram<u64>, String> {
                pin_to(cpu)?;
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
        let cpu = options.pin.map(|pin| pin[2]);
        thread::Builder::new()
            .name("conductor".into())
            .spawn(move || -> Result<(), String> {
                pin_to(cpu)?;
                let mut clean_position: i64 = 0;
                let mut limit_now = window;
                let trip_gain = window / 8;
                // The producer position about once per millisecond, the driver's cadence
                // (ADR 0022): the publisher stores to that line on every commit, and a
                // load on every spin kept the line moving between the two caches, which
                // was the publisher's p50 and its p99.99 band on the bench host. The
                // cadence is counted in spins, not read from the clock: a clock read on
                // every spin of this thread cost the publisher 100 ns and the subscriber
                // 70 to 130 ns at p99.99 on the bench host, in every one of eight
                // processes measured on 2026-10-13, for a reason not found; 10,000 spins
                // of this loop are about a millisecond there.
                let mut spins: u32 = 0;
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
                    spins += 1;
                    if spins >= PRODUCER_POSITION_SPINS {
                        publisher_position
                            .store_release(log.producer_position().map_err(|e| e.to_string())?);
                        spins = 0;
                    }
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
        if i == warmup {
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
        if i >= warmup {
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

/// The client rows: a real driver and a real client in this process, a publication and a
/// subscription on one stream, the publisher on this thread and the subscriber on its
/// own, each message claimed, written in place and committed, then delivered to the
/// subscription's handler (ADR 0021). The driver's and the client's conductor threads
/// inherit the conductor CPU under `--pin`, since a thread inherits the affinity of the
/// thread that spawns it. The log is the driver's, 64 MiB terms (`spec/client-protocol.md`,
/// "Sizes"), so the warm-up is one rotation of that log, not of `--term-length`'s, for the
/// same reason as the log rows': no measured message first-touches a page.
fn bench_client(options: &Options, recorded: bool) -> Result<LogRun, String> {
    let name = if recorded { "recorded" } else { "client" };
    let dir = bench_dir().join(format!("cavalry-bench-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let result = run_client(options, &dir, recorded);
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// The client rows, and with `recorded` the same rows while the driver's recorder writes
/// the stream to the log directory under the bench directory (ADR 0025): the recorder is
/// one more reader of the log, whose position the publisher's limit respects, and its
/// writes run on the driver's recorder thread. Under `--pin` the driver's conductor has
/// the third CPU to itself, and the recorder and the client's conductor run on the
/// lowest CPU outside the pins: three threads on one pinned CPU kept it ticking and
/// waking within itself (card 136). The recording must reach the publisher's final
/// position before the run counts, so a recorder that lost a byte or fell behind fails
/// the run, never a row.
fn run_client(options: &Options, dir: &std::path::Path, recorded: bool) -> Result<LogRun, String> {
    let housekeeping = options.housekeeping;
    let conductor_cpu = options.pin.map(|pin| pin[2]);
    // A pin a driver thread could not take is reported through `pins`, since the hook
    // returns nothing. `Driver::start_with_threads` returns as soon as both threads are
    // spawned and `Cavalry::connect_to` waits for no answer, so a load here would race
    // the hooks: the conductor's slot is read after the driver's first answer, which its
    // conductor sends from its loop, after its hook, and reaches this thread through
    // release and acquire stores; both slots are read again once `driver.close()` has
    // joined both threads, which is where the recorder's is first ordered. A failed pin
    // fails the run either way, and the row's numbers are never written.
    let pins = Arc::new(DriverPins::new());
    let on_thread_start: ThreadStart = {
        let pins = Arc::clone(&pins);
        Arc::new(move |thread| {
            let cpu = match thread {
                DriverThread::Conductor => conductor_cpu,
                DriverThread::Recorder => housekeeping,
            };
            if pin_to(cpu).is_err() {
                pins.slot(thread)
                    .store(cpu.map_or(0, |cpu| cpu + 1), Ordering::Release);
            }
        })
    };
    // This thread's affinity is what the client's conductor inherits.
    pin_to(housekeeping)?;
    let driver =
        Driver::start_with_threads(dir, dir.join("log"), IdleStrategy::Backoff, on_thread_start)
            .map_err(|e| e.to_string())?;
    let mut client = Cavalry::connect_to(dir).map_err(|e| e.to_string())?;
    let mut subscription = client
        .add_subscription("cavalry:ipc", STREAM_ID)
        .map_err(|e| e.to_string())?;
    // The driver's conductor has answered, so its hook has run.
    pins.check(DriverThread::Conductor)?;
    let mut publication = client
        .add_publication("cavalry:ipc", STREAM_ID)
        .map_err(|e| e.to_string())?;
    let recording = if recorded {
        Some(
            client
                .start_recording("cavalry:ipc", STREAM_ID)
                .map_err(|e| e.to_string())?,
        )
    } else {
        None
    };
    pin_to(options.pin.map(|pin| pin[0]))?;
    let deadline = Instant::now() + Duration::from_secs(10);
    while !subscription.is_connected() {
        subscription.poll(|_, _| {}, 1).map_err(|e| e.to_string())?;
        if Instant::now() > deadline {
            return Err("the subscription never connected".to_string());
        }
    }

    let warmup = options.warmup_for(spec::IPC_TERM_LENGTH);
    let total = warmup + options.messages;
    let epoch = Instant::now();

    let subscriber = {
        let cpu = options.pin.map(|pin| pin[1]);
        thread::Builder::new()
            .name("client-subscriber".into())
            .spawn(move || -> Result<Histogram<u64>, String> {
                pin_to(cpu)?;
                let mut hist = histogram();
                let mut received: u64 = 0;
                while received < total {
                    let delivered = subscription
                        .poll(
                            |body, _| {
                                let stamp =
                                    u64::from_le_bytes(body[..8].try_into().expect("8 bytes"));
                                if received >= warmup {
                                    hist.saturating_record(nanos(epoch).saturating_sub(stamp));
                                }
                                received += 1;
                            },
                            1024,
                        )
                        .map_err(|e| e.to_string())?;
                    if delivered == 0 {
                        std::hint::spin_loop();
                    }
                }
                Ok(hist)
            })
            .map_err(|e| e.to_string())?
    };

    let mut hist = histogram();
    let mut retries: u64 = 0;
    // The retries of the message being claimed. A subscriber that has returned, or one
    // that has stopped reading, leaves the publisher back-pressured for ever; the retry is
    // bounded so the run fails with the reason instead of hanging (invariant 3).
    let mut stalled: u64 = 0;
    const STALL_CHECK: u64 = 4096;
    const STALL_LIMIT: Duration = Duration::from_secs(10);
    let mut region: Option<Region<'static, System>> = None;
    let mut start = Instant::now();
    for i in 0..total {
        if i == warmup {
            region = Some(Region::new(GLOBAL));
            start = Instant::now();
        }
        let stamp = nanos(epoch);
        let t0 = Instant::now();
        loop {
            match publication.try_claim(options.body) {
                Ok(mut claim) => {
                    claim.body()[..8].copy_from_slice(&stamp.to_le_bytes());
                    claim.commit();
                    break;
                }
                Err(Offer::BackPressured | Offer::AdminAction | Offer::NotConnected) => {
                    retries += 1;
                    stalled += 1;
                    if subscriber.is_finished() {
                        return Err(subscriber_failure(subscriber.join()));
                    }
                    if stalled.is_multiple_of(STALL_CHECK) && t0.elapsed() > STALL_LIMIT {
                        return Err(format!(
                            "the publisher was back-pressured for over {} s at message {i} of \
                             {total}, so the subscriber stopped reading; see \
                             docs/how-to/run-the-bench.md",
                            STALL_LIMIT.as_secs()
                        ));
                    }
                    std::hint::spin_loop();
                }
                Err(other) => return Err(other.to_string()),
            }
        }
        stalled = 0;
        if i >= warmup {
            hist.saturating_record(span_ns(t0, Instant::now()));
        }
    }
    let elapsed = start.elapsed();
    let allocations = region.as_ref().map_or(0, |r| r.change().allocations);
    let one_way = match subscriber.join() {
        Ok(Ok(hist)) => hist,
        failed => return Err(subscriber_failure(failed)),
    };
    if let Some(recording) = recording {
        // The recorder's lag is bounded by the window; it reaches the publisher within
        // a window's worth of writing, or the run says so.
        let end = publication.position().map_err(|e| e.to_string())?;
        let deadline = Instant::now() + Duration::from_secs(30);
        while recording.position() < end {
            if Instant::now() > deadline {
                return Err(format!(
                    "the recorder stopped at {} of {end} bytes: a byte lost, or a disk that \
                     fell behind by over 30 s; see docs/how-to/record-a-stream.md",
                    recording.position()
                ));
            }
            std::hint::spin_loop();
        }
        client
            .stop_recording(recording.recording_id())
            .map_err(|e| e.to_string())?;
    }
    publication.close().map_err(|e| e.to_string())?;
    client.close().map_err(|e| e.to_string())?;
    driver.close().map_err(|e| e.to_string())?;
    // Both driver threads are joined, so both hooks are ordered before these loads.
    pins.check(DriverThread::Conductor)?;
    pins.check(DriverThread::Recorder)?;
    Ok(LogRun {
        claim_commit: summarize(&hist, elapsed),
        one_way: summarize(&one_way, elapsed),
        retries,
        allocations,
    })
}

/// Why the client subscriber thread ended before the publisher was done: its own error,
/// or its panic.
fn subscriber_failure(joined: thread::Result<Result<Histogram<u64>, String>>) -> String {
    match joined {
        Ok(Err(message)) => format!("the client subscriber failed: {message}"),
        Ok(Ok(_)) => "the client subscriber returned before the publisher was done".to_string(),
        Err(_) => "the client subscriber thread panicked".to_string(),
    }
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
/// the load and the store summaries. The loads run on this thread, the publisher's CPU
/// under `--pin`; the stores run on the subscriber's.
fn bench_slot(options: &Options, spacing: usize) -> Result<(Summary, Summary), String> {
    let words = Arc::new(Words {
        words: std::array::from_fn(|_| AtomicI64::new(0)),
    });
    let done = Arc::new(AtomicBool::new(false));
    let warmup = options.warmup();
    let total = warmup + options.messages;
    let writer = {
        let words = Arc::clone(&words);
        let done = Arc::clone(&done);
        let cpu = options.pin.map(|pin| pin[1]);
        thread::spawn(move || -> Result<Summary, String> {
            pin_to(cpu)?;
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
            Ok(summarize(&hist, elapsed))
        })
    };
    let index = spacing / 8;
    let mut hist = histogram();
    let mut sink: i64 = 0;
    let mut i: u64 = 0;
    let mut start = Instant::now();
    while !done.load(Ordering::Acquire) {
        if i == warmup {
            start = Instant::now();
        }
        let t0 = Instant::now();
        sink = sink.wrapping_add(words.words[index].load(Ordering::Acquire));
        let t1 = Instant::now();
        if i >= warmup {
            hist.saturating_record(span_ns(t0, t1));
        }
        i += 1;
    }
    let elapsed = start.elapsed();
    let stores = writer
        .join()
        .map_err(|_| "the writer thread panicked".to_string())??;
    std::hint::black_box(sink);
    Ok((summarize(&hist, elapsed), stores))
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

fn json_summary(out: &mut String, name: &str, s: &Summary, runs: &[u64]) {
    let every = runs
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    let _ = write!(
        out,
        "    \"{name}\": {{\"count\": {}, \"p50_ns\": {}, \"p99_ns\": {}, \"p99_99_ns\": {}, \"max_ns\": {}, \"rate_per_s\": {:.0}, \"p99_99_ns_runs\": [{every}]}}",
        s.count, s.p50_ns, s.p99_ns, s.p99_99_ns, s.max_ns, s.rate_per_s
    );
}

/// What the record row produced.
struct RecordRun {
    record_block: Summary,
    allocations: usize,
}

/// The recorder's block path (ADR 0025): a publisher fills one block of frames into an
/// exclusive log, then one scan of the block (`TermReader::block`) and its one positional
/// write into a segment file (`RecordingWriter::write`), timed per block under the
/// counting allocator. The log directory sits under the bench directory, `/dev/shm` on
/// the host, and the row runs last, so its writes reach no other row. The log holds
/// three terms and the segment two, so the row records at most two terms less a block,
/// whatever `--messages` says, and never rolls a segment.
fn bench_record(options: &Options) -> Result<RecordRun, String> {
    let dir = bench_dir().join(format!("cavalry-bench-{}-record", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let result = run_record(options, &dir);
    let _ = std::fs::remove_dir_all(&dir);
    result
}

fn run_record(options: &Options, dir: &std::path::Path) -> Result<RecordRun, String> {
    const BLOCK: usize = 1024 * 1024;
    const MS: i64 = 1_000_000;
    let window = options.term_length / 2;
    let fields = HeaderFields::new(
        LogType::Exclusive,
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
    let log = LogBuffers::create(dir, &fields).map_err(|e| e.to_string())?;
    log.set_connected(true).map_err(|e| e.to_string())?;
    let mut appender = log.appender().map_err(|e| e.to_string())?;
    let mut reader = log.reader(0).map_err(|e| e.to_string())?;
    let mut now = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos(),
    )
    .map_err(|e| e.to_string())?;
    let (mut log_dir, _) = LogDir::open(&dir.join("log"), now).map_err(|e| e.to_string())?;
    let recording = Recording {
        recording_id: 0,
        start_timestamp: now,
        start_position: 0,
        initial_term_id: 0,
        term_length: i32::try_from(options.term_length).map_err(|e| e.to_string())?,
        mtu_length: i32::try_from(MTU).map_err(|e| e.to_string())?,
        session_id: SESSION_ID,
        stream_id: STREAM_ID,
        channel: "cavalry:ipc",
        source_identity: "cavalry:ipc",
    };
    let mut writer = log_dir.start(&recording, now).map_err(|e| e.to_string())?;

    // Frames per block, and the blocks the log and the segment allow.
    let frame = (32 + options.body).div_ceil(32) * 32;
    let per_block = BLOCK / frame;
    let most = (2 * options.term_length as usize) / BLOCK - 1;
    let blocks =
        (usize::try_from(options.messages).unwrap_or(usize::MAX) / per_block).clamp(1, most);
    let warmup = blocks.min(8);
    let mut hist = histogram();
    let mut region: Option<Region<'static, System>> = None;
    let mut start = Instant::now();
    for block in 0..warmup + blocks {
        if block == warmup {
            region = Some(Region::new(GLOBAL));
            start = Instant::now();
        }
        for _ in 0..per_block {
            loop {
                match appender.try_claim(options.body, i64::MAX) {
                    Ok(claim) => {
                        claim.commit();
                        break;
                    }
                    Err(ClaimError::AdminAction) => std::hint::spin_loop(),
                    Err(other) => return Err(other.to_string()),
                }
            }
        }
        now += MS;
        let t0 = Instant::now();
        let scanned = reader.block(BLOCK).map_err(|e| e.to_string())?;
        let end = scanned.end();
        writer
            .write(scanned.bytes, scanned.length, now)
            .map_err(|e| e.to_string())?;
        reader.seek(end).map_err(|e| e.to_string())?;
        if block >= warmup {
            hist.saturating_record(span_ns(t0, Instant::now()));
        }
    }
    let elapsed = start.elapsed();
    let allocations = region.as_ref().map_or(0, |r| r.change().allocations);
    log_dir.stop(writer, now).map_err(|e| e.to_string())?;
    Ok(RecordRun {
        record_block: summarize(&hist, elapsed),
        allocations,
    })
}

/// What one pass over every benchmark produced.
struct Pass {
    benchmarks: BTreeMap<String, Summary>,
    retries: BTreeMap<String, u64>,
    allocations: usize,
}

/// Every benchmark once: the timer, both log types, the client path with and without a
/// recording on, the three slot spacings, and last the recorder's block path.
fn run_pass(options: &Options) -> Result<Pass, String> {
    let mut pass = Pass {
        benchmarks: BTreeMap::new(),
        retries: BTreeMap::new(),
        allocations: 0,
    };
    pass.benchmarks.insert("timer".into(), bench_timer(options));
    for (log_type, name) in [
        (LogType::Exclusive, "exclusive"),
        (LogType::Concurrent, "concurrent"),
    ] {
        let run = bench_log(options, log_type)
            .map_err(|message| format!("the {name} log benchmark failed: {message}"))?;
        pass.benchmarks.insert(
            format!("claim_commit_{}_{name}", options.body),
            run.claim_commit,
        );
        pass.benchmarks
            .insert(format!("one_way_{}_{name}", options.body), run.one_way);
        pass.retries.insert(name.into(), run.retries);
        pass.allocations += run.allocations;
    }
    for (recorded, name) in [(false, "client"), (true, "recorded")] {
        let client = bench_client(options, recorded)
            .map_err(|message| format!("the {name} benchmark failed: {message}"))?;
        pass.benchmarks.insert(
            format!("claim_commit_{}_{name}", options.body),
            client.claim_commit,
        );
        pass.benchmarks
            .insert(format!("one_way_{}_{name}", options.body), client.one_way);
        pass.retries.insert(name.into(), client.retries);
        pass.allocations += client.allocations;
    }
    for spacing in [0usize, 64, 128] {
        let (loads, stores) = bench_slot(options, spacing)
            .map_err(|message| format!("the slot_padding_{spacing} benchmark failed: {message}"))?;
        pass.benchmarks
            .insert(format!("slot_padding_{spacing}_load"), loads);
        pass.benchmarks
            .insert(format!("slot_padding_{spacing}_store"), stores);
    }
    let record = bench_record(options)
        .map_err(|message| format!("the record benchmark failed: {message}"))?;
    pass.benchmarks.insert(
        format!("record_block_{}", options.body),
        record.record_block,
    );
    pass.allocations += record.allocations;
    Ok(pass)
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
    // The publisher is this thread, and so are the timer and the slot-padding loads.
    if let Err(message) = pin_to(options.pin.map(|pin| pin[0])) {
        eprintln!("cavalry-bench: {message}");
        return ExitCode::FAILURE;
    }
    let pinned = options.pin.map_or_else(
        || "unpinned".to_string(),
        |pin| format!("pinned to CPUs {},{},{}", pin[0], pin[1], pin[2]),
    );
    let sha = git_sha();
    let date_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    println!(
        "cavalry-bench: profile {}, git {sha}, {} {}, {cpus} cores, {pinned}, {} messages after {} warm-up, body {} bytes, term {} bytes, files under {}, {} run(s) per benchmark",
        options.profile,
        std::env::consts::OS,
        std::env::consts::ARCH,
        options.messages,
        options.warmup(),
        options.body,
        options.term_length,
        bench_dir().display(),
        options.repeat
    );

    // Every pass, then each benchmark's median run by p99.99: on one pass, a 0.01 percent
    // tail of a few hundred nanoseconds lands anywhere in a band 50 percent wide, while
    // the median of 21 passes holds within 3 percent, and the result keeps every run's
    // p99.99 so that the band is on record (ADR 0016). For an even count the upper of
    // the two middle runs is reported, never a mean of them.
    let mut passes: BTreeMap<String, Vec<Summary>> = BTreeMap::new();
    let mut retries: BTreeMap<String, u64> = BTreeMap::new();
    let mut allocations: usize = 0;
    for _ in 0..options.repeat {
        let pass = match run_pass(&options) {
            Ok(pass) => pass,
            Err(message) => {
                eprintln!("cavalry-bench: {message}");
                return ExitCode::FAILURE;
            }
        };
        for (name, summary) in pass.benchmarks {
            passes.entry(name).or_default().push(summary);
        }
        for (name, count) in pass.retries {
            let worst = retries.entry(name).or_insert(0);
            *worst = (*worst).max(count);
        }
        allocations += pass.allocations;
    }
    let mut benchmarks: BTreeMap<String, Summary> = BTreeMap::new();
    let mut runs: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    for (name, summaries) in passes {
        runs.insert(
            name.clone(),
            summaries.iter().map(|s| s.p99_99_ns).collect(),
        );
        let mut sorted = summaries;
        sorted.sort_by_key(|s| s.p99_99_ns);
        let median = sorted.swap_remove(sorted.len() / 2);
        benchmarks.insert(name, median);
    }

    let messages_measured = 3 * options.messages * u64::from(options.repeat);
    let allocations_per_message = allocations as f64 / messages_measured as f64;

    println!();
    println!(
        "  {:<28} {:>10} {:>10} {:>10} {:>10} {:>12} {:>14}  p99.99 ns of every run",
        "benchmark", "count", "p50 ns", "p99 ns", "p99.99 ns", "max ns", "per second"
    );
    for (name, s) in &benchmarks {
        let every = runs.get(name).map_or(String::new(), |r| {
            r.iter().map(u64::to_string).collect::<Vec<_>>().join(" ")
        });
        println!(
            "  {:<28} {:>10} {:>10} {:>10} {:>10} {:>12} {:>14.0}  {every}",
            name, s.count, s.p50_ns, s.p99_ns, s.p99_99_ns, s.max_ns, s.rate_per_s
        );
    }
    println!();
    println!(
        "  publisher retries (back pressure or rotation), the most of any run: exclusive {}, concurrent {}, client {}",
        retries.get("exclusive").copied().unwrap_or(0),
        retries.get("concurrent").copied().unwrap_or(0),
        retries.get("client").copied().unwrap_or(0)
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
    let _ = writeln!(
        out,
        "  \"pinned\": {},",
        options.pin.map_or_else(
            || "null".to_string(),
            |pin| format!("[{}, {}, {}]", pin[0], pin[1], pin[2])
        )
    );
    let _ = writeln!(out, "  \"messages\": {},", options.messages);
    let _ = writeln!(out, "  \"repeat\": {},", options.repeat);
    let _ = writeln!(out, "  \"warmup\": {},", options.warmup());
    let _ = writeln!(out, "  \"body\": {},", options.body);
    let _ = writeln!(out, "  \"term_length\": {},", options.term_length);
    let _ = writeln!(
        out,
        "  \"publisher_retries\": {{\"exclusive\": {}, \"concurrent\": {}, \"client\": {}}},",
        retries.get("exclusive").copied().unwrap_or(0),
        retries.get("concurrent").copied().unwrap_or(0),
        retries.get("client").copied().unwrap_or(0)
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
            json_summary(
                &mut line,
                name,
                s,
                runs.get(name).map_or(&[][..], Vec::as_slice),
            );
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
