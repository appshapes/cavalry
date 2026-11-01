//! `cavalry-bench`: the latency and throughput harness `just bench` runs (CLAUDE.md,
//! gate 2; ADR 0015). The log rows drive `cavalry-shm` directly: a publisher thread
//! claims and commits into a real log buffer, a subscriber thread reads it, and a
//! conductor thread does the driver's duties, the limit and the cleaning, through a real
//! counters file. The client rows take the path a user takes: `cavalry-client` against a
//! real `cavalry-driver` in this process, publication to subscription (ADR 0021). Every
//! sample is one message, timed with the time stamp counter, calibrated once (ADR 0028),
//! and every number reported is a percentile or a max of an HdrHistogram, never a mean. The measured loops run under a counting
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
/// Spins of the conductor's loop between two reads of the clock, which decide when the
/// producer position is loaded: once every two milliseconds, the driver's cadence (ADR
/// 0022, ADR 0032;
/// [`PRODUCER_POSITION_INTERVAL_NS`]). The cadence was a count of 10,000 spins, about a
/// millisecond on the bench host, because a clock read on every spin cost the other two
/// threads at p99.99; on kip the same count was about half a millisecond, which put the
/// loads at the gate's percentile of the exclusive row (card 161), so the clock decides,
/// read once every this many spins, a 256th of that cost.
const CADENCE_SPINS: u32 = 256;
/// The driver's cadence for loading a publication's producer position, in nanoseconds
/// (ADR 0022; `cavalry_driver::spec::PRODUCER_POSITION_INTERVAL`).
const PRODUCER_POSITION_INTERVAL_NS: u64 =
    cavalry_driver::spec::PRODUCER_POSITION_INTERVAL.as_nanos() as u64;

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
    /// `--pin`; the driver's recorder and replayer and the client's conductor run on
    /// the lowest CPU outside them.
    pin: Option<[usize; 3]>,
    /// That lowest CPU outside the pins, from the process's allowed set as it was before
    /// this thread pinned itself: the affinity a thread reads is its own.
    housekeeping: Option<usize>,
    /// How many times every benchmark runs; the reported run is the median by p99.99,
    /// and the result lists every run's.
    repeat: u32,
    /// `--only`: run only the benchmarks with a row whose name contains this, for a
    /// profile of one row; the result is written under the profile with `-only`
    /// appended, so it is never compared as a full run. A value no row's name contains
    /// is an error from `parse`, which lists the rows.
    only: Option<String>,
}

/// One benchmark of a pass, with the rows it writes to the table and the result.
/// `--only` is matched against the rows, since those are the names the table, the
/// result and the baseline use, and one run of a benchmark writes all of its rows, so
/// the benchmark runs when any of them is wanted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Benchmark {
    /// Two readings of the clock back to back: the `timer` row.
    Timer,
    /// A publisher, a subscriber and a conductor over a real log buffer of this type:
    /// the `claim_commit_<body>_<name>` and `one_way_<body>_<name>` rows, and the key
    /// of the retries and the producer position loads in the result.
    Log {
        log_type: LogType,
        name: &'static str,
    },
    /// The client path through a real driver, with the recorder on when `recorded`:
    /// the same two rows under `name`, `client` or `recorded`.
    Client { recorded: bool, name: &'static str },
    /// A load and a store from two threads this many bytes apart (ADR 0011): the
    /// `slot_padding_<spacing>_load` and `slot_padding_<spacing>_store` rows.
    Slot { spacing: usize },
    /// The recorder's block path (ADR 0025): the `record_block_<body>` row.
    Record,
}

impl Benchmark {
    /// Every benchmark of a full pass, in the order they run: the timer, both log
    /// types, the client path with and without a recording on, the three slot
    /// spacings, and last the recorder's block path, so its writes reach no other row.
    const ALL: [Benchmark; 9] = [
        Benchmark::Timer,
        Benchmark::Log {
            log_type: LogType::Exclusive,
            name: "exclusive",
        },
        Benchmark::Log {
            log_type: LogType::Concurrent,
            name: "concurrent",
        },
        Benchmark::Client {
            recorded: false,
            name: "client",
        },
        Benchmark::Client {
            recorded: true,
            name: "recorded",
        },
        Benchmark::Slot { spacing: 0 },
        Benchmark::Slot { spacing: 64 },
        Benchmark::Slot { spacing: 128 },
        Benchmark::Record,
    ];

    /// The rows this benchmark writes, named as the table and the result name them for
    /// a body of `body` bytes, in the order `run_pass` has their summaries: the claim
    /// row before the one-way row, the load row before the store row.
    fn rows(self, body: usize) -> Vec<String> {
        match self {
            Benchmark::Timer => vec!["timer".into()],
            Benchmark::Log { name, .. } | Benchmark::Client { name, .. } => vec![
                format!("claim_commit_{body}_{name}"),
                format!("one_way_{body}_{name}"),
            ],
            Benchmark::Slot { spacing } => vec![
                format!("slot_padding_{spacing}_load"),
                format!("slot_padding_{spacing}_store"),
            ],
            Benchmark::Record => vec![format!("record_block_{body}")],
        }
    }
}

impl Options {
    /// Whether `--only` admits the row `name`: every row without it, else the rows
    /// whose name contains it.
    fn wanted(&self, name: &str) -> bool {
        self.only.as_deref().is_none_or(|only| name.contains(only))
    }

    /// The benchmarks of a pass, in the order they run: all of [`Benchmark::ALL`]
    /// without `--only`, else those with a row `--only` admits. Empty when `--only`
    /// names no row, which `parse` refuses.
    fn benchmarks(&self) -> Vec<Benchmark> {
        Benchmark::ALL
            .into_iter()
            .filter(|benchmark| benchmark.rows(self.body).iter().any(|row| self.wanted(row)))
            .collect()
    }

    /// Every row a full pass writes, in the order the benchmarks run: what `--only` is
    /// matched against, and what the error lists when it matches none.
    fn rows(&self) -> Vec<String> {
        Benchmark::ALL
            .into_iter()
            .flat_map(|benchmark| benchmark.rows(self.body))
            .collect()
    }
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
                    replayer and the client's conductor to the lowest CPU outside them; the
                    bench host passes 8,10,12 (ADR 0016)
  --repeat N        run every benchmark N times, report each one's median run by p99.99,
                    and list every run's p99.99 in the result (default 1; CI passes 21)
  --only NAME       run only the benchmarks whose name contains NAME, for a profile of
                    one row; the result goes under the profile with -only appended, and
                    a NAME that no row's name contains is an error listing the rows
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
        only: None,
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
            "--only" => options.only = Some(value("--only")?.clone()),
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
    if let Some(only) = &options.only {
        if options.benchmarks().is_empty() {
            return Err(format!(
                "--only {only}: no row's name contains it, so nothing would run. Pass one of these \
                 rows, or a part of one: {}",
                options.rows().join(", ")
            ));
        }
        options.profile.push_str("-only");
    }
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
    // lands in every one of them, about 38,000 records in all, once.
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
/// driver's recorder and replayer and the client's conductor run under `--pin`, so that
/// the third pin holds the driver's conductor alone (card 136). Read before any thread pins
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
                 driver's recorder and replayer and the client's conductor; run the harness from a shell \
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
    replayer: AtomicUsize,
    sender: AtomicUsize,
}

impl DriverPins {
    fn new() -> Self {
        DriverPins {
            conductor: AtomicUsize::new(0),
            recorder: AtomicUsize::new(0),
            replayer: AtomicUsize::new(0),
            sender: AtomicUsize::new(0),
        }
    }

    /// The slot of `thread`.
    fn slot(&self, thread: DriverThread) -> &AtomicUsize {
        match thread {
            DriverThread::Conductor => &self.conductor,
            DriverThread::Recorder => &self.recorder,
            DriverThread::Replayer => &self.replayer,
            DriverThread::Sender => &self.sender,
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
                    DriverThread::Replayer => "replayer",
                    DriverThread::Sender => "sender",
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

/// The harness's clock for every sample: the time stamp counter, calibrated once at
/// start, with the monotonic clock as the fallback where the counter is not invariant
/// (ADR 0028). Made in `main` before the first row; `clock()` is one acquire load.
static CLOCK: std::sync::OnceLock<quanta::Clock> = std::sync::OnceLock::new();

#[inline(always)]
fn clock() -> &'static quanta::Clock {
    CLOCK.get_or_init(quanta::Clock::new)
}

/// A reading of the clock, in its own units: what a stamp and a span are made from.
#[inline(always)]
fn tick() -> u64 {
    clock().raw()
}

/// Nanoseconds from the reading `epoch` to now.
#[inline(always)]
fn nanos(epoch: u64) -> u64 {
    let clock = clock();
    clock.delta_as_nanos(epoch, clock.raw())
}

/// Nanoseconds between two readings.
#[inline(always)]
fn span_ns(from: u64, to: u64) -> u64 {
    clock().delta_as_nanos(from, to)
}

/// How many pairs of consecutive readings `clock_step` takes.
const STEP_PAIRS: usize = 1_000_000;
/// The largest advance `clock_step` counts; a larger one is an interrupt or a migration.
const STEP_ADVANCES: usize = 4096;

/// The clock's step: the smallest change the counter reports, in nanoseconds rounded to
/// the nearest and in the counter's own ticks. Measured once, on the publisher's CPU
/// once it is pinned, before the first row: a million pairs of consecutive readings are
/// taken and their advances counted, with the counts on the stack, since a heap
/// allocation here moved the raw rows' tail by 60 ns on the bench host, and
/// `step_ticks` reads the step off the counts. `bench-compare` fails a gated row only
/// beyond one step as well as beyond 5 percent (ADR 0029), and the first line prints
/// both numbers, so the log shows which clock the harness is on.
fn clock_step() -> (u64, u64) {
    // How often each advance below STEP_ADVANCES ticks occurred; the rest are outliers.
    // Pairs with no advance are counted apart: a counter that moves less often than
    // the harness reads it shows its step as a jump, not as a divisor (ADR 0037).
    let mut counts = [0u32; STEP_ADVANCES];
    let mut unmoved = 0usize;
    for _ in 0..STEP_PAIRS {
        let a = tick();
        let b = tick();
        let advance = b.wrapping_sub(a) as usize;
        if advance == 0 {
            unmoved += 1;
        } else if advance < STEP_ADVANCES {
            counts[advance] += 1;
        }
    }
    let ticks = step_ticks(&counts, unmoved, STEP_PAIRS);
    // To the nearest nanosecond, not truncated: 37 ticks of a counter calibrated at
    // 3.7001 GHz are 9.9997 ns, and a floor of 9 would still fail a move of 10.
    let ns = (span_ns(0, ticks * 1000) + 500) / 1000;
    (ns.max(1), ticks)
}

/// The step in ticks that `counts` shows, where `counts[a]` is how many of `pairs` pairs
/// of readings advanced by `a` ticks and `unmoved` how many advanced by none. An
/// advance seen in fewer than one pair in a thousand is an interrupt or a migration and
/// is ignored. When most pairs advanced by none, the counter moves less often than the
/// harness reads it and its step is the jump it makes when it moves: the smallest
/// frequent advance, since the jumps of a counter whose period is not a whole number of
/// the reading's ticks come in two sizes and the smaller one under-reads the step,
/// which a floor may do and over-read may not (ADR 0037). On an Apple silicon laptop
/// `quanta` reads the machine's own counter at 1 GHz, which moves 24 million times a
/// second: 987,728 pairs in a million advanced by none, 3,318 by 41 ticks and 8,942 by
/// 42, which a divisor over 41 and 42 read as a step of 1 ns where the clock resolves
/// 42; the macOS CI runner's counter ticks at 24 MHz itself and reads 1 tick, 42 ns,
/// either way. Otherwise, as ADR 0029 says, an advance below half the most frequent one
/// is the counter's own residual, not a step, and is ignored too: the bench host's
/// counter moves by 37 ticks, 10 ns, at a time and reads 37 or 74 between two readings,
/// with a one-tick residual in 2 percent of them, and a divisor over everything read 1
/// there. The step is the greatest common divisor of what remains, at least 1: 37 ticks
/// on the bench host; 2 ticks, a third of a nanosecond each, on kip, whose readings are
/// 26 to 34 ticks apart in even numbers, so its percentiles resolve to the nanosecond.
/// Pure, so the tests below hold the bench hosts' and the laptop's histograms.
fn step_ticks(counts: &[u32; STEP_ADVANCES], unmoved: usize, pairs: usize) -> u64 {
    fn gcd(a: u64, b: u64) -> u64 {
        if b == 0 { a } else { gcd(b, a % b) }
    }
    let frequent = ((pairs / 1000) as u32).max(1);
    if unmoved > pairs / 2 {
        return (1..STEP_ADVANCES)
            .find(|&a| counts[a] >= frequent)
            .map_or(1, |a| a as u64);
    }
    let mode = (1..STEP_ADVANCES).max_by_key(|&a| counts[a]).unwrap_or(1);
    (mode / 2..STEP_ADVANCES)
        .filter(|&a| a > 0 && counts[a] >= frequent)
        .fold(0u64, |acc, a| gcd(acc, a as u64))
        .max(1)
}

/// Two readings of the harness's clock back to back: its own overhead, present in every
/// other sample.
fn bench_timer(options: &Options) -> Summary {
    let mut hist = histogram();
    let warmup = options.warmup();
    let start = Instant::now();
    for i in 0..warmup + options.messages {
        let t0 = tick();
        let t1 = tick();
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
    /// How many times the harness's own conductor loaded the producer position during
    /// the measured run, for the log rows: about one every two milliseconds of the run (ADR
    /// 0022; card 161). The client rows run the driver's conductor, which is not counted.
    producer_position_loads: Option<u64>,
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
    // Every static counter lives in the record whose index is its id, so the file holds
    // more records than the largest static id in counters.toml (72 at ADR 0041).
    let mut counters = CountersFile::create(dir, 128).map_err(|e| e.to_string())?;
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
    let epoch = tick();
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
            .spawn(move || -> Result<u64, String> {
                pin_to(cpu)?;
                let mut clean_position: i64 = 0;
                let mut limit_now = window;
                let trip_gain = window / 8;
                // The producer position at the driver's cadence by the clock, once every
                // two milliseconds (ADR 0022, ADR 0032): the publisher stores to that line
                // on every commit, and a load on every spin kept the line moving between
                // the two caches, which was the publisher's p50 and its p99.99 band on the
                // bench host.
                // The clock is read once every CADENCE_SPINS spins, not on every spin: a
                // clock read on every spin of this thread cost the publisher 100 ns and
                // the subscriber 70 to 130 ns at p99.99 on the bench host, in every one
                // of eight processes measured on 2026-10-13, for a reason not found. A
                // count of 10,000 spins stood in for the clock until card 161: about a
                // millisecond on the bench host and about half of one on kip, where the
                // loads then landed at the gate's percentile of the exclusive row. The
                // loads are counted and reported, so each host's cadence is on record.
                let mut spins: u32 = 0;
                let mut last_load = tick();
                let mut loads: u64 = 0;
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
                    if spins >= CADENCE_SPINS {
                        spins = 0;
                        let now = tick();
                        if span_ns(last_load, now) >= PRODUCER_POSITION_INTERVAL_NS {
                            publisher_position
                                .store_release(log.producer_position().map_err(|e| e.to_string())?);
                            last_load = now;
                            loads += 1;
                        }
                    }
                    std::hint::spin_loop();
                }
                Ok(loads)
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
        let t0 = tick();
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
            hist.saturating_record(span_ns(t0, tick()));
        }
    }
    let elapsed = start.elapsed();
    let allocations = region.as_ref().map_or(0, |r| r.change().allocations);
    let one_way = subscriber
        .join()
        .map_err(|_| "the subscriber thread panicked".to_string())??;
    done.store(true, Ordering::Release);
    let producer_position_loads = conductor
        .join()
        .map_err(|_| "the conductor thread panicked".to_string())??;
    Ok(LogRun {
        claim_commit: summarize(&hist, elapsed),
        one_way: summarize(&one_way, elapsed),
        retries,
        allocations,
        producer_position_loads: Some(producer_position_loads),
    })
}

/// The client rows: a real driver and a real client in this process, a publication and a
/// subscription on one stream, the publisher on this thread and the subscriber on its
/// own, each message claimed, written in place and committed, then delivered to the
/// subscription's handler (ADR 0021). Under `--pin` the driver's conductor pins itself
/// to the third CPU and the driver's recorder and replayer and the client's conductor
/// run on the lowest CPU outside the pins, as `run_client` says and does; a thread inherits the
/// affinity of the thread that spawns it, which is what the placement arranges. The log
/// is the driver's, 64 MiB terms (`spec/client-protocol.md`,
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
/// the third CPU to itself, and the recorder, the replayer and the client's conductor
/// run on the lowest CPU outside the pins: three threads on one pinned CPU kept it
/// ticking and waking within itself (card 136); the replayer holds no replay in any row
/// and stays parked. The recording must reach the publisher's final
/// position before the run counts, so a recorder that lost a byte or fell behind fails
/// the run, never a row.
fn run_client(options: &Options, dir: &std::path::Path, recorded: bool) -> Result<LogRun, String> {
    let housekeeping = options.housekeeping;
    let conductor_cpu = options.pin.map(|pin| pin[2]);
    // A pin a driver thread could not take is reported through `pins`, since the hook
    // returns nothing. `Driver::start_with_threads` returns as soon as its three threads
    // are spawned and `Cavalry::connect_to` waits for no answer, so a load here would
    // race the hooks: the conductor's slot is read after the driver's first answer,
    // which its conductor sends from its loop, after its hook, and reaches this thread
    // through release and acquire stores; every slot is read again once `driver.close()`
    // has joined the three threads, which is where the recorder's and the replayer's are
    // first ordered. A failed pin fails the run either way, and the row's numbers are
    // never written.
    let pins = Arc::new(DriverPins::new());
    let on_thread_start: ThreadStart = {
        let pins = Arc::clone(&pins);
        Arc::new(move |thread| {
            // The sender goes with the housekeeping threads until the UDP rows give it
            // a core of its own (card 182): no row of this version sends a datagram.
            let cpu = match thread {
                DriverThread::Conductor => conductor_cpu,
                DriverThread::Recorder | DriverThread::Replayer | DriverThread::Sender => {
                    housekeeping
                }
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
    let epoch = tick();

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
        let t0 = tick();
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
                    if stalled.is_multiple_of(STALL_CHECK)
                        && span_ns(t0, tick()) > STALL_LIMIT.as_nanos() as u64
                    {
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
            hist.saturating_record(span_ns(t0, tick()));
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
    // Every driver thread is joined, so every hook is ordered before these loads.
    pins.check(DriverThread::Conductor)?;
    pins.check(DriverThread::Recorder)?;
    pins.check(DriverThread::Replayer)?;
    Ok(LogRun {
        claim_commit: summarize(&hist, elapsed),
        one_way: summarize(&one_way, elapsed),
        retries,
        allocations,
        producer_position_loads: None,
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
                let t0 = tick();
                words.words[0].store(i as i64, Ordering::Release);
                let t1 = tick();
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
        let t0 = tick();
        sink = sink.wrapping_add(words.words[index].load(Ordering::Acquire));
        let t1 = tick();
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
        let t0 = tick();
        let scanned = reader.block(BLOCK).map_err(|e| e.to_string())?;
        let end = scanned.end();
        writer
            .write(scanned.bytes, scanned.length, now)
            .map_err(|e| e.to_string())?;
        reader.seek(end).map_err(|e| e.to_string())?;
        if block >= warmup {
            hist.saturating_record(span_ns(t0, tick()));
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
    producer_position_loads: BTreeMap<String, u64>,
    allocations: usize,
    /// How many message rows ran, each of `--messages` messages, for the allocations
    /// per message: the two log rows and the two client rows in a full pass, fewer
    /// under `--only`.
    message_rows: u64,
}

impl Pass {
    /// Records `summaries` under `benchmark`'s rows, in the order [`Benchmark::rows`]
    /// names them, so a row is written under the name `--only` was matched against.
    fn insert_rows(
        &mut self,
        benchmark: Benchmark,
        body: usize,
        summaries: impl IntoIterator<Item = Summary>,
    ) {
        for (row, summary) in benchmark.rows(body).into_iter().zip(summaries) {
            self.benchmarks.insert(row, summary);
        }
    }

    /// Records a message benchmark's run, a log or a client one: its two rows, its
    /// retries and producer position loads under `name`, and its allocations.
    fn insert_message_run(&mut self, benchmark: Benchmark, name: &str, body: usize, run: LogRun) {
        self.insert_rows(benchmark, body, [run.claim_commit, run.one_way]);
        self.retries.insert(name.into(), run.retries);
        if let Some(loads) = run.producer_position_loads {
            self.producer_position_loads.insert(name.into(), loads);
        }
        self.allocations += run.allocations;
        self.message_rows += 1;
    }
}

/// Every benchmark `--only` admits once, in [`Benchmark::ALL`]'s order: the timer, both
/// log types, the client path with and without a recording on, the three slot
/// spacings, and last the recorder's block path.
fn run_pass(options: &Options) -> Result<Pass, String> {
    let mut pass = Pass {
        benchmarks: BTreeMap::new(),
        retries: BTreeMap::new(),
        producer_position_loads: BTreeMap::new(),
        allocations: 0,
        message_rows: 0,
    };
    for benchmark in options.benchmarks() {
        match benchmark {
            Benchmark::Timer => {
                pass.insert_rows(benchmark, options.body, [bench_timer(options)]);
            }
            Benchmark::Log { log_type, name } => {
                let run = bench_log(options, log_type)
                    .map_err(|message| format!("the {name} log benchmark failed: {message}"))?;
                pass.insert_message_run(benchmark, name, options.body, run);
            }
            Benchmark::Client { recorded, name } => {
                let run = bench_client(options, recorded)
                    .map_err(|message| format!("the {name} benchmark failed: {message}"))?;
                pass.insert_message_run(benchmark, name, options.body, run);
            }
            Benchmark::Slot { spacing } => {
                let (loads, stores) = bench_slot(options, spacing).map_err(|message| {
                    format!("the slot_padding_{spacing} benchmark failed: {message}")
                })?;
                pass.insert_rows(benchmark, options.body, [loads, stores]);
            }
            Benchmark::Record => {
                let record = bench_record(options)
                    .map_err(|message| format!("the record benchmark failed: {message}"))?;
                pass.insert_rows(benchmark, options.body, [record.record_block]);
                pass.allocations += record.allocations;
            }
        }
    }
    Ok(pass)
}

fn main() -> ExitCode {
    // The clock calibrates once, for up to 200 ms, before the first row (ADR 0028).
    let _ = clock();
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
    // The clock's step, on the publisher's CPU now that it is pinned (ADR 0029).
    let (clock_step, clock_step_ticks) = clock_step();
    let pinned = options.pin.map_or_else(
        || "unpinned".to_string(),
        |pin| format!("pinned to CPUs {},{},{}", pin[0], pin[1], pin[2]),
    );
    let sha = git_sha();
    let date_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    println!(
        "cavalry-bench: profile {}, git {sha}, {} {}, {cpus} cores, {pinned}, clock step {clock_step} ns ({clock_step_ticks} ticks), {} messages after {} warm-up, body {} bytes, term {} bytes, files under {}, {} run(s) per benchmark",
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
    let mut producer_position_loads: BTreeMap<String, u64> = BTreeMap::new();
    let mut allocations: usize = 0;
    let mut message_rows: u64 = 0;
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
        for (name, count) in pass.producer_position_loads {
            let most = producer_position_loads.entry(name).or_insert(0);
            *most = (*most).max(count);
        }
        allocations += pass.allocations;
        message_rows += pass.message_rows;
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

    // The message rows that ran, times their messages: four rows in a full pass, the
    // two log rows and the two client rows; fewer under `--only`, and none when it
    // admitted only the timer, slot or record rows, where the ratio has no denominator
    // and is left out of the line and the result, while the count still decides the
    // exit code. The recorder's block row adds its allocations to the count and no
    // messages.
    let messages_measured = message_rows * options.messages;
    let allocations_per_message =
        (messages_measured > 0).then(|| allocations as f64 / messages_measured as f64);

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
        "  producer position loads by the harness's conductor, at the driver's cadence by the clock, the most of any run: {}",
        if producer_position_loads.is_empty() {
            "no log row ran".to_string()
        } else {
            producer_position_loads
                .iter()
                .map(|(name, loads)| format!("{name} {loads}"))
                .collect::<Vec<_>>()
                .join(", ")
        }
    );
    match allocations_per_message {
        Some(ratio) => println!(
            "  allocations per message: {ratio} ({allocations} in {messages_measured} messages)"
        ),
        None => println!(
            "  allocations per message: no message row ran ({allocations} allocations in the measured loops)"
        ),
    }

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
        "  \"producer_position_loads\": {{{}}},",
        producer_position_loads
            .iter()
            .map(|(name, loads)| format!("\"{name}\": {loads}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    // The count the exit code is decided on, beside the ratio: `bench-compare` judges
    // the count when no message row ran and the ratio is `null` (card 161).
    let _ = writeln!(out, "  \"allocations\": {allocations},");
    let _ = writeln!(
        out,
        "  \"allocations_per_message\": {},",
        allocations_per_message.map_or_else(|| "null".to_string(), |ratio| ratio.to_string())
    );
    let _ = writeln!(out, "  \"clock_step_ns\": {clock_step},");
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A histogram from `(advance, pairs)` entries, every other advance unseen.
    fn counts(advances: &[(usize, u32)]) -> [u32; STEP_ADVANCES] {
        let mut counts = [0u32; STEP_ADVANCES];
        for &(advance, n) in advances {
            counts[advance] = n;
        }
        counts
    }

    /// The bench host's counter on its publisher CPU at 5712454 (ADR 0029, Evidence):
    /// 37 ticks in 965,497 pairs of a million, 74 in 11,203, and the one-tick residual in
    /// 23,282, which a divisor over everything turned into a step of 1. Two more runs of
    /// the same loop on 2026-10-28 counted 37: 968,470, 1: 24,772, 74: 6,758, and
    /// 37: 972,972, 1: 27,027.
    #[test]
    fn the_bench_hosts_counter_steps_by_37_ticks_despite_its_residual() {
        let bench = counts(&[(37, 965_497), (1, 23_282), (74, 11_203)]);
        assert_eq!(step_ticks(&bench, 18, STEP_PAIRS), 37);
    }

    /// kip's counter on its publisher CPU on 2026-10-28, from `clock_step`'s loop printing
    /// its counts: every advance even, 26 to 34 ticks, so the step is 2 ticks, a third of
    /// a nanosecond, and the harness prints `1 ns (2 ticks)`.
    #[test]
    fn kips_counter_steps_by_2_ticks() {
        let kip = counts(&[
            (26, 6_383),
            (28, 216_425),
            (30, 571_482),
            (32, 173_417),
            (34, 32_290),
        ]);
        assert_eq!(step_ticks(&kip, 3, STEP_PAIRS), 2);
    }

    /// The Apple silicon laptop pull request 58 was written on, a million pairs of
    /// `quanta` raw readings in `clock_step`'s own loop (card 154): the counter counts at
    /// 1 GHz and moves 24 million times a second, so 987,728 pairs advanced by none,
    /// 3,318 by 41 ticks and 8,942 by 42, with 18, 58 and 59 a few times each. A
    /// divisor over 41 and 42 read 1; the jump is the step, and the smaller of the two
    /// sizes it comes in, 41 ticks, 41 ns (ADR 0037).
    #[test]
    fn a_counter_that_moves_less_often_than_it_is_read_steps_by_its_jump() {
        let laptop = counts(&[(41, 3_318), (42, 8_942), (18, 3), (58, 2), (59, 4)]);
        assert_eq!(step_ticks(&laptop, 987_728, STEP_PAIRS), 41);
    }

    /// The macOS CI runner's counter ticks at 24 MHz itself: most pairs read the same
    /// value and the rest are one tick apart, 42 ns, as before (ADR 0029).
    #[test]
    fn a_counter_read_faster_than_it_ticks_steps_by_one_tick() {
        let runner = counts(&[(1, 12_000), (2, 30)]);
        assert_eq!(step_ticks(&runner, 987_970, STEP_PAIRS), 1);
    }

    /// Just under half the pairs unmoved is still a counter that advances on most
    /// readings, read by the divisor: the bench host with a slow half would read 37.
    #[test]
    fn the_jump_rule_needs_most_pairs_unmoved() {
        let half = counts(&[(41, 200_000), (42, 300_000)]);
        assert_eq!(
            step_ticks(&half, 500_000, STEP_PAIRS),
            1,
            "41 and 42 divide to 1"
        );
        assert_eq!(step_ticks(&half, 500_001, STEP_PAIRS), 41);
    }

    /// An advance in fewer than one pair in a thousand is an interrupt or a migration,
    /// whatever it would divide the step to; at one in a thousand it counts.
    #[test]
    fn a_rare_advance_is_left_out() {
        assert_eq!(
            step_ticks(&counts(&[(37, 998_000), (1_001, 999)]), 0, STEP_PAIRS),
            37
        );
        assert_eq!(
            step_ticks(&counts(&[(37, 998_000), (1_001, 1_000)]), 0, STEP_PAIRS),
            1
        );
    }

    /// A residual below half the most frequent advance is left out however often it
    /// occurs; at half it is a step.
    #[test]
    fn a_residual_below_half_the_mode_is_left_out() {
        assert_eq!(
            step_ticks(&counts(&[(40, 500_000), (19, 400_000)]), 0, STEP_PAIRS),
            40
        );
        assert_eq!(
            step_ticks(&counts(&[(40, 500_000), (20, 400_000)]), 0, STEP_PAIRS),
            20
        );
    }

    /// No frequent advance at all reads one tick: the floor is never zero.
    #[test]
    fn an_empty_histogram_reads_one_tick() {
        assert_eq!(step_ticks(&counts(&[]), 0, STEP_PAIRS), 1);
        assert_eq!(step_ticks(&counts(&[]), STEP_PAIRS, STEP_PAIRS), 1);
    }

    /// `parse` over `args` with a fixed profile and no `--pin`, so no CPU is read and
    /// the environment's profile plays no part.
    fn parsed(args: &[&str]) -> Result<Options, String> {
        let mut all = vec!["--profile".to_string(), "p".to_string()];
        all.extend(args.iter().map(ToString::to_string));
        parse(&all)
    }

    /// `parse` over `args` that must parse.
    fn options(args: &[&str]) -> Options {
        match parsed(args) {
            Ok(options) => options,
            Err(error) => panic!("{args:?} did not parse: {error}"),
        }
    }

    /// Without `--only`, every benchmark runs, in `Benchmark::ALL`'s order, and the
    /// profile is the one given.
    #[test]
    fn without_only_every_benchmark_runs() {
        let options = options(&[]);
        assert_eq!(options.benchmarks(), Benchmark::ALL.to_vec());
        assert_eq!(options.profile, "p");
    }

    /// `--only` with a full slot row name, as the table writes it, admits the pair
    /// that writes the row and nothing else, and the result goes under `-only`. The
    /// pair was admitted on its prefix alone, `slot_padding_64`, which the full name
    /// does not contain, so `--only slot_padding_64_load` ran nothing.
    #[test]
    fn only_with_a_full_slot_row_name_admits_its_pair() {
        for row in ["slot_padding_64_load", "slot_padding_64_store"] {
            let options = options(&["--only", row]);
            assert!(options.wanted(row));
            assert_eq!(options.benchmarks(), vec![Benchmark::Slot { spacing: 64 }]);
            assert_eq!(options.profile, "p-only");
        }
    }

    /// `--only` admits a message benchmark on either of its rows, since one run writes
    /// both, and a part of a name admits every benchmark with a row containing it.
    #[test]
    fn only_admits_a_benchmark_on_any_of_its_rows() {
        let exclusive = Benchmark::Log {
            log_type: LogType::Exclusive,
            name: "exclusive",
        };
        for row in ["claim_commit_32_exclusive", "one_way_32_exclusive"] {
            assert_eq!(options(&["--only", row]).benchmarks(), vec![exclusive]);
        }
        assert_eq!(
            options(&["--only", "slot_padding"]).benchmarks(),
            vec![
                Benchmark::Slot { spacing: 0 },
                Benchmark::Slot { spacing: 64 },
                Benchmark::Slot { spacing: 128 },
            ]
        );
        // The rows carry the body, so `--body` changes what a name must contain.
        assert_eq!(
            options(&["--only", "record_block_64", "--body", "64"]).benchmarks(),
            vec![Benchmark::Record]
        );
    }

    /// `--only` with a name no row contains is an error that lists every row of a full
    /// pass, so the operator knows what to pass, in place of a run that wrote an empty
    /// result and exited 0.
    #[test]
    fn only_with_no_matching_row_is_an_error_listing_the_rows() {
        let Err(error) = parsed(&["--only", "slot_padding_96"]) else {
            panic!("--only slot_padding_96 parsed, and no row's name contains it");
        };
        assert!(error.starts_with("--only slot_padding_96:"), "{error}");
        // The timer, two rows each of the four message benchmarks and the three slot
        // pairs, and the record row.
        let rows = options(&[]).rows();
        assert_eq!(rows.len(), 16);
        for row in rows {
            assert!(error.contains(&row), "{error} does not list {row}");
        }
    }
}
