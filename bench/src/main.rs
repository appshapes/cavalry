//! `cavalry-bench`: the latency and throughput harness `just bench` runs (CLAUDE.md,
//! gate 2; ADR 0015). The log rows drive `cavalry-shm` directly: a publisher thread
//! claims and commits into a real log buffer, a subscriber thread reads it, and a
//! conductor thread does the driver's duties, the limit and the cleaning, through a real
//! counters file. The client rows take the path a user takes: `cavalry-client` against a
//! real `cavalry-driver` in this process, publication to subscription (ADR 0021). The
//! UDP rows take the client rows' path over `cavalry:udp` on the loopback, through the
//! driver's sender and receiver, one message in flight at a time; the link rows take it
//! across the direct link between the two bench hosts to a peer, `--echo`, that echoes
//! every message (ADR 0044). Every
//! sample is one message, timed with the time stamp counter, calibrated once (ADR 0028),
//! and every number reported is a percentile or a max of an HdrHistogram, never a mean. The measured loops run under a counting
//! allocator and the run fails when they allocate, naming the benchmark whose loops did.
//!
//! `cavalry-bench --help` lists the options; `docs/how-to/run-the-bench.md` is the page.

use std::alloc::System;
use std::backtrace::Backtrace;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::net::UdpSocket;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use cavalry_client::{Cavalry, Offer, Publication, Subscription};
use cavalry_driver::{Driver, DriverThread, IdleStrategy, ThreadStart, spec};
use cavalry_ffi::harness;
use cavalry_log::catalog::CatalogWriter;
use cavalry_log::dir::LogDir;
use cavalry_log::layout::{
    IndexHeader, Recording, SEGMENT_FILE_LENGTH, index_file_name, segment_file_name,
};
use cavalry_shm::counters::generated::{
    CHANNEL_PUBLICATIONS, IMAGE_ENDED, IMAGE_TIMED_OUT, PUBLICATION_LIMIT, PUBLICATION_POSITION,
    RECORDING_SEGMENTS_PURGED, SUBSCRIPTION_POSITION,
};
use cavalry_shm::counters::{CountersFile, CountersReader};
use cavalry_shm::log::{ClaimError, HeaderFields, LogBuffers, LogType};
use hdrhistogram::Histogram;
use tracking_allocator::{AllocationGroupId, AllocationRegistry, AllocationTracker, Allocator};

#[global_allocator]
static GLOBAL: Allocator<System> = Allocator::system();

/// The allocations every thread of this process made while a [`Counting`] window was
/// open, a reallocation counted as one, counted by the hook below; a window's count is
/// the difference between its two readings.
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
/// The backtrace of the first allocation a window saw, once per process, captured in
/// the hook, so that a run which fails the allocation gate names the frame that
/// allocated and the thread it ran on, not a count alone: one allocation in 20,000,000
/// messages on the macOS gate, seen now and then, could not be placed from the count
/// (card 176; ADR 0047).
static FIRST_ALLOCATION: OnceLock<String> = OnceLock::new();

/// The hook the allocator calls for every allocation while tracking is enabled, which
/// is only inside a [`Counting`] window: it counts, and on the first one it captures
/// the backtrace. The allocator ignores the allocations the capture itself makes, so
/// the capture neither recurses nor counts. A reallocation reaches it too: the
/// allocator keeps `GlobalAlloc`'s default `realloc`, which allocates anew through the
/// hooked `alloc`, so a vector grown past its capacity counts, where `stats_alloc`'s
/// count left it out (ADR 0047).
struct CountingTracker;

impl AllocationTracker for CountingTracker {
    fn allocated(
        &self,
        _addr: usize,
        _object_size: usize,
        _wrapped_size: usize,
        _group: AllocationGroupId,
    ) {
        if ALLOCATIONS.fetch_add(1, Ordering::Relaxed) == 0 {
            let _ = FIRST_ALLOCATION.set(Backtrace::force_capture().to_string());
        }
    }

    fn deallocated(
        &self,
        _addr: usize,
        _object_size: usize,
        _wrapped_size: usize,
        _source: AllocationGroupId,
        _current: AllocationGroupId,
    ) {
    }
}

/// A counting window around a measured loop: opened when the warm-up ends, closed when
/// the loop ends. Every thread's allocations count while it is open, the driver's and
/// the subscriber's included, as the gate's rule says; nothing counts outside one.
struct Counting {
    start: usize,
}

impl Counting {
    fn open() -> Self {
        let start = ALLOCATIONS.load(Ordering::Relaxed);
        AllocationRegistry::enable_tracking();
        Self { start }
    }

    /// Closes the window and returns the allocations it saw.
    fn close(&self) -> usize {
        AllocationRegistry::disable_tracking();
        ALLOCATIONS.load(Ordering::Relaxed).wrapping_sub(self.start)
    }
}

/// A window closes when it is dropped as well, so a measured loop that returns early
/// through `?` leaves no tracking enabled behind it. Closing twice is harmless.
impl Drop for Counting {
    fn drop(&mut self) {
        AllocationRegistry::disable_tracking();
    }
}

/// The first allocation's backtrace once the hook has captured it: `None` when no
/// window counted an allocation. Otherwise the thread that allocated first is still
/// symbolising the capture, or has finished, and this waits up to five seconds for it.
fn first_allocation() -> Option<&'static str> {
    if ALLOCATIONS.load(Ordering::Relaxed) == 0 {
        return None;
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(first) = FIRST_ALLOCATION.get() {
            return Some(first.as_str());
        }
        if Instant::now() > deadline {
            return None;
        }
        thread::yield_now();
    }
}

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
/// The share of `--messages` the UDP rows run: one in ten. The rows keep one message in
/// flight, so a message is the path's latency, about ten microseconds on the loopback
/// of a bench host against a tenth of one through shared memory for the client rows,
/// and a tenth of the messages measures the same percentile in the same time (ADR
/// 0044). The link rows take `--messages` as given, since they run alone.
const UDP_SHARE: u64 = 10;
/// The warm-up of every UDP row, on the loopback and across the link, in place of the
/// rotation the shared-memory rows warm up with: a rotation's warm-up is so that no
/// measured message first-touches a page, and the driver pre-touches the publication's
/// log and the image's when it makes them (ADR 0027), so this many messages warm the
/// caches and the connection, a tenth of a second on the loopback and a second across
/// the link, where a rotation's 786,432 messages would be most of a run (ADR 0044).
const UDP_WARMUP: u64 = 10_000;
/// How long the UDP rows wait for the loopback stream to connect, the receiver's status
/// message answering the sender's setup frame, which goes every 100 ms.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the link rows and the echo peer wait for each other: the two jobs of one
/// workflow run start when the single-host bench jobs are done, within seconds of each
/// other, and a person starting the two by hand types two commands (ADR 0044).
const LINK_TIMEOUT: Duration = Duration::from_secs(600);
/// The least body the link rows carry: this host's clock stamp, this host's wall-clock
/// stamp and the peer's wall-clock stamp, eight bytes each.
const LINK_BODY_LEAST: usize = 24;

struct Options {
    messages: u64,
    /// `--warmup`, or `None` for the default of one rotation (`Options::warmup()`).
    warmup_option: Option<u64>,
    body: usize,
    term_length: u32,
    profile: String,
    profile_suffix: String,
    out: Option<PathBuf>,
    /// The CPUs from `--pin`: the publisher's, the subscriber's, the driver's
    /// conductor's in the IPC rows and the sender's in the UDP rows, and the receiver's
    /// in the UDP rows when a fourth is given; the driver's recorder, replayer and
    /// the client's conductor run on the lowest CPU outside them, with the sender and
    /// the receiver in the IPC rows and the driver's conductor in the UDP rows.
    pin: Option<Pins>,
    /// That lowest CPU outside the pins, from the process's allowed set as it was before
    /// this thread pinned itself: the affinity a thread reads is its own.
    housekeeping: Option<usize>,
    /// `--link-here` and `--link-peer`: the link rows across the direct link in place
    /// of every single-host row, or with `--echo` the peer that echoes them.
    link: Option<Link>,
    /// How many times every benchmark runs; the reported run is the median by p99.99,
    /// and the result lists every run's.
    repeat: u32,
    /// `--only`: run only the benchmarks with a row whose name contains this, for a
    /// profile of one row; the result is written under the profile with `-only`
    /// appended, so it is never compared as a full run. A value no row's name contains
    /// is an error from `parse`, which lists the rows.
    only: Option<String>,
    /// `--purging`: the recorded rows run again as `recorded_purging`, while one `purge
    /// segments` removes a stopped recording's segment files on the driver's replayer,
    /// one file per duty cycle, for the whole run (card 177; ADR 0031's Evidence).
    purging: bool,
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
    /// The client path through a real driver, with the recorder on when `recorded`, and
    /// with `purging` a purge of a stopped recording's segment files running on the
    /// driver's replayer throughout (card 177): the same two rows under `name`,
    /// `client`, `recorded` or `recorded_purging`.
    Client {
        recorded: bool,
        purging: bool,
        name: &'static str,
    },
    /// The client path over `cavalry:udp` on the loopback, through the driver's sender
    /// and receiver, one message in flight (ADR 0044): the same two rows under `udp`.
    Udp,
    /// The client path over a control channel to two destinations, each a subscription
    /// of this driver on a loopback port, under `fc=min`, one message in flight,
    /// measured at the first (ADR 0053): the same two rows under `udp_mdc`, reported
    /// and never gated (ADR 0052, feature 1).
    UdpMdc,
    /// The client path through the C ABI (ADR 0051): `cavalry-ffi`'s `extern "C"`
    /// functions in this binary, so the rows need no C compiler and the allocation gate
    /// counts the ABI's path as it counts the client's; the same two rows under `ffi`.
    Ffi,
    /// A load and a store from two threads this many bytes apart (ADR 0011): the
    /// `slot_padding_<spacing>_load` and `slot_padding_<spacing>_store` rows.
    Slot { spacing: usize },
    /// The recorder's block path (ADR 0025): the `record_block_<body>` row.
    Record,
}

impl Benchmark {
    /// Every benchmark of a full pass, in the order they run: the timer, both log
    /// types, the client path with and without a recording on, the client path over
    /// UDP on the loopback, the client path through the C ABI, the three slot
    /// spacings, and last the recorder's block path, so its writes reach no other row.
    const ALL: [Benchmark; 12] = [
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
            purging: false,
            name: "client",
        },
        Benchmark::Client {
            recorded: true,
            purging: false,
            name: "recorded",
        },
        Benchmark::Udp,
        Benchmark::UdpMdc,
        Benchmark::Ffi,
        Benchmark::Slot { spacing: 0 },
        Benchmark::Slot { spacing: 64 },
        Benchmark::Slot { spacing: 128 },
        Benchmark::Record,
    ];

    /// The recorded rows with a purge running on the replayer (card 177), which
    /// `--purging` adds after the recorded rows.
    const PURGING: Benchmark = Benchmark::Client {
        recorded: true,
        purging: true,
        name: "recorded_purging",
    };

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
            Benchmark::Udp => vec![
                format!("claim_commit_{body}_udp"),
                format!("one_way_{body}_udp"),
            ],
            Benchmark::UdpMdc => vec![
                format!("claim_commit_{body}_udp_mdc"),
                format!("one_way_{body}_udp_mdc"),
            ],
            Benchmark::Ffi => vec![
                format!("claim_commit_{body}_ffi"),
                format!("one_way_{body}_ffi"),
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

    /// Every benchmark this run can admit, in the order they run: [`Benchmark::ALL`],
    /// with [`Benchmark::PURGING`] after the recorded rows under `--purging`.
    fn all(&self) -> Vec<Benchmark> {
        let mut all = Benchmark::ALL.to_vec();
        if self.purging {
            let after = all
                .iter()
                .position(|benchmark| matches!(benchmark, Benchmark::Client { recorded: true, .. }))
                .map_or(all.len(), |index| index + 1);
            all.insert(after, Benchmark::PURGING);
        }
        all
    }

    /// The benchmarks of a pass, in the order they run: all of [`Options::all`]
    /// without `--only`, else those with a row `--only` admits. Empty when `--only`
    /// names no row, which `parse` refuses.
    fn benchmarks(&self) -> Vec<Benchmark> {
        self.all()
            .into_iter()
            .filter(|benchmark| benchmark.rows(self.body).iter().any(|row| self.wanted(row)))
            .collect()
    }

    /// Every row a full pass writes, in the order the benchmarks run: what `--only` is
    /// matched against, and what the error lists when it matches none.
    fn rows(&self) -> Vec<String> {
        self.all()
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
                    for the default body and term), and 10000 for the UDP rows, whose
                    logs the driver pre-touches
  --body N          body bytes per message, at least 8 for the timestamp (default 32)
  --term-length N   term length of the log buffer, a power of two from 65536 (default 4194304)
  --profile NAME    the baseline profile; default $CAVALRY_BENCH_PROFILE or local-<os>-<arch>
  --profile-suffix S  appended to the profile; `just bench-unchecked` passes -unchecked
  --out DIR         where the result JSON goes (default bench/results); --no-write writes none
  --pin P,S,C[,R]   pin the publisher and the subscriber to CPUs P and S, the driver's
                    conductor to C in the IPC rows, and in the UDP rows the sender to C
                    and the receiver to R, distinct CPUs, Linux only; the driver's
                    recorder and replayer and the client's conductor run on the lowest
                    CPU outside them, the sender and the receiver with them in the IPC
                    rows and the driver's conductor with them in the UDP rows; three CPUs
                    refuse a UDP row; the bench host passes 8,10,12,14 (ADR 0016, ADR 0044)
  --link-here H:P   with --link-peer: run the link rows alone, udp_round_trip_<body> and
                    udp_one_way_<body>, across the direct link to a peer running --echo,
                    this host's link address and port for the echo to come back to, and
                    the peer's; the profile gets -link appended; --body is at least 24
  --link-peer H:P   the peer's link address and the port its receive endpoint binds
  --echo            with both link ends: be the peer, subscribing on --link-here and
                    echoing every message to --link-peer until the stream ends, with no
                    result written; the bench host runs it on grip (ADR 0044)
  --repeat N        run every benchmark N times, report each one's median run by p99.99,
                    and list every run's p99.99 in the result (default 1; CI passes 21)
  --only NAME       run only the benchmarks whose name contains NAME, for a profile of
                    one row; the result goes under the profile with -only appended, and
                    a NAME that no row's name contains is an error listing the rows
  --purging         run the recorded rows again as claim_commit_<body>_recorded_purging and
                    one_way_<body>_recorded_purging, with one `purge segments` of a stopped
                    recording's sparse segment files running on the driver's replayer, one
                    file per duty cycle, from before the warm-up until past the measured
                    loop; the recording has as many files as the bench directory's file
                    system removes in about 2.5 s, measured before each run (8,192 at
                    least, 1,000,000 at most); the lines under the table say how many
                    files went during the loop and whether the purge outlasted it; the
                    rows are reported until a baseline holds them
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
        link: None,
        repeat: 1,
        only: None,
        purging: false,
    };
    let mut link_here: Option<String> = None;
    let mut link_peer: Option<String> = None;
    let mut echo = false;
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
            "--purging" => options.purging = true,
            "--link-here" => link_here = Some(value("--link-here")?.clone()),
            "--link-peer" => link_peer = Some(value("--link-peer")?.clone()),
            "--echo" => echo = true,
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
    options.link = match (link_here, link_peer, echo) {
        (None, None, false) => None,
        (Some(here), Some(peer), echo) => Some(Link { here, peer, echo }),
        _ => {
            return Err(
                "--link-here and --link-peer go together, each <host>:<port>, and --echo needs \
                 both: the link rows run between this host and a peer running --echo (ADR 0044)"
                    .to_string(),
            );
        }
    };
    if let Some(link) = &options.link {
        if options.only.is_some() {
            return Err(
                "--only chooses among the single-host rows, and the link rows run alone: drop \
                 --only, or the link ends"
                    .to_string(),
            );
        }
        if options.body < LINK_BODY_LEAST {
            return Err(format!(
                "--body must be at least {LINK_BODY_LEAST} for the link rows: this host's two \
                 stamps and the peer's, eight bytes each"
            ));
        }
        if !link.echo {
            options.profile_suffix.push_str("-link");
        }
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
    if let Some(pin) = options.pin {
        let udp = options.link.is_some()
            || options
                .benchmarks()
                .iter()
                .any(|benchmark| matches!(benchmark, Benchmark::Udp | Benchmark::UdpMdc));
        check_pins(pin, udp)?;
    }
    // Read here, before `main` pins this thread to the publisher's CPU, since the
    // allowed set a thread reads is its own.
    options.housekeeping = options.pin.map(housekeeping_cpu).transpose()?;
    Ok(options)
}

/// `--pin` with three CPUs and a UDP row to run is an error: the receiver would have
/// to share a CPU with a measured thread or go with the housekeeping threads, and a
/// number taken off an isolated core is not comparable with the baseline.
fn check_pins(pin: Pins, udp: bool) -> Result<(), String> {
    if udp && pin.receiver.is_none() {
        return Err(format!(
            "--pin {}: the UDP rows need a fourth CPU for the receiver, --pin P,S,C,R; pass four, \
             or --only a row without udp in its name (ADR 0044); see \
             docs/how-to/run-the-bench.md#when-it-does-not-work",
            pin.list()
        ));
    }
    Ok(())
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

    /// The UDP rows' warm-up: `--warmup`, or [`UDP_WARMUP`].
    fn udp_warmup(&self) -> u64 {
        self.warmup_option.unwrap_or(UDP_WARMUP)
    }

    /// The UDP rows' messages per run: a tenth of `--messages`, [`UDP_SHARE`].
    fn udp_messages(&self) -> u64 {
        (self.messages / UDP_SHARE).max(1)
    }
}

/// `--pin P,S,C[,R]`: where each measured thread runs (ADR 0016, ADR 0044).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Pins {
    publisher: usize,
    subscriber: usize,
    /// The driver's conductor in the IPC rows; the sender in the UDP rows, whose
    /// conductor runs with the housekeeping threads, since a bench host has four
    /// isolated cores and the publisher, the subscriber, the sender and the receiver
    /// take them.
    conductor: usize,
    /// The receiver in the UDP rows; `None` for `--pin` with three CPUs, which refuses
    /// a UDP row (`check_pins`).
    receiver: Option<usize>,
}

impl Pins {
    fn contains(self, id: usize) -> bool {
        self.publisher == id
            || self.subscriber == id
            || self.conductor == id
            || self.receiver == Some(id)
    }

    /// The CPUs as `--pin` spells them, `8,10,12,14`.
    fn list(self) -> String {
        let mut text = format!("{},{},{}", self.publisher, self.subscriber, self.conductor);
        if let Some(receiver) = self.receiver {
            let _ = write!(text, ",{receiver}");
        }
        text
    }

    /// The CPUs as the result writes them, `[8, 10, 12, 14]`.
    fn json(self) -> String {
        let mut text = format!(
            "[{}, {}, {}",
            self.publisher, self.subscriber, self.conductor
        );
        if let Some(receiver) = self.receiver {
            let _ = write!(text, ", {receiver}");
        }
        text.push(']');
        text
    }
}

/// The two ends of the direct link, `--link-here` and `--link-peer`, each `<host>:<port>`
/// as a `cavalry:udp` endpoint names them: this host's receive endpoint and the peer's.
/// The link rows publish to the peer's and subscribe on this host's; the echo peer does
/// the reverse (ADR 0044).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Link {
    here: String,
    peer: String,
    echo: bool,
}

impl Link {
    fn here_channel(&self) -> String {
        format!("cavalry:udp?endpoint={}", self.here)
    }

    /// This host's address on the link, without its port: where the multi-destination
    /// cast rows' control channel and local destination are.
    fn here_host(&self) -> String {
        self.here
            .rsplit_once(':')
            .map_or_else(|| self.here.clone(), |(host, _)| host.to_string())
    }

    fn peer_channel(&self) -> String {
        format!("cavalry:udp?endpoint={}", self.peer)
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

/// `--pin P,S,C[,R]`: three or four distinct CPU numbers.
fn parse_pin(text: &str) -> Result<Pins, String> {
    let cpus: Vec<usize> = text
        .split(',')
        .map(|part| {
            part.trim()
                .parse::<usize>()
                .map_err(|e| format!("--pin: {part:?}: {e}"))
        })
        .collect::<Result<_, _>>()?;
    let pins = match cpus[..] {
        [publisher, subscriber, conductor] => Pins {
            publisher,
            subscriber,
            conductor,
            receiver: None,
        },
        [publisher, subscriber, conductor, receiver] => Pins {
            publisher,
            subscriber,
            conductor,
            receiver: Some(receiver),
        },
        _ => {
            return Err(format!(
                "--pin takes three or four CPUs, publisher,subscriber,conductor[,receiver]; got {}",
                cpus.len()
            ));
        }
    };
    let distinct = cpus
        .iter()
        .all(|cpu| cpus.iter().filter(|other| *other == cpu).count() == 1);
    if !distinct {
        return Err(
            "--pin: the CPUs must differ; two spinning threads on one CPU starve each other"
                .to_string(),
        );
    }
    Ok(pins)
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

/// The lowest CPU in the calling thread's allowed set outside the pins: where the
/// driver's recorder and replayer and the client's conductor run under `--pin`, with
/// the sender and the receiver in the IPC rows and the driver's conductor in the UDP
/// rows, so that a pinned CPU holds one measured thread alone (card 136, ADR 0044).
/// Read before any thread pins itself: the bench host's shells and runner start with
/// the housekeeping CPUs alone in their set, and a thread already pinned to one CPU
/// reads a set of one.
fn housekeeping_cpu(pin: Pins) -> Result<usize, String> {
    core_affinity::get_core_ids()
        .unwrap_or_default()
        .into_iter()
        .map(|core| core.id)
        .filter(|id| !pin.contains(*id))
        .min()
        .ok_or_else(|| {
            format!(
                "--pin: no CPU outside {} in the process's allowed set for the \
                 driver's recorder and replayer and the client's conductor; \
                 run the harness from a shell \
                 whose allowed set holds a housekeeping CPU as well: `taskset -cp $$` prints \
                 the shell's, and `taskset -c 0-7 just bench --pin 8,10,12,14` runs with CPUs 0 \
                 to 7 in it (docs/how-to/run-the-bench.md#when-it-does-not-work)",
                pin.list()
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
    receiver: AtomicUsize,
}

impl DriverPins {
    fn new() -> Self {
        DriverPins {
            conductor: AtomicUsize::new(0),
            recorder: AtomicUsize::new(0),
            replayer: AtomicUsize::new(0),
            sender: AtomicUsize::new(0),
            receiver: AtomicUsize::new(0),
        }
    }

    /// The slot of `thread`.
    fn slot(&self, thread: DriverThread) -> &AtomicUsize {
        match thread {
            DriverThread::Conductor => &self.conductor,
            DriverThread::Recorder => &self.recorder,
            DriverThread::Replayer => &self.replayer,
            DriverThread::Sender => &self.sender,
            DriverThread::Receiver => &self.receiver,
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
                    DriverThread::Receiver => "receiver",
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
    /// The purge that ran through the loop, under `--purging` (card 177).
    purged: Option<Purged>,
}

/// What a purge running through a measured loop did (card 177): the segment files it
/// removed while the loop ran, read from `recording.segments_purged` at the loop's two
/// ends, the files it had to remove, and whether it was still removing them when the
/// loop ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Purged {
    during: u64,
    pile: u64,
    outlasted: bool,
}

/// A publisher, a subscriber and a conductor over a real log buffer and counters file
/// under the system's temporary directory.
fn bench_log(options: &Options, log_type: LogType) -> Result<LogRun, String> {
    with_bench_dir(&format!("{log_type:?}"), |dir| {
        run_log(options, log_type, dir)
    })
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
        let cpu = options.pin.map(|pin| pin.subscriber);
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
        let cpu = options.pin.map(|pin| pin.conductor);
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
    let mut counting: Option<Counting> = None;
    let mut start = Instant::now();
    for i in 0..total {
        if i == warmup {
            counting = Some(Counting::open());
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
    let allocations = counting.as_ref().map_or(0, Counting::close);
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
        purged: None,
    })
}

/// The client rows: a real driver and a real client in this process, a publication and a
/// subscription on one stream, the publisher on this thread and the subscriber on its
/// own, each message claimed, written in place and committed, then delivered to the
/// subscription's handler (ADR 0021). Under `--pin` the driver's conductor pins itself
/// to the third CPU and the driver's recorder, replayer, sender and receiver and the
/// client's conductor run on the lowest CPU outside the pins, as `run_client` says and does; a thread inherits the
/// affinity of the thread that spawns it, which is what the placement arranges. The log
/// is the driver's, 64 MiB terms (`spec/client-protocol.md`,
/// "Sizes"), so the warm-up is one rotation of that log, not of `--term-length`'s, for the
/// same reason as the log rows': no measured message first-touches a page.
fn bench_client(
    options: &Options,
    recorded: bool,
    purging: bool,
    name: &str,
) -> Result<LogRun, String> {
    with_bench_dir(name, |dir| run_client(options, dir, recorded, purging))
}

/// How long the purge of a pile is sized to last, in seconds. The pile has as many
/// files as the bench directory's file system removes in this long, by a probe made
/// before the pile is built ([`removal_rate`]), because the rate is the file system's
/// and no one number of files serves two of them: a laptop's disk removed about
/// 22,000 sparse files a second through the replayer and a Linux host's `/dev/shm`
/// about 590,000 (card 177), so a pile that lasts the laptop five seconds is gone from
/// `/dev/shm` before the warm-up ends. The purge must outlast the warm-up and the
/// measured loop, about half a second for 5,000,000 messages on a bench host, and one
/// `purge segments` removes the whole pile, so its reply must come inside the client's
/// 10 s response timeout: the replayer removes a file per duty cycle and never faster
/// than the probe, which leaves it up to four times slower before the timeout.
const PILE_SECONDS: f64 = 2.5;
/// The sparse files the probe creates and removes to measure the rate.
const PILE_PROBE: u64 = 4_096;
/// The fewest files in a pile, for a file system slower than any measured.
const PILE_LEAST: u64 = 8_192;
/// The most files in a pile: on a memory-backed file system each file is an inode held
/// in memory, about a kibibyte, so a million are about 1 GiB for the seconds the pile
/// lives, and a `/dev/shm` holds as many inodes as half the host's memory has pages:
/// about two million on `grip`'s 16 GB, which a larger pile would run out of. A million
/// last `/dev/shm` about 1.7 s at the rate above, three times a bench host's row.
const PILE_MOST: u64 = 1_000_000;
/// The pile's term length, the smallest the durable log admits: nothing reads its
/// bytes, and a segment file is at least a term.
const PILE_TERM_LENGTH: i32 = 64 * 1024;

/// The pile `--purging` purges (card 177): a stopped recording in the log directory,
/// in the driver's catalog before the driver opens it, with `segments` segment files.
struct Pile {
    recording_id: i64,
    segments: u64,
    segment_length: i64,
}

impl Pile {
    /// The position past every file: what `purge segments` is asked to retain from.
    fn stop_position(&self) -> i64 {
        self.segment_length.saturating_mul(self.segments as i64)
    }
}

/// The sparse files a second the file system that will hold `log_dir` removes: the
/// probe creates [`PILE_PROBE`] files of a segment's length beside it, never written,
/// as the pile's are, and times their removal alone. The replayer removes the pile's
/// files with the same call, one per duty cycle, so this is the most it can do.
fn removal_rate(log_dir: &std::path::Path) -> Result<f64, String> {
    let probe = log_dir.with_extension("probe");
    let _ = std::fs::remove_dir_all(&probe);
    std::fs::create_dir_all(&probe).map_err(|e| format!("creating {}: {e}", probe.display()))?;
    let length = SEGMENT_FILE_LENGTH as u64;
    let paths: Vec<PathBuf> = (0..PILE_PROBE).map(|k| probe.join(k.to_string())).collect();
    for path in &paths {
        std::fs::File::create(path)
            .and_then(|file| file.set_len(length))
            .map_err(|e| format!("creating the probe's {}: {e}", path.display()))?;
    }
    let started = Instant::now();
    for path in &paths {
        std::fs::remove_file(path)
            .map_err(|e| format!("removing the probe's {}: {e}", path.display()))?;
    }
    let seconds = started.elapsed().as_secs_f64();
    let _ = std::fs::remove_dir_all(&probe);
    Ok(PILE_PROBE as f64 / seconds.max(f64::EPSILON))
}

/// The files of a pile for a file system that removes `rate` files a second: what it
/// removes in [`PILE_SECONDS`], within [`PILE_LEAST`] and [`PILE_MOST`].
fn pile_segments(rate: f64) -> u64 {
    ((rate * PILE_SECONDS) as u64).clamp(PILE_LEAST, PILE_MOST)
}

/// Builds a pile of `segments` segment files in `log_dir` before any driver opens it: a
/// recording added to the catalog and stopped at the end of its last file, its index
/// header, and each file created at the segment length and never written, so it is
/// sparse and costs no block; the driver's recovery pass leaves a stopped slot whose
/// checksums hold alone, and a purge removes files without reading them
/// (`spec/durable-log.md`, "Recovery", "The driver").
fn build_pile(log_dir: &std::path::Path, segments: u64) -> Result<Pile, String> {
    let now = realtime_nanos() as i64;
    let (log, _) = LogDir::open(log_dir, now).map_err(|e| e.to_string())?;
    // The directory's lock goes with it; the catalog is written below without one,
    // since no driver has the directory yet.
    drop(log);
    let mut catalog = CatalogWriter::open(log_dir, now).map_err(|e| e.to_string())?;
    let recording_id = catalog
        .add(&Recording {
            recording_id: 0,
            start_timestamp: now,
            start_position: 0,
            initial_term_id: 0,
            term_length: PILE_TERM_LENGTH,
            mtu_length: MTU as i32,
            session_id: 0,
            stream_id: STREAM_ID,
            channel: "cavalry:ipc",
            source_identity: "cavalry:ipc",
        })
        .map_err(|e| e.to_string())?;
    let pile = Pile {
        recording_id,
        segments,
        segment_length: i64::from(SEGMENT_FILE_LENGTH),
    };
    catalog
        .stop(recording_id, pile.stop_position(), now)
        .map_err(|e| e.to_string())?;
    drop(catalog);
    let index = log_dir.join(index_file_name(recording_id));
    let header = IndexHeader::new(recording_id).map_err(|e| e.to_string())?;
    std::fs::write(&index, header.encode())
        .map_err(|e| format!("writing {}: {e}", index.display()))?;
    for k in 0..segments {
        let base = pile.segment_length.saturating_mul(k as i64);
        let path = log_dir.join(segment_file_name(recording_id, base));
        std::fs::File::create(&path)
            .and_then(|file| file.set_len(pile.segment_length as u64))
            .map_err(|e| format!("creating the pile's {}: {e}", path.display()))?;
    }
    Ok(pile)
}

/// The purge of the pile, on a thread and a client of its own: one `purge segments`
/// for every file, sent before the warm-up and answered once the replayer has removed
/// the last file, so no command is admitted while the loop runs and nothing allocates
/// for one (the replayer lists a command's files once, when it is admitted). The
/// thread keeps its client open after the reply and closes it only once it is
/// released, which the publisher does after its counting window: a client that closes
/// sends the driver's conductor a command, and what the conductor does for a command
/// allocates and would be counted (card 176).
struct Purger {
    thread: Option<JoinHandle<Result<(), String>>>,
    release: Arc<AtomicBool>,
}

impl Purger {
    fn spawn(
        dir: &std::path::Path,
        pile: &Pile,
        housekeeping: Option<usize>,
    ) -> Result<Self, String> {
        let dir = dir.to_path_buf();
        let recording_id = pile.recording_id;
        let position = pile.stop_position();
        let release = Arc::new(AtomicBool::new(false));
        let released = Arc::clone(&release);
        let thread = thread::Builder::new()
            .name("bench-purger".into())
            .spawn(move || {
                pin_to(housekeeping)?;
                let mut client = Cavalry::connect_to(&dir).map_err(|e| e.to_string())?;
                let purged = client
                    .purge_segments(recording_id, position)
                    .map_err(|e| format!("purging the pile: {e}"));
                while !released.load(Ordering::Acquire) {
                    thread::sleep(Duration::from_millis(1));
                }
                client.close().map_err(|e| e.to_string())?;
                purged.map(|_| ())
            })
            .map_err(|e| format!("spawning the purger: {e}"))?;
        Ok(Self {
            thread: Some(thread),
            release,
        })
    }

    /// The purger's own error when its thread has ended, which before the release is
    /// only ever a failure: a connect or a purge the driver refused.
    fn failed(&mut self) -> Option<String> {
        if !self.thread.as_ref().is_some_and(JoinHandle::is_finished) {
            return None;
        }
        self.finish().err()
    }

    /// Releases the thread, which closes its client, and waits for it: the purge's
    /// own result, once the reply has come.
    fn finish(&mut self) -> Result<(), String> {
        self.release.store(true, Ordering::Release);
        match self.thread.take().map(JoinHandle::join) {
            Some(Ok(result)) => result,
            Some(Err(_)) => Err("the purger thread panicked".to_string()),
            None => Ok(()),
        }
    }
}

impl Drop for Purger {
    /// On an early return the thread is released and waited for here, while the driver
    /// it talks to is still up: locals drop in reverse, and the driver is declared
    /// before the purger.
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

/// The client rows, and with `recorded` the same rows while the driver's recorder writes
/// the stream to the log directory beside the bench directory (ADR 0025): the recorder is
/// one more reader of the log, whose position the publisher's limit respects, and its
/// writes run on the driver's recorder thread. Under `--pin` the driver's conductor has
/// the third CPU to itself, and the recorder, the replayer, the sender, the receiver
/// and the client's conductor run on the lowest CPU outside the pins: the threads on
/// one pinned CPU kept it ticking and waking within itself (card 136); the replayer
/// holds no replay in any row and stays parked, and the sender and the receiver hold
/// no endpoint and park too. The recording must reach the publisher's final
/// position before the run counts, so a recorder that lost a byte or fell behind fails
/// the run, never a row.
fn run_client(
    options: &Options,
    dir: &std::path::Path,
    recorded: bool,
    purging: bool,
) -> Result<LogRun, String> {
    let housekeeping = options.housekeeping;
    let conductor_cpu = options.pin.map(|pin| pin.conductor);
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
            // The sender and the receiver hold no endpoint in these rows and park, so
            // they go with the housekeeping threads; the UDP rows give each a core of
            // its own (`udp_thread_start`).
            let cpu = match thread {
                DriverThread::Conductor => conductor_cpu,
                DriverThread::Recorder
                | DriverThread::Replayer
                | DriverThread::Sender
                | DriverThread::Receiver => housekeeping,
            };
            if pin_to(cpu).is_err() {
                pins.slot(thread)
                    .store(cpu.map_or(0, |cpu| cpu + 1), Ordering::Release);
            }
        })
    };
    // This thread's affinity is what the client's conductor inherits.
    pin_to(housekeeping)?;
    // The log directory is the bench directory's sibling, not under it: the driver
    // removes a directory of its own that has no command ring before it starts
    // (`conductor::prepare_dir`), which would take a log directory under it, and the
    // pile is built there before the driver opens it. `with_bench_dir` removes both.
    let log_dir = dir.with_extension("log");
    let pile = if purging {
        Some(build_pile(
            &log_dir,
            pile_segments(removal_rate(&log_dir)?),
        )?)
    } else {
        None
    };
    let driver = Driver::start_with_threads(dir, &log_dir, IdleStrategy::Backoff, on_thread_start)
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
    // The purge starts here, before the warm-up, and runs on the driver's replayer
    // through the loop; the counter says how much of the pile went during it.
    let mut purger = pile
        .as_ref()
        .map(|pile| Purger::spawn(dir, pile, housekeeping))
        .transpose()?;
    let counters = pile
        .as_ref()
        .map(|_| CountersReader::open(dir).map_err(|e| e.to_string()))
        .transpose()?;
    let purged_at = |counters: &Option<CountersReader>| {
        counters.as_ref().map_or(Ok(0), |reader| {
            reader
                .read_acquire(RECORDING_SEGMENTS_PURGED.record())
                .map_err(|e| e.to_string())
        })
    };
    let mut purged_before: u64 = 0;
    // The replayer lists the pile's files once, when the command is admitted, which
    // allocates and takes a while for a large pile; the loop starts once the first file
    // has gone, so the listing is behind it and only removals run through it.
    if counters.is_some() {
        let deadline = Instant::now() + Duration::from_secs(60);
        while purged_at(&counters)? == 0 {
            if let Some(message) = purger.as_mut().and_then(Purger::failed) {
                return Err(message);
            }
            if Instant::now() > deadline {
                return Err("the purge removed no file of the pile in 60 s; see \
                            docs/how-to/run-the-bench.md#when-it-does-not-work"
                    .to_string());
            }
            thread::sleep(Duration::from_micros(200));
        }
    }
    pin_to(options.pin.map(|pin| pin.publisher))?;
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
        let cpu = options.pin.map(|pin| pin.subscriber);
        thread::Builder::new()
            .name("client-subscriber".into())
            // The subscription comes back with the histogram and is closed by the
            // publisher's thread after the counting window, as the UDP rows' is:
            // dropped on this thread's way out, it wrote `remove subscription` onto the
            // driver's command ring while the publisher was still inside the window
            // once in twenty million messages, and the driver's conductor ran
            // `drop_subscription` on its next duty cycle; that allocates, and the
            // window counted it (card 176).
            .spawn(move || -> Result<(Subscription, Histogram<u64>), String> {
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
                Ok((subscription, hist))
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
    let mut counting: Option<Counting> = None;
    let mut start = Instant::now();
    for i in 0..total {
        if i == warmup {
            purged_before = purged_at(&counters)? as u64;
            counting = Some(Counting::open());
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
    // The counter at the loop's end, read before the window closes so that it is the
    // loop's and not the close's: files of the pile still to go mean the purge outlasted
    // the loop, exactly, where the reply's arrival would say so a duty cycle or two late.
    let purged_after = purged_at(&counters)? as u64;
    let allocations = counting.as_ref().map_or(0, Counting::close);
    let (subscription, one_way) = match subscriber.join() {
        Ok(Ok(returned)) => returned,
        failed => return Err(subscriber_failure(failed)),
    };
    // The window is closed, so the purger may close its client; its result is the
    // purge's own, the reply that the replayer has removed the last file.
    let purged = match (purger.as_mut(), pile.as_ref()) {
        (Some(purger), Some(pile)) => {
            purger.finish()?;
            Some(Purged {
                during: purged_after.saturating_sub(purged_before),
                pile: pile.segments,
                outlasted: purged_after < pile.segments,
            })
        }
        _ => None,
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
    subscription.close().map_err(|e| e.to_string())?;
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
        purged,
    })
}

fn bench_ffi(options: &Options) -> Result<LogRun, String> {
    with_bench_dir("ffi", |dir| run_ffi(options, dir))
}

/// The client rows through the C ABI (ADR 0051): the same driver, client, subscription
/// and publication as `run_client`'s, reached through `cavalry-ffi`'s `extern "C"`
/// functions in this binary, behind its `harness` module's safe methods, since this
/// crate may hold no `unsafe`. The stamp crosses the ABI in the claimed body, and the
/// subscriber's C fragment handler hands it back. The driver's
/// threads, the pins, the warm-up, the counting window and the stall bound are the
/// client rows', so the two sets of rows differ by the boundary alone.
fn run_ffi(options: &Options, dir: &std::path::Path) -> Result<LogRun, String> {
    let housekeeping = options.housekeeping;
    let conductor_cpu = options.pin.map(|pin| pin.conductor);
    let pins = Arc::new(DriverPins::new());
    let on_thread_start: ThreadStart = {
        let pins = Arc::clone(&pins);
        Arc::new(move |thread| {
            let cpu = match thread {
                DriverThread::Conductor => conductor_cpu,
                DriverThread::Recorder
                | DriverThread::Replayer
                | DriverThread::Sender
                | DriverThread::Receiver => housekeeping,
            };
            if pin_to(cpu).is_err() {
                pins.slot(thread)
                    .store(cpu.map_or(0, |cpu| cpu + 1), Ordering::Release);
            }
        })
    };
    pin_to(housekeeping)?;
    let driver =
        Driver::start_with_threads(dir, dir.join("log"), IdleStrategy::Backoff, on_thread_start)
            .map_err(|e| e.to_string())?;
    let mut client = harness::Client::connect(dir)?;
    let mut subscription = client.add_subscription("cavalry:ipc", STREAM_ID)?;
    pins.check(DriverThread::Conductor)?;
    let mut publication = client.add_publication("cavalry:ipc", STREAM_ID)?;
    pin_to(options.pin.map(|pin| pin.publisher))?;
    let deadline = Instant::now() + Duration::from_secs(10);
    while !subscription.is_connected() {
        subscription.poll(&mut |_| {}, 1)?;
        if Instant::now() > deadline {
            return Err("the subscription never connected".to_string());
        }
    }

    let warmup = options.warmup_for(spec::IPC_TERM_LENGTH);
    let total = warmup + options.messages;
    let epoch = tick();

    let subscriber = {
        let cpu = options.pin.map(|pin| pin.subscriber);
        thread::Builder::new()
            .name("ffi-subscriber".into())
            .spawn(
                move || -> Result<(harness::Subscription, Histogram<u64>), String> {
                    pin_to(cpu)?;
                    let mut hist = histogram();
                    let mut received: u64 = 0;
                    while received < total {
                        let delivered = subscription.poll(
                            &mut |body| {
                                let stamp =
                                    u64::from_le_bytes(body[..8].try_into().expect("8 bytes"));
                                if received >= warmup {
                                    hist.saturating_record(nanos(epoch).saturating_sub(stamp));
                                }
                                received += 1;
                            },
                            1024,
                        )?;
                        if delivered == 0 {
                            std::hint::spin_loop();
                        }
                    }
                    Ok((subscription, hist))
                },
            )
            .map_err(|e| e.to_string())?
    };

    let mut hist = histogram();
    let mut retries: u64 = 0;
    let mut stalled: u64 = 0;
    const STALL_CHECK: u64 = 4096;
    const STALL_LIMIT: Duration = Duration::from_secs(10);
    let mut counting: Option<Counting> = None;
    let mut start = Instant::now();
    for i in 0..total {
        if i == warmup {
            counting = Some(Counting::open());
            start = Instant::now();
        }
        let stamp = nanos(epoch);
        let t0 = tick();
        loop {
            let position = publication.claim_commit(options.body, stamp);
            if position >= 0 {
                break;
            }
            match position {
                cavalry_ffi::CAVALRY_PUBLICATION_BACK_PRESSURED
                | cavalry_ffi::CAVALRY_PUBLICATION_ADMIN_ACTION
                | cavalry_ffi::CAVALRY_PUBLICATION_NOT_CONNECTED => {
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
                other => return Err(harness::refusal(other)),
            }
        }
        stalled = 0;
        if i >= warmup {
            hist.saturating_record(span_ns(t0, tick()));
        }
    }
    let elapsed = start.elapsed();
    let allocations = counting.as_ref().map_or(0, Counting::close);
    let (subscription, one_way) = match subscriber.join() {
        Ok(Ok(returned)) => returned,
        failed => return Err(subscriber_failure(failed)),
    };
    // The subscription is handed back and closed after the window, as the client rows'
    // is (card 176): its close is the conductor's work, not the window's.
    publication.close()?;
    subscription.close()?;
    client.close()?;
    driver.close().map_err(|e| e.to_string())?;
    pins.check(DriverThread::Conductor)?;
    pins.check(DriverThread::Recorder)?;
    pins.check(DriverThread::Replayer)?;
    Ok(LogRun {
        claim_commit: summarize(&hist, elapsed),
        one_way: summarize(&one_way, elapsed),
        retries,
        allocations,
        producer_position_loads: None,
        purged: None,
    })
}

/// Why the client subscriber thread ended before the publisher was done: its own error,
/// or its panic.
fn subscriber_failure<T>(joined: thread::Result<Result<T, String>>) -> String {
    match joined {
        Ok(Err(message)) => format!("the client subscriber failed: {message}"),
        Ok(Ok(_)) => "the client subscriber returned before the publisher was done".to_string(),
        Err(_) => "the client subscriber thread panicked".to_string(),
    }
}

/// A loopback port no socket holds at the moment it is read, bound and released: the
/// UDP rows' channel is `cavalry:udp?endpoint=127.0.0.1:<port>`.
fn free_port() -> Result<u16, String> {
    UdpSocket::bind("127.0.0.1:0")
        .and_then(|socket| socket.local_addr())
        .map(|address| address.port())
        .map_err(|e| format!("finding a free loopback port: {e}"))
}

/// The wall clock in nanoseconds since the Unix epoch, chrony's on the bench hosts: the
/// one-way row's stamp, read on this host before the message goes and on the peer as it
/// arrives (ADR 0044). A `clock_gettime` through the vDSO, with the tail ADR 0028 names,
/// which the round trip, stamped by the harness's clock, does not carry.
fn realtime_nanos() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos() as u64)
}

/// The driver's idle strategy for the UDP rows and the link rows: busy-spin under
/// `--pin`, where the sender and the receiver each have a core and the conductor a
/// housekeeping CPU, which is what `doctor` recommends on isolated cores; backoff
/// otherwise, where five spinning driver and harness threads on a laptop's or a shared
/// runner's cores would starve each other. The client rows keep backoff with the
/// conductor pinned, as their baselines were recorded (ADR 0044).
fn driver_idle(options: &Options) -> IdleStrategy {
    if options.pin.is_some() {
        IdleStrategy::BusySpin
    } else {
        IdleStrategy::Backoff
    }
}

/// The hook that places the driver's threads for the UDP rows and the link rows: under
/// `--pin` the sender on the third CPU, the receiver on the fourth, and the conductor
/// with the recorder and the replayer on the housekeeping CPU, as the recorder runs for
/// the recorded rows (ADR 0039, ADR 0044). A pin a thread could not take is reported
/// through `pins`, as `run_client` does.
fn udp_thread_start(options: &Options, pins: &Arc<DriverPins>) -> ThreadStart {
    let housekeeping = options.housekeeping;
    let sender_cpu = options.pin.map(|pin| pin.conductor);
    let receiver_cpu = options.pin.and_then(|pin| pin.receiver);
    let pins = Arc::clone(pins);
    Arc::new(move |thread| {
        let cpu = match thread {
            DriverThread::Sender => sender_cpu,
            DriverThread::Receiver => receiver_cpu,
            DriverThread::Conductor | DriverThread::Recorder | DriverThread::Replayer => {
                housekeeping
            }
        };
        if pin_to(cpu).is_err() {
            pins.slot(thread)
                .store(cpu.map_or(0, |cpu| cpu + 1), Ordering::Release);
        }
    })
}

/// A driver, a client, a publication and a subscription for a `cavalry:udp` row, with
/// the pins its driver threads reported.
struct UdpStream {
    driver: Driver,
    client: Cavalry,
    publication: Publication,
    subscription: Subscription,
    pins: Arc<DriverPins>,
}

/// A driver and a client in `dir`, a subscription on `subscribe` and a publication on
/// `publish`, connected both ways within `timeout`: the publication once a receiver on
/// `publish`'s endpoint has answered its setup frame, the subscription once a setup frame
/// has reached `subscribe`'s endpoint and made its image. On the loopback both are this
/// driver's; over the link the other end is the peer's, and the two wait for each other.
/// This thread is on the housekeeping CPU while the driver and the client spawn, so
/// their threads inherit it, and on the publisher's CPU when this returns.
fn udp_stream(
    options: &Options,
    dir: &std::path::Path,
    subscribe: &str,
    publish: &str,
    timeout: Duration,
) -> Result<UdpStream, String> {
    let pins = Arc::new(DriverPins::new());
    let on_thread_start = udp_thread_start(options, &pins);
    pin_to(options.housekeeping)?;
    let driver =
        Driver::start_with_threads(dir, dir.join("log"), driver_idle(options), on_thread_start)
            .map_err(|e| e.to_string())?;
    let mut client = Cavalry::connect_to(dir).map_err(|e| e.to_string())?;
    let mut subscription = client
        .add_subscription(subscribe, STREAM_ID)
        .map_err(|e| e.to_string())?;
    // The driver's conductor has answered, so its hook has run.
    pins.check(DriverThread::Conductor)?;
    let publication = client
        .add_publication(publish, STREAM_ID)
        .map_err(|e| e.to_string())?;
    pin_to(options.pin.map(|pin| pin.publisher))?;
    // A poll with a fragment limit of 0 applies the subscription's events, the image
    // joining among them, and delivers nothing: the other end of the link may start
    // publishing the moment its own publication connects, which is before this end
    // sees its publication connected, and a message delivered here would be lost to
    // the echo.
    let deadline = Instant::now() + timeout;
    while !(publication.is_connected() && subscription.is_connected()) {
        subscription.poll(|_, _| {}, 0).map_err(|e| e.to_string())?;
        if Instant::now() > deadline {
            return Err(format!(
                "the stream did not connect in {} s: the publication on {publish} is {}, the \
                 subscription on {subscribe} {}; a setup frame goes every 100 ms, so a receiver \
                 on the publication's endpoint whose subscription wants the stream answers within \
                 one, and the subscription's image comes with the first setup frame on its \
                 endpoint (docs/how-to/run-the-bench.md#when-it-does-not-work)",
                timeout.as_secs(),
                if publication.is_connected() {
                    "connected"
                } else {
                    "not connected"
                },
                if subscription.is_connected() {
                    "has its image"
                } else {
                    "has no image"
                }
            ));
        }
        std::thread::sleep(Duration::from_micros(100));
    }
    // The receiver answered a setup frame the sender sent, so both hooks have run.
    pins.check(DriverThread::Sender)?;
    pins.check(DriverThread::Receiver)?;
    Ok(UdpStream {
        driver,
        client,
        publication,
        subscription,
        pins,
    })
}

/// Closes a stream's four handles in order, the publication first, so that the stream's
/// end goes out, and reads every driver thread's pin, ordered by the driver's join. With
/// `linger`, waits after the publication's close until its linger has ended, with the
/// subscription and its receive endpoint still open: a closed publication lingers with
/// heartbeats carrying end of stream until a status message with the flag answers or
/// the linger passes, and a driver closed before its first heartbeat went, within the
/// heartbeat interval of the close, would leave the peer's image to time out after 10 s
/// instead of ending (`spec/udp.md`, "The sender"); the receive endpoint stays open so
/// that the peer's own end of stream, which it sends once its image has ended, is
/// answered the same way and its linger ends early too. The loopback rows skip the
/// wait: their own subscription goes with the publication, and nothing answers.
fn close_stream(
    dir: &std::path::Path,
    publication: Publication,
    subscription: Subscription,
    client: Cavalry,
    driver: Driver,
    pins: &DriverPins,
    linger: bool,
) -> Result<(), String> {
    publication.close().map_err(|e| e.to_string())?;
    if linger {
        wait_publications_lingered(dir)?;
    }
    subscription.close().map_err(|e| e.to_string())?;
    client.close().map_err(|e| e.to_string())?;
    driver.close().map_err(|e| e.to_string())?;
    for thread in [
        DriverThread::Conductor,
        DriverThread::Recorder,
        DriverThread::Replayer,
        DriverThread::Sender,
        DriverThread::Receiver,
    ] {
        pins.check(thread)?;
    }
    Ok(())
}

/// The margin past the publication linger that [`wait_publications_lingered`] allows
/// the driver to remove the publication and release its endpoint: a few conductor duty
/// cycles, each at most a millisecond under backoff, and the counter reuse timeout.
const LINGER_MARGIN: Duration = Duration::from_secs(3);

/// Waits until no send endpoint of the driver in `dir` carries a publication, reading
/// `channel.publications` of every endpoint from the counters file as `doctor` does: the
/// count falls when a closed publication's linger has ended, which a status message
/// with the end-of-stream flag ends early and the linger timeout ends otherwise
/// (`spec/udp.md`, "The sender"). A receive endpoint's count is 0. After the linger and
/// [`LINGER_MARGIN`] a publication still carried is an error.
fn wait_publications_lingered(dir: &std::path::Path) -> Result<(), String> {
    let reader = CountersReader::open(dir).map_err(|e| e.to_string())?;
    let deadline = Instant::now() + spec::PUBLICATION_LINGER + LINGER_MARGIN;
    loop {
        let carried = (0..reader.capacity()).any(|id| {
            reader
                .record(id)
                .ok()
                .flatten()
                .is_some_and(|fields| fields.type_id() == CHANNEL_PUBLICATIONS.0)
                && reader.read_acquire(id).is_ok_and(|count| count > 0)
        });
        if !carried {
            return Ok(());
        }
        if Instant::now() > deadline {
            return Err(format!(
                "a send endpoint still carried a publication {} s past the publication linger: \
                 the driver did not remove the closed publication when its linger ended; see \
                 docs/how-to/run-the-bench.md#when-it-does-not-work",
                LINGER_MARGIN.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Runs `benchmark` with a directory of its own under the bench directory, named by the
/// process and `suffix`, made empty before and removed after, whatever the outcome.
fn with_bench_dir<T>(
    suffix: &str,
    benchmark: impl FnOnce(&std::path::Path) -> Result<T, String>,
) -> Result<T, String> {
    let dir = bench_dir().join(format!("{BENCH_DIR_PREFIX}{}-{suffix}", std::process::id()));
    // The client rows keep the driver's log directory beside the bench directory, as
    // `dir.with_extension("log")`, so that goes too.
    let log_dir = dir.with_extension("log");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&log_dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let result = benchmark(&dir);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&log_dir);
    result
}

/// What every directory the harness makes under the bench directory begins with; the
/// process's id and a hyphen follow, then the row's name, `.log` or `.probe` after it
/// for the two directories a row keeps beside its own.
const BENCH_DIR_PREFIX: &str = "cavalry-bench-";

/// What a sweep found under the bench directory.
#[derive(Debug, Default, PartialEq, Eq)]
struct Swept {
    /// This user's entries of processes that no longer run, removed.
    removed: usize,
    /// Another user's entries of such processes, left untouched.
    others: usize,
    /// This user's entries of such processes that could not be removed.
    failed: usize,
    /// The first of those, with the error.
    first_failure: Option<String>,
    /// Entries named for other processes where it cannot be told which processes
    /// have ended, or which user this is: none of them was touched.
    untold: usize,
}

/// The process id in an entry's name under the bench directory, `None` for a name
/// the harness did not give: another prefix, or anything but the id as the harness
/// writes it where the id goes, so not `+77` and not `077`.
fn bench_dir_owner(name: &str) -> Option<u32> {
    let (field, _row) = name.strip_prefix(BENCH_DIR_PREFIX)?.split_once('-')?;
    let pid: u32 = field.parse().ok()?;
    (pid > 0 && pid.to_string() == field).then_some(pid)
}

/// How this process tells a running process from one that has ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Processes {
    /// Linux: `/proc/<pid>` is a directory while a process, or a thread, has the id.
    Proc,
    /// Elsewhere: `ps -p <pid>` exits 0 while one has.
    Ps,
    /// Neither finds this process itself, a `/proc` that is not mounted and a `ps`
    /// that is missing or refused, so no process is known to have ended.
    Unknown,
}

impl Processes {
    /// The way that finds this process, which is the proof that it answers at all:
    /// a `ps` that fails for every id must not read as every process having ended.
    fn detect() -> Self {
        let me = std::process::id();
        if std::path::Path::new("/proc").join(me.to_string()).is_dir() {
            Self::Proc
        } else if ps_finds(me) {
            Self::Ps
        } else {
            Self::Unknown
        }
    }

    /// Whether a process with this id runs now, whoever's it is, as this process sees
    /// processes: one in another PID namespace is not seen. An id whose process ended
    /// may have been given to another; its entries then wait for a later sweep.
    fn runs(self, pid: u32) -> bool {
        match self {
            Self::Proc => std::path::Path::new("/proc").join(pid.to_string()).is_dir(),
            Self::Ps => ps_finds(pid),
            Self::Unknown => true,
        }
    }

    /// The user this process runs as, asked the same way and without a write, since
    /// a bench directory that is full is when the sweep matters most: the owner of
    /// this process's directory under `/proc`, or what `ps -o uid=` prints for it.
    /// The standard library has no call for it. `None` where it cannot be told.
    #[cfg(unix)]
    fn user(self) -> Option<u32> {
        let me = std::process::id().to_string();
        match self {
            Self::Proc => owning_user(&std::path::Path::new("/proc").join(me)),
            Self::Ps => std::process::Command::new("ps")
                .args(["-o", "uid=", "-p", &me])
                .stdin(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .output()
                .ok()
                .filter(|output| output.status.success())
                .and_then(|output| String::from_utf8(output.stdout).ok())
                .and_then(|uid| uid.trim().parse().ok()),
            Self::Unknown => None,
        }
    }
}

/// Whether `ps -p` finds a process with this id.
fn ps_finds(pid: u32) -> bool {
    std::process::Command::new("ps")
        .args(["-p", &pid.to_string()])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// The user that owns an entry, itself and not what a link names.
#[cfg(unix)]
fn owning_user(path: &std::path::Path) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(path).map(|meta| meta.uid()).ok()
}

/// A name under `dir` that nothing has, for an entry this process takes before it
/// removes it: this process's own id, so that no other sweep reads it as a dead
/// process's while this one runs. An earlier process with the same id may have left
/// one it could not remove, so the names in use are passed over.
fn taken_name(dir: &std::path::Path) -> Option<PathBuf> {
    (0..SWEPT_NAMES)
        .map(|n| {
            dir.join(format!(
                "{BENCH_DIR_PREFIX}{}-swept-{n}",
                std::process::id()
            ))
        })
        .find(|name| std::fs::symlink_metadata(name).is_err())
}

/// The names [`taken_name`] tries. One is in use only while its entry is being
/// removed, or for good when it would not go, so this many is that many failures.
const SWEPT_NAMES: usize = 4_096;

/// Removes what this user's bench processes that no longer run left under `dir`. A
/// row removes its directory when it ends, passed or failed ([`with_bench_dir`]); a
/// process killed before then, a job cancelled by a newer push or stopped at its
/// timeout, leaves it, and on `/dev/shm` what it leaves is memory until something
/// removes it (card 251). So every harness removes, before its first row, each entry
/// named for a process for which `runs` answers no and for which `mine` answers yes.
///
/// - An entry of a running process is never touched: the udp pair's two ends, and a
///   run by hand beside a job, each keep theirs. A process found running is asked
///   for once; one found ended is asked for again before each of its entries goes,
///   since removing what it left can take seconds and its id can be given out anew.
/// - Another user's entry is counted and not touched, so none is left half removed,
///   and a process this user cannot see, another's under `hidepid`, loses nothing.
/// - A name the harness did not give is not read.
/// - An entry is renamed under this process's own name before it is removed, in one
///   step, so that a process that starts with the dead one's id and makes the same
///   directory while the files go does not lose its own, and so that of two sweeps
///   started together one takes an entry and the other finds it gone and counts
///   nothing. What cannot be removed stays under that name and is the next run's to
///   try, once this process has ended.
///
/// The work has the directory's entries as its bound.
fn sweep_dead(
    dir: &std::path::Path,
    mine: impl Fn(&std::path::Path) -> bool,
    runs: impl Fn(u32) -> bool,
) -> Swept {
    let mut swept = Swept::default();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return swept;
    };
    let mut running: Vec<u32> = Vec::new();
    for entry in entries.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(bench_dir_owner) else {
            continue;
        };
        if running.contains(&pid) {
            continue;
        }
        if runs(pid) {
            running.push(pid);
            continue;
        }
        let path = entry.path();
        if !mine(&path) {
            swept.others += 1;
            continue;
        }
        let mut fail = |left: &std::path::Path, error: String| {
            swept.failed += 1;
            swept
                .first_failure
                .get_or_insert_with(|| format!("{}: {error}", left.display()));
        };
        let Some(taken) = taken_name(dir) else {
            fail(
                &path,
                format!("no free name of {SWEPT_NAMES} to take it under"),
            );
            continue;
        };
        match std::fs::rename(&path, &taken) {
            Ok(()) => {}
            // Another sweep that started with this one took it first: its to count.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                fail(&path, error.to_string());
                continue;
            }
        }
        let gone = if taken.is_dir() {
            std::fs::remove_dir_all(&taken)
        } else {
            std::fs::remove_file(&taken)
        };
        match gone {
            Ok(()) => swept.removed += 1,
            Err(error) => fail(&taken, error.to_string()),
        }
    }
    swept
}

/// The sweep the harness runs before its first row: this user's entries of processes
/// that have ended, under the bench directory. Where the state of a process or this
/// process's user cannot be told, nothing is touched and the entries named for other
/// processes are counted, so that the line says they are there.
fn sweep_bench_dir(dir: &std::path::Path) -> Swept {
    sweep_as(dir, Processes::detect())
}

/// [`sweep_bench_dir`] with the way to ask for a process given.
fn sweep_as(dir: &std::path::Path, processes: Processes) -> Swept {
    #[cfg(unix)]
    let user = processes.user();
    #[cfg(not(unix))]
    let user: Option<u32> = None;
    let Some(user) = user else {
        return Swept {
            untold: others_entries(dir),
            ..Swept::default()
        };
    };
    #[cfg(unix)]
    {
        sweep_dead(
            dir,
            |path| owning_user(path) == Some(user),
            |pid| processes.runs(pid),
        )
    }
    #[cfg(not(unix))]
    {
        let _ = user;
        Swept::default()
    }
}

/// How many entries under `dir` are named for a bench process other than this one.
fn others_entries(dir: &std::path::Path) -> usize {
    std::fs::read_dir(dir).map_or(0, |entries| {
        entries
            .flatten()
            .filter_map(|entry| entry.file_name().to_str().and_then(bench_dir_owner))
            .filter(|pid| *pid != std::process::id())
            .count()
    })
}

/// "1 entry" or "N entries".
fn entries(count: usize) -> String {
    if count == 1 {
        "1 entry".to_string()
    } else {
        format!("{count} entries")
    }
}

/// The sweep's line, when it found anything: what went, what is another user's, what
/// would not go, and what it could not judge.
fn swept_line(swept: &Swept, dir: &std::path::Path) -> Option<String> {
    let mut parts = Vec::new();
    if swept.removed > 0 {
        parts.push(format!(
            "removed {} under {} that bench processes no longer running left",
            entries(swept.removed),
            dir.display()
        ));
    }
    if swept.others > 0 {
        parts.push(format!(
            "{} under {} that such processes left belong to another user and were not \
             touched: a bench run as that user removes them",
            entries(swept.others),
            dir.display()
        ));
    }
    if let Some(first) = swept.first_failure.as_ref() {
        parts.push(format!(
            "{} that such processes left could not be removed, the first: {first}",
            entries(swept.failed)
        ));
    }
    if swept.untold > 0 {
        parts.push(format!(
            "{} under {} are named for other bench processes, and whether those have \
             ended cannot be told here, with no /proc and no `ps -p` that finds this \
             process: none was removed",
            entries(swept.untold),
            dir.display()
        ));
    }
    if parts.is_empty() {
        return None;
    }
    Some(format!(
        "cavalry-bench: {} (docs/how-to/run-the-bench.md#when-it-does-not-work)",
        parts.join("; ")
    ))
}

/// What a run with one message in flight produced on the publisher's side.
struct Published {
    claim_commit: Histogram<u64>,
    elapsed: Duration,
    retries: u64,
    allocations: usize,
}

/// One run of a stream with one message in flight: its warm-up and its length, the
/// body, the clock reading its stamps count from, whether it crosses the link, and what
/// a message that does not reach the subscriber means on its path, for the stall's error.
#[derive(Clone, Copy)]
struct Run {
    warmup: u64,
    total: u64,
    body: usize,
    epoch: u64,
    link: bool,
    stall: &'static str,
}

/// One run with one message in flight: this thread, the publisher, claims message i
/// once `received` says the subscriber's handler has had message i − 1, stamps it and
/// commits it, timing the claim and the commit for the claim row; the subscriber thread
/// times the rest. The first eight bytes of the body carry the harness's clock and, over
/// the link, the next eight the wall clock, for the peer to answer beside. A subscriber
/// that returned, or a message that does not come back for ten seconds, fails the run
/// with the reason instead of hanging (invariant 3).
fn publish_one_in_flight<T>(
    publication: &mut Publication,
    received: &AtomicU64,
    subscriber: &JoinHandle<T>,
    run: Run,
) -> Result<Published, String> {
    let Run {
        warmup,
        total,
        body,
        epoch,
        link,
        stall,
    } = run;
    const STALL_CHECK: u64 = 4096;
    const STALL_LIMIT: Duration = Duration::from_secs(10);
    let mut hist = histogram();
    let mut retries: u64 = 0;
    let mut counting: Option<Counting> = None;
    let mut start = Instant::now();
    for i in 0..total {
        if i == warmup {
            counting = Some(Counting::open());
            start = Instant::now();
        }
        // One in flight: the last message has reached the subscriber's handler.
        let waited = tick();
        let mut spins: u64 = 0;
        while received.load(Ordering::Acquire) < i {
            spins += 1;
            if spins.is_multiple_of(STALL_CHECK) {
                if subscriber.is_finished() {
                    return Err("the subscriber returned before the publisher was done".to_string());
                }
                if span_ns(waited, tick()) > STALL_LIMIT.as_nanos() as u64 {
                    return Err(format!(
                        "message {} of {total} did not reach the subscriber in {} s: {stall}; \
                         see docs/how-to/run-the-bench.md#when-it-does-not-work",
                        i.saturating_sub(1),
                        STALL_LIMIT.as_secs(),
                    ));
                }
            }
            std::hint::spin_loop();
        }
        let wall = if link { realtime_nanos() } else { 0 };
        let stamp = nanos(epoch);
        let t0 = tick();
        let mut stalled: u64 = 0;
        loop {
            match publication.try_claim(body) {
                Ok(mut claim) => {
                    let out = claim.body();
                    out[..8].copy_from_slice(&stamp.to_le_bytes());
                    if link {
                        out[8..16].copy_from_slice(&wall.to_le_bytes());
                    }
                    claim.commit();
                    break;
                }
                Err(Offer::BackPressured | Offer::AdminAction | Offer::NotConnected) => {
                    retries += 1;
                    stalled += 1;
                    if stalled.is_multiple_of(STALL_CHECK)
                        && span_ns(t0, tick()) > STALL_LIMIT.as_nanos() as u64
                    {
                        return Err(format!(
                            "the publisher was refused for over {} s at message {i} of {total} \
                             with one message in flight: the publication lost its connection; \
                             see docs/how-to/run-the-bench.md#when-it-does-not-work",
                            STALL_LIMIT.as_secs()
                        ));
                    }
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
    let allocations = counting.as_ref().map_or(0, Counting::close);
    Ok(Published {
        claim_commit: hist,
        elapsed,
        retries,
        allocations,
    })
}

/// What a subscriber thread hands back: its subscription, for the next run, the
/// histogram of the row it timed from the harness's stamp, the one-way row's over the
/// link, and how many one-way samples read below zero. A tuple and not a `Vec`: the
/// thread returns as soon as it has handled the last message, when the publisher's
/// counting region can still be open, so nothing on its way out may allocate.
type Received = Result<(Subscription, Histogram<u64>, Option<Histogram<u64>>, u64), String>;

/// The subscriber of a run with one message in flight, on its own thread: polls until
/// `total` messages have come, times each from the harness's stamp in its first eight
/// bytes, the one-way row on the loopback and the round trip over the link, and over the
/// link times the peer's wall-clock stamp against this host's, the one-way row; then
/// tells the publisher through `received` that the message has been handled. A one-way
/// sample below zero, the peer's clock behind this host's by more than the path, is
/// recorded as 0 and counted, so that the row is read beside the offset chrony gives.
fn spawn_subscriber(
    name: &'static str,
    cpu: Option<usize>,
    mut subscription: Subscription,
    received: Arc<AtomicU64>,
    run: Run,
) -> Result<JoinHandle<Received>, String> {
    let Run {
        warmup,
        total,
        epoch,
        link,
        ..
    } = run;
    thread::Builder::new()
        .name(name.into())
        .spawn(move || -> Received {
            pin_to(cpu)?;
            let mut first = histogram();
            let mut second = link.then(histogram);
            let mut below_zero: u64 = 0;
            let mut count: u64 = 0;
            while count < total {
                let delivered = subscription
                    .poll(
                        |body, _| {
                            let stamp = u64::from_le_bytes(body[..8].try_into().expect("8 bytes"));
                            if count >= warmup {
                                first.saturating_record(nanos(epoch).saturating_sub(stamp));
                                if let Some(second) = second.as_mut() {
                                    let sent = u64::from_le_bytes(
                                        body[8..16].try_into().expect("8 bytes"),
                                    );
                                    let peer = u64::from_le_bytes(
                                        body[16..24].try_into().expect("8 bytes"),
                                    );
                                    match peer.checked_sub(sent) {
                                        Some(one_way) => second.saturating_record(one_way),
                                        None => {
                                            second.saturating_record(0);
                                            below_zero += 1;
                                        }
                                    }
                                }
                            }
                            count += 1;
                            received.store(count, Ordering::Release);
                        },
                        1024,
                    )
                    .map_err(|e| e.to_string())?;
                if delivered == 0 {
                    std::hint::spin_loop();
                }
            }
            Ok((subscription, first, second, below_zero))
        })
        .map_err(|e| e.to_string())
}

/// The UDP rows: the client rows' path over `cavalry:udp` on the loopback, a
/// publication and a subscription on one port through a real driver in this process,
/// every message through the driver's sender and receiver, the publisher on this thread
/// and the subscriber on its own, one message in flight at a time: the publisher claims
/// the next once the subscriber's handler has had the last, so that a sample is the
/// path's latency and not the depth of the sender's window, which a publisher that runs
/// ahead fills, a publication window of 8 MiB a message then waits in for tens of
/// milliseconds (ADR 0044). Under `--pin` the sender has the third CPU and the receiver
/// the fourth, and the driver's conductor runs with the housekeeping threads, since a
/// bench host has four isolated cores; the driver's threads busy-spin when each has a
/// core and back off otherwise. The rows run a tenth of `--messages` ([`UDP_SHARE`])
/// after [`UDP_WARMUP`] messages.
fn bench_udp(options: &Options) -> Result<LogRun, String> {
    with_bench_dir("udp", |dir| run_udp(options, dir))
}

/// The multi-destination cast rows, `claim_commit_<body>_udp_mdc` and
/// `one_way_<body>_udp_mdc` (ADR 0052, feature 1; ADR 0053): the UDP rows' path over
/// a control channel, `cavalry:udp?control=<host>:<port>|control-mode=manual|fc=min`,
/// whose sender sends every datagram to two destinations, one message in flight,
/// measured at the first destination, a subscription of this driver on a port of this
/// host. In the loopback pass the second destination is a subscription of this driver
/// too, drained on a thread of its own, on the housekeeping CPU under `--pin`, so that
/// `fc=min` paces the sender by the slower of two readers that both keep up; in the link
/// pass it is the peer's endpoint across the cable, where the peer's `--echo` reads the
/// stream as its second image and echoes none of it. Reported and never gated: every
/// isolated core of both bench hosts is taken (ADR 0044), and the row joins the gate
/// with the host change that gives it cores (ADR 0052's Revisit).
fn bench_udp_mdc(options: &Options) -> Result<LogRun, String> {
    with_bench_dir("udp-mdc", |dir| {
        let pins = Arc::new(DriverPins::new());
        let on_thread_start = udp_thread_start(options, &pins);
        pin_to(options.housekeeping)?;
        let driver =
            Driver::start_with_threads(dir, dir.join("log"), driver_idle(options), on_thread_start)
                .map_err(|e| e.to_string())?;
        let mut client = Cavalry::connect_to(dir).map_err(|e| e.to_string())?;
        let second = format!("cavalry:udp?endpoint=127.0.0.1:{}", free_port()?);
        let run = mdc_rows(
            options,
            &mut client,
            &pins,
            "127.0.0.1",
            Second::Drained(&second),
            CONNECT_TIMEOUT,
        )?;
        client.close().map_err(|e| e.to_string())?;
        driver.close().map_err(|e| e.to_string())?;
        for thread in [
            DriverThread::Conductor,
            DriverThread::Recorder,
            DriverThread::Replayer,
            DriverThread::Sender,
            DriverThread::Receiver,
        ] {
            pins.check(thread)?;
        }
        Ok(run)
    })
}

/// Where the multi-destination cast rows' second destination is, by the channel of its
/// endpoint.
#[derive(Clone, Copy)]
enum Second<'a> {
    /// A subscription of this driver, drained on a thread of its own ([`spawn_drain`]):
    /// the loopback pass.
    Drained(&'a str),
    /// The peer's endpoint, whose `--echo` reads the stream and echoes none of it: the
    /// link pass.
    Peer(&'a str),
}

impl<'a> Second<'a> {
    fn channel(self) -> &'a str {
        match self {
            Second::Drained(channel) | Second::Peer(channel) => channel,
        }
    }

    /// What a message that does not reach the measured subscriber means, for the
    /// stall's error: under `fc=min` either destination's image holds the sender back.
    fn stall(self) -> &'static str {
        match self {
            Second::Drained(_) => {
                "the stream stopped on the loopback, at either destination, since under \
                 fc=min the slower image holds the sender back"
            }
            Second::Peer(_) => {
                "the peer stopped reading the second destination's image, which under fc=min \
                 holds the sender back, or the link lost a datagram the sender no longer holds"
            }
        }
    }
}

/// One run of the multi-destination cast rows on `client`, whose driver already runs:
/// a subscription on a free port of `host`, the measured destination, a second
/// subscription on the second destination when it is [`Second::Drained`], a
/// publication on a control channel of `host` with both as its destinations, connected
/// within `timeout`, the measured run, the drain stopped and joined whatever the run's
/// outcome, and the handles closed in the order `close_stream` keeps, the publication
/// first so that the stream's end goes out.
fn mdc_rows(
    options: &Options,
    client: &mut Cavalry,
    pins: &DriverPins,
    host: &str,
    second: Second<'_>,
    timeout: Duration,
) -> Result<LogRun, String> {
    let control = format!(
        "cavalry:udp?control={host}:{}|control-mode=manual|fc=min",
        free_port()?
    );
    let first = format!("cavalry:udp?endpoint={host}:{}", free_port()?);
    let mut subscription = client
        .add_subscription(&first, STREAM_ID)
        .map_err(|e| e.to_string())?;
    let mut drained = match second {
        Second::Drained(channel) => Some(
            client
                .add_subscription(channel, STREAM_ID)
                .map_err(|e| e.to_string())?,
        ),
        Second::Peer(_) => None,
    };
    let second_channel = second.channel();
    pins.check(DriverThread::Conductor)?;
    let mut publication = client
        .add_publication(&control, STREAM_ID)
        .map_err(|e| e.to_string())?;
    let id = publication.registration_id();
    client
        .add_destination(id, &first)
        .map_err(|e| e.to_string())?;
    client
        .add_destination(id, second_channel)
        .map_err(|e| e.to_string())?;
    pin_to(options.pin.map(|pin| pin.publisher))?;
    let deadline = Instant::now() + timeout;
    loop {
        subscription.poll(|_, _| {}, 0).map_err(|e| e.to_string())?;
        if let Some(drained) = drained.as_mut() {
            drained.poll(|_, _| {}, 0).map_err(|e| e.to_string())?;
        }
        if publication.is_connected()
            && subscription.is_connected()
            && drained.as_ref().is_none_or(Subscription::is_connected)
        {
            break;
        }
        if Instant::now() > deadline {
            let image = |subscription: &Subscription| {
                if subscription.is_connected() {
                    "has its image"
                } else {
                    "has no image"
                }
            };
            let second_state = match drained.as_ref() {
                Some(drained) => format!(
                    "the second destination's subscription on {second_channel} {}",
                    image(drained)
                ),
                None => format!(
                    "the second destination is the peer's endpoint {second_channel}, whose answer \
                     the wait does not need"
                ),
            };
            return Err(format!(
                "the control channel's stream did not connect in {} s: the publication on \
                 {control} is {}, and a status message from either destination connects it; \
                 the subscription on {first} {}; {second_state}; a setup frame goes to each \
                 destination every 100 ms, so a receiver on it whose subscription wants the \
                 stream answers within one (docs/how-to/run-the-bench.md#when-it-does-not-work)",
                timeout.as_secs(),
                if publication.is_connected() {
                    "connected"
                } else {
                    "not connected"
                },
                image(&subscription),
            ));
        }
        std::thread::sleep(Duration::from_micros(100));
    }
    pins.check(DriverThread::Sender)?;
    pins.check(DriverThread::Receiver)?;
    let warmup = options.udp_warmup();
    let run = Run {
        warmup,
        total: warmup + options.udp_messages(),
        body: options.body,
        epoch: tick(),
        link: false,
        stall: second.stall(),
    };
    let received = Arc::new(AtomicU64::new(0));
    let subscriber = spawn_subscriber(
        "udp-mdc-subscriber",
        options.pin.map(|pin| pin.subscriber),
        subscription,
        Arc::clone(&received),
        run,
    )?;
    let stop = Arc::new(AtomicBool::new(false));
    let drain = drained
        .map(|drained| {
            spawn_drain(
                "udp-mdc-drain",
                options.housekeeping,
                drained,
                Arc::clone(&stop),
            )
        })
        .transpose()?;
    let measured = publish_one_in_flight(&mut publication, &received, &subscriber, run).and_then(
        |published| match subscriber.join() {
            Ok(Ok((subscription, one_way, _, _))) => Ok((published, subscription, one_way)),
            failed => Err(subscriber_failure(failed)),
        },
    );
    // The drain stops whatever the run's outcome, so that its join is bounded; a drain
    // that failed is reported first, since under `fc=min` it is why the stream stalled.
    let drained = drain.map(|drain| join_drain(drain, &stop)).transpose()?;
    let (published, subscription, one_way) = measured?;
    publication.close().map_err(|e| e.to_string())?;
    subscription.close().map_err(|e| e.to_string())?;
    if let Some(drained) = drained {
        drained.close().map_err(|e| e.to_string())?;
    }
    Ok(LogRun {
        claim_commit: summarize(&published.claim_commit, published.elapsed),
        one_way: summarize(&one_way, published.elapsed),
        retries: published.retries,
        allocations: published.allocations,
        producer_position_loads: None,
        purged: None,
    })
}

/// How long the second destination's drain sleeps after a poll that delivered nothing:
/// short beside the time its image's receiver window of 128 KiB (`spec/udp.md`,
/// "Values") takes to fill with one message in flight, about 20 ms of 64-byte frames at
/// the loopback's 10 µs a message and about 1 ms of the MTU's frames, so that under
/// `fc=min` the drain does not pace the sender; long enough that the drain does not load
/// the housekeeping CPU it shares with the driver's conductor under `--pin`.
const DRAIN_IDLE: Duration = Duration::from_micros(100);

/// A thread that polls `subscription`, the second destination's reader in the loopback
/// pass, keeping nothing, until `stop` is set, and returns the subscription. It pins
/// itself to `cpu` first, the housekeeping CPU under `--pin`, as the subscriber pins
/// itself, since a thread spawned from the publisher's thread inherits the publisher's
/// CPU and a pinned CPU holds one measured thread alone (card 136); after a poll that
/// delivered nothing it sleeps [`DRAIN_IDLE`]. [`join_drain`] waits one poll and one
/// sleep past `stop` at most.
fn spawn_drain(
    name: &'static str,
    cpu: Option<usize>,
    mut subscription: Subscription,
    stop: Arc<AtomicBool>,
) -> Result<JoinHandle<Result<Subscription, String>>, String> {
    thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            pin_to(cpu)?;
            while !stop.load(Ordering::Acquire) {
                let delivered = subscription
                    .poll(|_, _| {}, 1024)
                    .map_err(|e| e.to_string())?;
                if delivered == 0 {
                    thread::sleep(DRAIN_IDLE);
                }
            }
            Ok(subscription)
        })
        .map_err(|e| format!("the second destination's drain thread did not start: {e}"))
}

/// Sets `stop` and joins the drain, with its subscription, or what failed in it.
fn join_drain(
    drain: JoinHandle<Result<Subscription, String>>,
    stop: &AtomicBool,
) -> Result<Subscription, String> {
    stop.store(true, Ordering::Release);
    match drain.join() {
        Ok(Ok(subscription)) => Ok(subscription),
        Ok(Err(message)) => Err(format!("the second destination's drain failed: {message}")),
        Err(_) => Err(
            "the second destination's drain thread panicked: the panic's message, printed \
             above, names the line to look at"
                .to_string(),
        ),
    }
}

fn run_udp(options: &Options, dir: &std::path::Path) -> Result<LogRun, String> {
    let channel = format!("cavalry:udp?endpoint=127.0.0.1:{}", free_port()?);
    let UdpStream {
        driver,
        client,
        mut publication,
        subscription,
        pins,
    } = udp_stream(options, dir, &channel, &channel, CONNECT_TIMEOUT)?;
    let warmup = options.udp_warmup();
    let run = Run {
        warmup,
        total: warmup + options.udp_messages(),
        body: options.body,
        epoch: tick(),
        link: false,
        stall: "the stream stopped on the loopback",
    };
    let received = Arc::new(AtomicU64::new(0));
    let subscriber = spawn_subscriber(
        "udp-subscriber",
        options.pin.map(|pin| pin.subscriber),
        subscription,
        Arc::clone(&received),
        run,
    )?;
    let published = publish_one_in_flight(&mut publication, &received, &subscriber, run)?;
    let (subscription, one_way, _, _) = match subscriber.join() {
        Ok(Ok(received)) => received,
        failed => return Err(subscriber_failure(failed)),
    };
    close_stream(dir, publication, subscription, client, driver, &pins, false)?;
    Ok(LogRun {
        claim_commit: summarize(&published.claim_commit, published.elapsed),
        one_way: summarize(&one_way, published.elapsed),
        retries: published.retries,
        allocations: published.allocations,
        producer_position_loads: None,
        purged: None,
    })
}

/// What one run of the link rows produced.
struct LinkRun {
    round_trip: Summary,
    one_way: Summary,
    retries: u64,
    allocations: usize,
    below_zero: u64,
}

/// The link rows, `udp_round_trip_<body>` and `udp_one_way_<body>`: the UDP rows' path
/// across the direct link to the peer running `--echo` on the other bench host, which
/// claims every message it receives on a publication back to this host, so that the
/// round trip is measured on this host's clock alone, publisher's stamp to subscriber's
/// handler, and gated; and the one-way is this host's wall-clock stamp against the
/// peer's at its handler, which carries the offset between the two hosts' clocks, so
/// it is reported beside the offset chrony gives on the peer and never gated (ADR 0039,
/// ADR 0044). The stream, the driver and the peer's image live across every run, so a
/// run after the first starts connected; `--messages` is the count per run, as given.
fn bench_link(options: &Options, link: &Link) -> Result<(Vec<LinkRun>, LogRun), String> {
    with_bench_dir("link", |dir| run_link(options, dir, link))
}

fn run_link(
    options: &Options,
    dir: &std::path::Path,
    link: &Link,
) -> Result<(Vec<LinkRun>, LogRun), String> {
    let UdpStream {
        driver,
        mut client,
        mut publication,
        subscription,
        pins,
    } = udp_stream(
        options,
        dir,
        &link.here_channel(),
        &link.peer_channel(),
        LINK_TIMEOUT,
    )?;
    let warmup = options.udp_warmup();
    let mut subscription = Some(subscription);
    let mut runs = Vec::with_capacity(options.repeat as usize);
    for _ in 0..options.repeat {
        let run = Run {
            warmup,
            total: warmup + options.messages,
            body: options.body,
            epoch: tick(),
            link: true,
            stall: "the peer stopped echoing, or the link lost a datagram the sender no longer \
                    holds",
        };
        let received = Arc::new(AtomicU64::new(0));
        let held = subscription
            .take()
            .ok_or_else(|| "the subscription is held between runs".to_string())?;
        let subscriber = spawn_subscriber(
            "link-subscriber",
            options.pin.map(|pin| pin.subscriber),
            held,
            Arc::clone(&received),
            run,
        )?;
        let published = publish_one_in_flight(&mut publication, &received, &subscriber, run)?;
        let (returned, round_trip, one_way, below_zero) = match subscriber.join() {
            Ok(Ok(received)) => received,
            failed => return Err(subscriber_failure(failed)),
        };
        subscription = Some(returned);
        let Some(one_way) = one_way else {
            return Err("the link subscriber measured no one-way row".to_string());
        };
        runs.push(LinkRun {
            round_trip: summarize(&round_trip, published.elapsed),
            one_way: summarize(&one_way, published.elapsed),
            retries: published.retries,
            allocations: published.allocations,
            below_zero,
        });
    }
    // The multi-destination cast rows over the cable and this host at once (ADR 0052,
    // feature 1): a control channel on this host's link address with the peer's
    // endpoint and a subscription of this driver as its destinations, measured here;
    // the peer reads the stream as a second image and echoes none of it, so the link
    // subscription, held unpolled until the close, gets nothing.
    let peer = link.peer_channel();
    let mdc = mdc_rows(
        options,
        &mut client,
        &pins,
        &link.here_host(),
        Second::Peer(&peer),
        LINK_TIMEOUT,
    )?;
    let subscription = subscription
        .take()
        .ok_or_else(|| "the subscription is held after the runs".to_string())?;
    close_stream(dir, publication, subscription, client, driver, &pins, true)?;
    Ok((runs, mdc))
}

/// The echo peer, `--echo`: the other end of the link rows, on the other bench host. A
/// driver and a client, a subscription on this host's link endpoint and a publication to
/// the peer's; every message the subscription delivers is claimed on the publication
/// with the same body, this host's wall clock written at its third eight bytes, and
/// committed, so that the other host times the round trip on its own clock and the
/// one-way against this host's. Only the link stream's image is echoed, the first image,
/// by its session id: the multi-destination cast rows' publication, which the other
/// host opens after the link rows with this endpoint as its second destination, makes a
/// second image of the same stream id here, which is read to keep up under its `fc=min`
/// and echoed nowhere, so that nothing of it fills the other host's link subscription,
/// which nothing polls then, or lands on its receiver beside the measured datagrams. The
/// loop ends when the link stream's image ends with the stream's end, and fails when
/// that image goes without one or no message comes for the link timeout (ADR 0044).
/// Under `--pin` the echo runs on the publisher's CPU, with the sender and the receiver
/// on theirs.
fn run_echo(options: &Options, link: &Link) -> Result<(), String> {
    with_bench_dir("echo", |dir| echo(options, dir, link))
}

fn echo(options: &Options, dir: &std::path::Path, link: &Link) -> Result<(), String> {
    /// How long a claim is retried before the message is dropped from the echo: with
    /// one message in flight the publication is never back-pressured, so a refusal that
    /// lasts is a lost connection.
    const CLAIM_LIMIT_NS: u64 = 1_000_000_000;
    println!(
        "cavalry-bench: echo peer on {}, answering to {}, waiting up to {} s for the stream",
        link.here,
        link.peer,
        LINK_TIMEOUT.as_secs()
    );
    let UdpStream {
        driver,
        client,
        mut publication,
        mut subscription,
        pins,
    } = udp_stream(
        options,
        dir,
        &link.here_channel(),
        &link.peer_channel(),
        LINK_TIMEOUT,
    )?;
    let Some((session, source)) = subscription
        .images()
        .first()
        .map(|image| (image.session_id(), image.source_identity().to_string()))
    else {
        return Err(format!(
            "the stream from {} lost its image as it connected: run the harness on both hosts \
             again",
            link.peer
        ));
    };
    println!("cavalry-bench: the stream from {source} is connected, echoing");
    let started = Instant::now();
    let mut echoed: u64 = 0;
    let mut refused: u64 = 0;
    // Messages of another image, the multi-destination cast rows', read and not echoed.
    let mut unechoed: u64 = 0;
    let mut last = tick();
    let mut ended = false;
    loop {
        let delivered = subscription
            .poll(
                |body, header| {
                    if header.session_id() != session {
                        unechoed += 1;
                        return;
                    }
                    let now = realtime_nanos();
                    let t0 = tick();
                    loop {
                        match publication.try_claim(body.len()) {
                            Ok(mut claim) => {
                                let out = claim.body();
                                out.copy_from_slice(body);
                                if out.len() >= LINK_BODY_LEAST {
                                    out[16..24].copy_from_slice(&now.to_le_bytes());
                                }
                                claim.commit();
                                echoed += 1;
                                break;
                            }
                            Err(
                                Offer::BackPressured | Offer::AdminAction | Offer::NotConnected,
                            ) => {
                                if span_ns(t0, tick()) > CLAIM_LIMIT_NS {
                                    refused += 1;
                                    break;
                                }
                                std::hint::spin_loop();
                            }
                            Err(_) => {
                                refused += 1;
                                break;
                            }
                        }
                    }
                },
                1,
            )
            .map_err(|e| e.to_string())?;
        if delivered > 0 {
            last = tick();
            continue;
        }
        match subscription
            .images()
            .iter()
            .find(|image| image.session_id() == session)
        {
            // The image has read everything the stream wrote, so every echo is claimed:
            // leave now, and not when the receiver retires the image, which it does in
            // the same cycle, so that the exit does not wait on the retire. The other
            // image's end, which comes first, is not the link stream's.
            Some(image) if image.is_end_of_stream() => {
                ended = true;
                break;
            }
            Some(_) => {}
            // The link stream's image went, with its end or without it: the counters
            // below say which.
            None => break,
        }
        if span_ns(last, tick()) > LINK_TIMEOUT.as_nanos() as u64 {
            return Err(format!(
                "no message for {} s after {echoed} echoed from {source}: the other host's run \
                 stopped without ending the stream; see \
                 docs/how-to/run-the-bench.md#when-it-does-not-work",
                LINK_TIMEOUT.as_secs()
            ));
        }
        std::hint::spin_loop();
    }
    // The image's end from the counters, as `doctor` would read it: the receiver
    // retires an image that reached its end of stream in the same cycle it stores the
    // end, when every reader is already there, and the client may apply the image's
    // going before the loop above has looked at the flag; `image.ended` and
    // `image.timed_out` say which it was (`spec/udp.md`, "The receiver", step 5).
    let (image_ended, image_timed_out) = {
        let reader = CountersReader::open(dir).map_err(|e| e.to_string())?;
        (
            reader
                .read_acquire(IMAGE_ENDED.record())
                .map_err(|e| e.to_string())?,
            reader
                .read_acquire(IMAGE_TIMED_OUT.record())
                .map_err(|e| e.to_string())?,
        )
    };
    let ended = ended || (image_ended > 0 && image_timed_out == 0);
    close_stream(dir, publication, subscription, client, driver, &pins, true)?;
    if !ended {
        return Err(format!(
            "the stream from {source} went after {echoed} messages without its end: the other \
             host's harness stopped, or its image timed out here after 10 s without a datagram \
             (image.ended {image_ended}, image.timed_out {image_timed_out}); see \
             docs/how-to/run-the-bench.md#when-it-does-not-work"
        ));
    }
    println!(
        "cavalry-bench: echoed {echoed} messages from {source} back to {} in {:.1} s, {refused} \
         claim(s) refused, {unechoed} of the multi-destination cast rows' image read and not \
         echoed",
        link.peer,
        started.elapsed().as_secs_f64()
    );
    Ok(())
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
        let cpu = options.pin.map(|pin| pin.subscriber);
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

/// `name count, name count` over a map by name, for the lines under the table, or
/// `empty` when nothing was kept.
fn listed<T: std::fmt::Display>(by_name: &BTreeMap<String, T>, empty: &str) -> String {
    if by_name.is_empty() {
        return empty.to_string();
    }
    by_name
        .iter()
        .map(|(name, count)| format!("{name} {count}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The members of a JSON object over a map by name, `"name": count, "name": count`.
fn json_object<T: std::fmt::Display>(by_name: &BTreeMap<String, T>) -> String {
    by_name
        .iter()
        .map(|(name, count)| format!("\"{name}\": {count}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The failure a run exits 1 with when a measured loop allocated: the count, the
/// benchmarks whose loops made it, each under the name the lines under the table list
/// it by, and the anchor that explains it; `None` when every benchmark's count is 0.
/// A count of 1 in 20,500,000 messages, which `gates (macos)` read at cb2f538, could
/// not be placed from the sum alone, so the message carries `first`, the backtrace of
/// the first allocation a window saw ([`FIRST_ALLOCATION`]), frame by frame.
fn allocation_failure(
    by_benchmark: &BTreeMap<String, usize>,
    first: Option<&str>,
) -> Option<String> {
    let allocations: usize = by_benchmark.values().sum();
    if allocations == 0 {
        return None;
    }
    let allocated: BTreeMap<String, usize> = by_benchmark
        .iter()
        .filter(|(_, count)| **count > 0)
        .map(|(name, &count)| (name.clone(), count))
        .collect();
    let mut message = format!(
        "the measured loops allocated {allocations} times, by benchmark {}; the gate requires \
         zero allocations per message (CLAUDE.md, gate 2; .claude/rules/data-plane.md); see \
         docs/how-to/run-the-bench.md#when-it-does-not-work",
        listed(&allocated, "")
    );
    match first {
        Some(backtrace) => {
            let _ = write!(
                message,
                "\n  the first allocation a measured loop saw, frame by frame from the \
                 allocator's hook to the thread's start:\n{backtrace}"
            );
        }
        None => message.push_str(
            "\n  no backtrace was captured for the first allocation within five seconds of the \
             last row; run the same command again, since each run captures afresh",
        ),
    }
    Some(message)
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
    with_bench_dir("record", |dir| run_record(options, dir))
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
    let mut counting: Option<Counting> = None;
    let mut start = Instant::now();
    for block in 0..warmup + blocks {
        if block == warmup {
            counting = Some(Counting::open());
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
    let allocations = counting.as_ref().map_or(0, Counting::close);
    log_dir.stop(writer, now).map_err(|e| e.to_string())?;
    Ok(RecordRun {
        record_block: summarize(&hist, elapsed),
        allocations,
    })
}

/// What one pass over every benchmark produced.
#[derive(Default)]
struct Pass {
    benchmarks: BTreeMap<String, Summary>,
    retries: BTreeMap<String, u64>,
    producer_position_loads: BTreeMap<String, u64>,
    /// The allocations each benchmark's measured loops made, under the name its retries
    /// are kept under and `record` for the recorder's block row, so that a run which
    /// fails the allocation gate names the loop to look at; the sum over the names is
    /// the count the run is judged on.
    allocations: BTreeMap<String, usize>,
    /// The messages the message rows measured, for the allocations per message:
    /// `--messages` for each of the two log rows and the two client rows in a full
    /// pass, a tenth of that for the UDP row ([`UDP_SHARE`]), fewer under `--only`.
    messages_measured: u64,
    /// What the purge running through a benchmark's loop did, under its name (card 177).
    purged: BTreeMap<String, Purged>,
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

    /// Records a message benchmark's run of `messages` messages, a log, a client or a
    /// UDP one: its two rows, and its retries, producer position loads and allocations
    /// under `name`.
    fn insert_message_run(
        &mut self,
        benchmark: Benchmark,
        name: &str,
        body: usize,
        run: LogRun,
        messages: u64,
    ) {
        self.insert_rows(benchmark, body, [run.claim_commit, run.one_way]);
        self.retries.insert(name.into(), run.retries);
        if let Some(loads) = run.producer_position_loads {
            self.producer_position_loads.insert(name.into(), loads);
        }
        self.allocations.insert(name.into(), run.allocations);
        if let Some(purged) = run.purged {
            self.purged.insert(name.into(), purged);
        }
        self.messages_measured += messages;
    }
}

/// Every benchmark `--only` admits once, in [`Benchmark::ALL`]'s order: the timer, both
/// log types, the client path with and without a recording on, the client path over
/// UDP on the loopback, the client path through the C ABI, the three slot spacings,
/// and last the recorder's block path.
fn run_pass(options: &Options) -> Result<Pass, String> {
    let mut pass = Pass::default();
    for benchmark in options.benchmarks() {
        match benchmark {
            Benchmark::Timer => {
                pass.insert_rows(benchmark, options.body, [bench_timer(options)]);
            }
            Benchmark::Log { log_type, name } => {
                let run = bench_log(options, log_type)
                    .map_err(|message| format!("the {name} log benchmark failed: {message}"))?;
                pass.insert_message_run(benchmark, name, options.body, run, options.messages);
            }
            Benchmark::Client {
                recorded,
                purging,
                name,
            } => {
                let run = bench_client(options, recorded, purging, name)
                    .map_err(|message| format!("the {name} benchmark failed: {message}"))?;
                pass.insert_message_run(benchmark, name, options.body, run, options.messages);
            }
            Benchmark::Udp => {
                let run = bench_udp(options)
                    .map_err(|message| format!("the udp benchmark failed: {message}"))?;
                pass.insert_message_run(
                    benchmark,
                    "udp",
                    options.body,
                    run,
                    options.udp_messages(),
                );
            }
            Benchmark::UdpMdc => {
                let run = bench_udp_mdc(options)
                    .map_err(|message| format!("the udp_mdc benchmark failed: {message}"))?;
                pass.insert_message_run(
                    benchmark,
                    "udp_mdc",
                    options.body,
                    run,
                    options.udp_messages(),
                );
            }
            Benchmark::Ffi => {
                let run = bench_ffi(options)
                    .map_err(|message| format!("the ffi benchmark failed: {message}"))?;
                pass.insert_message_run(benchmark, "ffi", options.body, run, options.messages);
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
                pass.allocations.insert("record".into(), record.allocations);
            }
        }
    }
    Ok(pass)
}

fn main() -> ExitCode {
    // The hook behind the measured loops' allocation count, set once per process and
    // enabled only inside a counting window.
    if AllocationRegistry::set_global_tracker(CountingTracker).is_err() {
        eprintln!(
            "cavalry-bench: the allocation tracker was already set, which nothing in this binary \
             does before main; the allocation gate cannot count without its own, so report this \
             line as a bug in the harness (bench/src/main.rs, main; ADR 0047)"
        );
        return ExitCode::FAILURE;
    }
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
    // What killed bench processes left under the bench directory goes before the first
    // row, and before this thread is pinned, so that the file work is on no measured
    // CPU (card 251).
    let scratch = bench_dir();
    if let Some(line) = swept_line(&sweep_bench_dir(&scratch), &scratch) {
        println!("{line}");
    }
    // The publisher is this thread, and so are the timer and the slot-padding loads,
    // and the echo peer's loop.
    if let Err(message) = pin_to(options.pin.map(|pin| pin.publisher)) {
        eprintln!("cavalry-bench: {message}");
        return ExitCode::FAILURE;
    }
    if let Some(link) = options.link.as_ref().filter(|link| link.echo) {
        return match run_echo(&options, link) {
            Ok(()) => ExitCode::SUCCESS,
            Err(message) => {
                eprintln!("cavalry-bench: {message}");
                ExitCode::FAILURE
            }
        };
    }
    // The clock's step, on the publisher's CPU now that it is pinned (ADR 0029).
    let (clock_step, clock_step_ticks) = clock_step();
    let pinned = options.pin.map_or_else(
        || "unpinned".to_string(),
        |pin| format!("pinned to CPUs {}", pin.list()),
    );
    let rows = options.link.as_ref().map_or_else(
        || {
            format!(
                "{} messages after {} warm-up",
                options.messages,
                options.warmup()
            )
        },
        |link| {
            format!(
                "the link rows alone from {} to the peer at {}, {} messages after {} warm-up",
                link.here,
                link.peer,
                options.messages,
                options.udp_warmup()
            )
        },
    );
    let sha = git_sha();
    let date_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    println!(
        "cavalry-bench: profile {}, git {sha}, {} {}, {cpus} cores, {pinned}, clock step {clock_step} ns ({clock_step_ticks} ticks), {rows}, body {} bytes, term {} bytes, files under {}, {} run(s) per benchmark",
        options.profile,
        std::env::consts::OS,
        std::env::consts::ARCH,
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
    // Each benchmark's allocations under its name, summed over the runs, `link` for the
    // link rows; the sum over the names is the count the exit code is decided on.
    let mut allocations_by_benchmark: BTreeMap<String, usize> = BTreeMap::new();
    // The purge's figures over the runs: the fewest files removed during a loop, and
    // whether it outlasted every loop (card 177).
    let mut purged_by_benchmark: BTreeMap<String, Purged> = BTreeMap::new();
    let mut messages_measured: u64 = 0;
    let mut below_zero: u64 = 0;
    if let Some(link) = &options.link {
        // The link rows alone, every run over one stream, so the runs are the pass.
        let (runs, mdc) = match bench_link(&options, link) {
            Ok(runs) => runs,
            Err(message) => {
                eprintln!("cavalry-bench: the link rows failed: {message}");
                return ExitCode::FAILURE;
            }
        };
        for (row, summary) in Benchmark::UdpMdc
            .rows(options.body)
            .into_iter()
            .zip([mdc.claim_commit, mdc.one_way])
        {
            passes.entry(row).or_default().push(summary);
        }
        retries.insert("udp_mdc".to_string(), mdc.retries);
        *allocations_by_benchmark
            .entry("udp_mdc".to_string())
            .or_insert(0) += mdc.allocations;
        messages_measured += options.udp_messages();
        let round_trip = format!("udp_round_trip_{}", options.body);
        let one_way = format!("udp_one_way_{}", options.body);
        for run in runs {
            passes
                .entry(round_trip.clone())
                .or_default()
                .push(run.round_trip);
            passes.entry(one_way.clone()).or_default().push(run.one_way);
            let worst = retries.entry("link".to_string()).or_insert(0);
            *worst = (*worst).max(run.retries);
            *allocations_by_benchmark
                .entry("link".to_string())
                .or_insert(0) += run.allocations;
            messages_measured += options.messages;
            below_zero = below_zero.max(run.below_zero);
        }
    }
    let passes_to_run = if options.link.is_some() {
        0
    } else {
        options.repeat
    };
    for _ in 0..passes_to_run {
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
        for (name, count) in pass.allocations {
            *allocations_by_benchmark.entry(name).or_insert(0) += count;
        }
        for (name, purged) in pass.purged {
            purged_by_benchmark
                .entry(name)
                .and_modify(|worst| {
                    worst.during = worst.during.min(purged.during);
                    worst.outlasted &= purged.outlasted;
                })
                .or_insert(purged);
        }
        messages_measured += pass.messages_measured;
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

    // The messages the message rows measured: five rows in a full pass, the two log
    // rows and the two client rows of `--messages` each and the UDP row of a tenth, or
    // the link rows alone; fewer under `--only`, and none when it admitted only the
    // timer, slot or record rows, where the ratio has no denominator and is left out of
    // the line and the result, while the count still decides the exit code. The
    // recorder's block row adds its allocations to the count and no messages.
    let allocations: usize = allocations_by_benchmark.values().sum();
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
        "  publisher retries (back pressure, rotation or not connected), the most of any run: {}",
        listed(&retries, "no message row ran")
    );
    println!(
        "  producer position loads by the harness's conductor, at the driver's cadence by the clock, the most of any run: {}",
        listed(&producer_position_loads, "no log row ran")
    );
    println!(
        "  allocations by benchmark, summed over the runs: {}",
        listed(
            &allocations_by_benchmark,
            "no row ran under the counting allocator"
        )
    );
    match allocations_per_message {
        Some(ratio) => println!(
            "  allocations per message: {ratio} ({allocations} in {messages_measured} messages)"
        ),
        None => println!(
            "  allocations per message: no message row ran ({allocations} allocations in the measured loops)"
        ),
    }
    if options.link.is_some() {
        println!(
            "  one-way samples below zero, the peer's clock behind this host's by more than the path, recorded as 0, the most of any run: {below_zero}"
        );
    }
    for (name, purged) in &purged_by_benchmark {
        println!(
            "  segment files the replayer removed during {name}'s measured loop, the fewest of any run: {} of a pile of {}; the purge {} the loop in every run",
            purged.during,
            purged.pile,
            if purged.outlasted {
                "outlasted"
            } else {
                "did not outlast"
            }
        );
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
        options.pin.map_or_else(|| "null".to_string(), Pins::json)
    );
    let _ = writeln!(out, "  \"messages\": {},", options.messages);
    let _ = writeln!(out, "  \"repeat\": {},", options.repeat);
    // The warm-up the run used: the UDP rows' alone in link mode, where no other row ran.
    let _ = writeln!(
        out,
        "  \"warmup\": {},",
        if options.link.is_some() {
            options.udp_warmup()
        } else {
            options.warmup()
        }
    );
    let _ = writeln!(out, "  \"body\": {},", options.body);
    let _ = writeln!(out, "  \"term_length\": {},", options.term_length);
    let _ = writeln!(
        out,
        "  \"publisher_retries\": {{{}}},",
        json_object(&retries)
    );
    if options.link.is_some() {
        let _ = writeln!(out, "  \"link_one_way_below_zero\": {below_zero},");
    }
    let _ = writeln!(
        out,
        "  \"producer_position_loads\": {{{}}},",
        json_object(&producer_position_loads)
    );
    // Each benchmark's share of the count under its name, then the count the exit code
    // is decided on, beside the ratio: `bench-compare` judges the count when no message
    // row ran and the ratio is `null` (card 161), and reads neither the shares nor the
    // names.
    let _ = writeln!(
        out,
        "  \"allocations_by_benchmark\": {{{}}},",
        json_object(&allocations_by_benchmark)
    );
    let _ = writeln!(out, "  \"allocations\": {allocations},");
    let _ = writeln!(
        out,
        "  \"allocations_per_message\": {},",
        allocations_per_message.map_or_else(|| "null".to_string(), |ratio| ratio.to_string())
    );
    let _ = writeln!(out, "  \"clock_step_ns\": {clock_step},");
    let purged_json: BTreeMap<String, String> = purged_by_benchmark
        .iter()
        .map(|(name, purged)| {
            (
                name.clone(),
                format!(
                    "{{\"during_least\": {}, \"pile\": {}, \"outlasted_every_run\": {}}}",
                    purged.during, purged.pile, purged.outlasted
                ),
            )
        })
        .collect();
    let _ = writeln!(
        out,
        "  \"segments_purged\": {{{}}},",
        json_object(&purged_json)
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

    if let Some(message) = allocation_failure(&allocations_by_benchmark, first_allocation()) {
        eprintln!("cavalry-bench: {message}");
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
            options(&["--only", "one_way_32_udp"]).benchmarks(),
            vec![Benchmark::Udp, Benchmark::UdpMdc],
            "a name one row contains is in the mdc row too"
        );
        assert_eq!(
            options(&["--only", "one_way_32_udp_mdc"]).benchmarks(),
            vec![Benchmark::UdpMdc]
        );
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
        // The timer, two rows each of the seven message benchmarks and the three slot
        // pairs, and the record row.
        let rows = options(&[]).rows();
        assert_eq!(rows.len(), 22);
        for row in rows {
            assert!(error.contains(&row), "{error} does not list {row}");
        }
    }

    /// `--pin` takes three CPUs, the receiver left out, or four, all distinct.
    #[test]
    fn pin_takes_three_or_four_distinct_cpus() {
        let three = parse_pin("8,10,12").expect("three parse");
        assert_eq!(
            three,
            Pins {
                publisher: 8,
                subscriber: 10,
                conductor: 12,
                receiver: None
            }
        );
        assert_eq!(three.list(), "8,10,12");
        assert_eq!(three.json(), "[8, 10, 12]");
        let four = parse_pin("4,5,6,7").expect("four parse");
        assert_eq!(four.receiver, Some(7));
        assert_eq!(four.list(), "4,5,6,7");
        assert_eq!(four.json(), "[4, 5, 6, 7]");
        assert!(four.contains(7) && !four.contains(8));
        for text in ["8,10", "8,10,12,14,16", "8,10,12,12", "8,10,8", "8,10,x"] {
            assert!(parse_pin(text).is_err(), "{text} parsed");
        }
    }

    /// Three pins refuse a UDP row, which needs the fourth for the receiver, and admit
    /// a run without one; four admit both.
    #[test]
    fn three_pins_refuse_a_udp_row() {
        let three = parse_pin("8,10,12").expect("three parse");
        let four = parse_pin("8,10,12,14").expect("four parse");
        assert!(check_pins(three, false).is_ok());
        let error = check_pins(three, true).expect_err("a UDP row needs four");
        assert!(
            error.contains("--pin 8,10,12")
                && error.contains("fourth")
                && error.contains("docs/how-to/run-the-bench.md#when-it-does-not-work"),
            "{error}"
        );
        assert!(check_pins(four, true).is_ok());
    }

    /// The link rows need both ends, the profile gets `-link`, `--only` and a short body
    /// are refused, and `--echo` needs the ends too and leaves the profile alone.
    #[test]
    fn the_link_rows_need_both_ends() {
        for args in [
            ["--link-here", "bench-link:40182"].as_slice(),
            ["--link-peer", "grip-link:40183"].as_slice(),
            ["--echo"].as_slice(),
        ] {
            let Err(error) = parsed(args) else {
                panic!("{args:?} parsed, and one end alone is refused");
            };
            assert!(
                error.contains("--link-here and --link-peer go together"),
                "{error}"
            );
        }
        let both = [
            "--link-here",
            "bench-link:40182",
            "--link-peer",
            "grip-link:40183",
        ];
        let link = options(&both);
        assert_eq!(
            link.link,
            Some(Link {
                here: "bench-link:40182".into(),
                peer: "grip-link:40183".into(),
                echo: false
            })
        );
        assert_eq!(link.profile, "p-link");
        assert_eq!(
            link.link.as_ref().map(Link::here_channel).as_deref(),
            Some("cavalry:udp?endpoint=bench-link:40182")
        );
        let mut echo = both.to_vec();
        echo.push("--echo");
        let echo = options(&echo);
        assert!(echo.link.as_ref().is_some_and(|link| link.echo));
        assert_eq!(echo.profile, "p");
        let mut only = both.to_vec();
        only.extend(["--only", "udp"]);
        assert!(matches!(parsed(&only), Err(error) if error.contains("--only chooses")));
        let mut short = both.to_vec();
        short.extend(["--body", "16"]);
        assert!(matches!(parsed(&short), Err(error) if error.contains("at least 24")));
    }

    /// The UDP rows run a tenth of the messages, never none, after their own warm-up;
    /// `--warmup` sets theirs as it sets the others'.
    #[test]
    fn the_udp_rows_run_a_tenth() {
        let defaults = options(&[]);
        assert_eq!(defaults.udp_messages(), 100_000);
        assert_eq!(defaults.udp_warmup(), 10_000);
        assert_eq!(options(&["--messages", "5"]).udp_messages(), 1);
        assert_eq!(options(&["--warmup", "7"]).udp_warmup(), 7);
    }

    /// A pass keeps each message benchmark's allocations under its name, so that the
    /// failure can name the loop that allocated, and its messages in the sum.
    #[test]
    fn a_pass_keeps_allocations_by_benchmark() {
        let run = |allocations| {
            let empty = Summary {
                count: 0,
                p50_ns: 0,
                p99_ns: 0,
                p99_99_ns: 0,
                max_ns: 0,
                rate_per_s: 0.0,
            };
            LogRun {
                claim_commit: empty.clone(),
                one_way: empty,
                retries: 0,
                allocations,
                producer_position_loads: None,
                purged: None,
            }
        };
        let mut pass = Pass::default();
        pass.insert_message_run(Benchmark::Udp, "udp", 32, run(1), 10);
        pass.insert_message_run(
            Benchmark::Client {
                recorded: false,
                purging: false,
                name: "client",
            },
            "client",
            32,
            run(0),
            100,
        );
        assert_eq!(
            pass.allocations,
            BTreeMap::from([("client".to_string(), 0), ("udp".to_string(), 1)])
        );
        assert_eq!(pass.messages_measured, 110);
        assert_eq!(pass.benchmarks.len(), 4);
    }

    /// A run whose loops allocated fails naming the count, the benchmarks that made it
    /// and the anchor, and one whose every benchmark counted 0 does not fail.
    #[test]
    fn the_allocation_failure_names_the_benchmarks_that_allocated() {
        let mut by_benchmark = BTreeMap::from([("client".to_string(), 0), ("udp".to_string(), 0)]);
        assert_eq!(allocation_failure(&by_benchmark, None), None);
        by_benchmark.insert("udp".to_string(), 1);
        by_benchmark.insert("record".to_string(), 2);
        let message = allocation_failure(&by_benchmark, None).expect("three allocations fail");
        assert!(
            message.contains("allocated 3 times")
                && message.contains("by benchmark record 2, udp 1;")
                && !message.contains("client")
                && message.contains("docs/how-to/run-the-bench.md#when-it-does-not-work"),
            "{message}"
        );
    }

    /// The failure's message carries the first allocation's backtrace when one was
    /// captured, and says so when none was.
    #[test]
    fn the_failure_carries_the_first_allocations_backtrace() {
        let by_benchmark = BTreeMap::from([("udp".to_string(), 1)]);
        let frames = "   0: alloc::raw_vec::grow\n   1: cavalry_bench::entry";
        let message =
            allocation_failure(&by_benchmark, Some(frames)).expect("one allocation fails");
        assert!(
            message.contains("allocated 1 times, by benchmark udp 1"),
            "{message}"
        );
        assert!(message.contains("frame by frame"), "{message}");
        assert!(message.contains("0: alloc::raw_vec::grow"), "{message}");
        let message = allocation_failure(&by_benchmark, None).expect("one allocation fails");
        assert!(message.contains("no backtrace was captured"), "{message}");
    }

    /// The tests that open a counting window take this first, so they run one at a
    /// time: tracking is process-wide, and one test's window closing would stop the
    /// count inside another's. The tests that open none run beside them, and their
    /// allocations count in an open window, so a window's count is a lower bound.
    static ONE_WINDOW: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn one_window() -> std::sync::MutexGuard<'static, ()> {
        ONE_WINDOW
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// A counting window counts the allocations made while it is open and keeps the
    /// first one's backtrace, captured inside the allocator's hook.
    #[test]
    fn a_counting_window_counts_and_keeps_the_first_backtrace() {
        let _one = one_window();
        // The tracker is process-wide and set once; another test may have set it.
        let _ = AllocationRegistry::set_global_tracker(CountingTracker);
        let window = Counting::open();
        let grown: Vec<u8> = std::hint::black_box(vec![7u8; 4096]);
        let counted = window.close();
        assert_eq!(grown.len(), 4096);
        assert!(counted >= 1, "counted {counted}");
        let first = first_allocation().expect("the first allocation's backtrace");
        assert!(!first.is_empty());
    }

    /// A vector grown past a capacity it already had counts: the allocator keeps
    /// `GlobalAlloc`'s default `realloc`, which allocates anew through the hooked
    /// `alloc`, copies and frees. `stats_alloc` counted a reallocation apart from the
    /// allocations the gate read, so before ADR 0047 none of these counted.
    #[test]
    fn a_counting_window_counts_a_vector_grown_past_its_capacity() {
        // Each grow below is from a capacity of one or more, so each is a reallocation
        // and none an allocation from empty; 64 of them, so that the count is the
        // grows' and not the few allocations another test makes inside the window.
        const GROWS: usize = 64;
        let _one = one_window();
        let _ = AllocationRegistry::set_global_tracker(CountingTracker);
        let mut grown: Vec<u8> = Vec::with_capacity(1);
        grown.push(0);
        let window = Counting::open();
        for _ in 0..GROWS {
            grown.reserve_exact(1);
            grown.push(7);
            std::hint::black_box(&mut grown);
        }
        let counted = window.close();
        assert_eq!(grown.len(), GROWS + 1);
        assert!(
            counted >= GROWS,
            "counted {counted} in {GROWS} reallocations"
        );
    }

    /// A pile is a stopped recording the driver's open leaves alone: its slot holds
    /// the stop at the end of its last file, every file is there at the segment length
    /// and sparse, the recovery pass finds nothing to repair, and the next recording
    /// takes the next slot.
    #[test]
    fn a_pile_is_a_stopped_recording_the_driver_leaves_alone() {
        use cavalry_log::catalog::CatalogReader;
        use cavalry_log::dir::Recovery;
        let dir = std::env::temp_dir().join(format!("cavalry-bench-pile-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let pile = build_pile(&dir, 8).expect("the pile");
        assert_eq!((pile.recording_id, pile.segments), (0, 8));
        let reader = CatalogReader::open(&dir).expect("the catalog");
        assert_eq!(reader.slots().expect("slots"), 1);
        let slot = reader.get(0).expect("slot 0").expect("the pile's slot");
        assert_eq!(slot.stop_position(), pile.stop_position());
        assert!(!slot.is_live());
        for k in 0..8 {
            let path = dir.join(segment_file_name(0, k * pile.segment_length));
            let length = std::fs::metadata(&path).expect("the file").len();
            assert_eq!(length, pile.segment_length as u64, "{}", path.display());
        }
        drop(reader);
        // The driver's open: recovery leaves the slot as it is, and a recording added
        // next takes slot 1.
        let now = realtime_nanos() as i64;
        let (mut log, recovery) = LogDir::open(&dir, now).expect("the driver's open");
        assert_eq!(recovery, Recovery::default());
        assert_eq!(log.catalog().slots(), 1);
        let writer = log
            .start(
                &Recording {
                    recording_id: 0,
                    start_timestamp: now,
                    start_position: 0,
                    initial_term_id: 7,
                    term_length: 64 * 1024,
                    mtu_length: MTU as i32,
                    session_id: 1,
                    stream_id: STREAM_ID,
                    channel: "cavalry:ipc",
                    source_identity: "cavalry:ipc",
                },
                now,
            )
            .expect("the next recording");
        assert_eq!(writer.recording_id(), 1);
        drop(writer);
        drop(log);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A pile is what the file system removes in the pile's seconds, within its
    /// bounds: the laptop's and the Linux host's measured rates, and both ends.
    #[test]
    fn a_pile_is_sized_by_the_removal_rate_within_its_bounds() {
        assert_eq!(pile_segments(0.0), PILE_LEAST);
        assert_eq!(pile_segments(f64::NAN), PILE_LEAST);
        assert_eq!(pile_segments(22_000.0), 55_000);
        assert_eq!(pile_segments(300_000.0), 750_000);
        assert_eq!(pile_segments(590_000.0), PILE_MOST);
        assert_eq!(pile_segments(5_000_000.0), PILE_MOST);
        assert_eq!(pile_segments(f64::INFINITY), PILE_MOST);
    }

    /// The probe measures a rate above zero, in a directory of its own beside the log
    /// directory, and leaves neither it nor the log directory behind.
    #[test]
    fn the_probe_measures_a_removal_rate_and_leaves_nothing_behind() {
        let log_dir =
            std::env::temp_dir().join(format!("cavalry-bench-probe-{}.log", std::process::id()));
        let rate = removal_rate(&log_dir).expect("the probe");
        assert!(rate.is_finite() && rate > 0.0, "{rate}");
        assert!(!log_dir.with_extension("probe").exists());
        assert!(!log_dir.exists());
    }

    /// An empty directory of the test's own to stand for the bench directory.
    fn sweep_scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sweep-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the scratch directory");
        dir
    }

    /// The names left in a scratch directory, sorted.
    fn names_in(dir: &std::path::Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .expect("the scratch directory")
            .map(|entry| {
                entry
                    .expect("an entry")
                    .file_name()
                    .into_string()
                    .expect("a name")
            })
            .collect();
        names.sort();
        names
    }

    /// A name is the harness's when the process id stands where the harness puts it,
    /// written as the harness writes it.
    #[test]
    fn a_bench_directorys_owner_is_read_from_its_name_and_no_other_name_has_one() {
        assert_eq!(
            bench_dir_owner("cavalry-bench-1205210-link"),
            Some(1_205_210)
        );
        assert_eq!(
            bench_dir_owner("cavalry-bench-77-recorded_purging.log"),
            Some(77)
        );
        assert_eq!(bench_dir_owner("cavalry-bench-77-udp-mdc"), Some(77));
        for name in [
            "cavalry-bench-pile-77",
            "cavalry-bench-probe-77.log",
            "cavalry-bench-77",
            "cavalry-bench--77-link",
            "cavalry-bench-77x-link",
            "cavalry-bench-+77-link",
            "cavalry-bench-077-link",
            "cavalry-bench-0-link",
            "cavalry-bench-4294967296-link",
            "cavalry-77-link",
            "cavalry-driver-77",
            "",
        ] {
            assert_eq!(bench_dir_owner(name), None, "{name}");
        }
    }

    /// What a killed process left goes, whole: its row's directory with the files in
    /// it, the log directory and the probe beside it, and a file under its name. What a
    /// running process keeps stays, and so does every name the harness did not give.
    /// A running process is asked for once, however many entries it has; an ended one
    /// before each of its entries goes.
    #[test]
    fn a_sweep_removes_a_dead_processs_entries_and_leaves_a_running_ones_and_other_names() {
        let dir = sweep_scratch("dead");
        let (dead, running) = (4_000_001_u32, 4_000_002_u32);
        for name in [
            format!("cavalry-bench-{dead}-recorded"),
            format!("cavalry-bench-{dead}-recorded.log"),
            format!("cavalry-bench-{dead}-recorded.probe"),
            format!("cavalry-bench-{running}-udp"),
            format!("cavalry-bench-{running}-udp.log"),
            "cavalry-bench-pile-4000001".to_string(),
            "cavalry-bench-+4000001-link".to_string(),
            "cavalry-driver".to_string(),
        ] {
            std::fs::create_dir_all(dir.join(&name).join("nested")).expect("an entry");
            std::fs::write(dir.join(&name).join("nested").join("file"), b"left").expect("a file");
        }
        std::fs::write(dir.join(format!("cavalry-bench-{dead}-file")), b"left").expect("a file");
        std::fs::write(dir.join("other.log"), b"kept").expect("a file");
        let mut kept = vec![
            format!("cavalry-bench-{running}-udp"),
            format!("cavalry-bench-{running}-udp.log"),
            "cavalry-bench-+4000001-link".to_string(),
            "cavalry-bench-pile-4000001".to_string(),
            "cavalry-driver".to_string(),
            "other.log".to_string(),
        ];
        kept.sort();

        let asked = std::cell::RefCell::new(Vec::new());
        let runs = |pid: u32| {
            asked.borrow_mut().push(pid);
            pid != dead
        };
        let swept = sweep_dead(&dir, |_| true, runs);
        assert_eq!(
            swept,
            Swept {
                removed: 4,
                ..Swept::default()
            }
        );
        assert_eq!(names_in(&dir), kept);
        assert!(
            dir.join(format!("cavalry-bench-{running}-udp/nested/file"))
                .exists()
        );
        let asked = asked.into_inner();
        assert_eq!(asked.iter().filter(|pid| **pid == running).count(), 1);
        assert_eq!(asked.iter().filter(|pid| **pid == dead).count(), 4);
        let line = swept_line(&swept, &dir).expect("a line");
        assert!(
            line.starts_with("cavalry-bench: removed 4 entries under ")
                && line.ends_with("(docs/how-to/run-the-bench.md#when-it-does-not-work)")
                && !line.contains("another user"),
            "{line}"
        );
        // A second sweep finds nothing to do, and says nothing.
        let again = sweep_dead(&dir, |_| true, |pid| pid != dead);
        assert_eq!(again, Swept::default());
        assert_eq!(swept_line(&again, &dir), None);
        std::fs::remove_dir_all(&dir).expect("removing the scratch directory");
    }

    /// Another user's entry of a process that has ended is counted and not touched,
    /// not a file of it, and the line says whose it is to remove.
    #[test]
    fn a_sweep_does_not_touch_another_users_entries_and_says_so() {
        let dir = sweep_scratch("others");
        let theirs = dir.join("cavalry-bench-4000001-link");
        let ours = dir.join("cavalry-bench-4000001-udp");
        for entry in [&theirs, &ours] {
            std::fs::create_dir_all(entry.join("nested")).expect("an entry");
            std::fs::write(entry.join("nested").join("file"), b"left").expect("a file");
        }
        let swept = sweep_dead(&dir, |path| path != theirs, |_| false);
        assert_eq!(
            swept,
            Swept {
                removed: 1,
                others: 1,
                ..Swept::default()
            }
        );
        assert_eq!(names_in(&dir), ["cavalry-bench-4000001-link"]);
        assert!(theirs.join("nested").join("file").exists());
        let line = swept_line(&swept, &dir).expect("a line");
        assert!(
            line.starts_with("cavalry-bench: removed 1 entry under ")
                && line.contains("; 1 entry under ")
                && line.contains("belong to another user and were not touched")
                && line.ends_with("(docs/how-to/run-the-bench.md#when-it-does-not-work)"),
            "{line}"
        );
        // With nothing of this user's to remove, the line does not open with a removal.
        let swept = sweep_dead(&dir, |_| false, |_| false);
        let line = swept_line(&swept, &dir).expect("a line");
        assert!(
            line.starts_with("cavalry-bench: 1 entry under ") && !line.contains("removed"),
            "{line}"
        );
        std::fs::remove_dir_all(&dir).expect("removing the scratch directory");
    }

    /// An entry of this user's that will not go, here a directory in it that cannot be
    /// written, is counted with its error and is not called another user's. It stays
    /// under the sweeping process's name. A user that file modes do not hold, root,
    /// removes it, and the test then has only that to see.
    #[cfg(unix)]
    #[test]
    fn a_sweep_reports_what_it_could_not_remove_with_the_error() {
        use std::os::unix::fs::PermissionsExt;
        let dir = sweep_scratch("failed");
        let entry = dir.join("cavalry-bench-4000001-link");
        std::fs::create_dir_all(entry.join("locked")).expect("an entry");
        std::fs::write(entry.join("locked").join("file"), b"left").expect("a file");
        let lock = |path: &std::path::Path, mode| {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("modes");
        };
        lock(&entry.join("locked"), 0o555);

        let swept = sweep_dead(&dir, |_| true, |_| false);
        let taken = dir.join(format!("cavalry-bench-{}-swept-0", std::process::id()));
        if taken.exists() {
            lock(&taken.join("locked"), 0o755);
            assert_eq!(
                (swept.removed, swept.others, swept.failed, swept.untold),
                (0, 0, 1, 0)
            );
            let line = swept_line(&swept, &dir).expect("a line");
            assert!(
                line.starts_with(
                    "cavalry-bench: 1 entry that such processes left could not be removed, \
                     the first: "
                ) && line.contains("-swept-0: ")
                    && !line.contains("another user")
                    && line.ends_with("(docs/how-to/run-the-bench.md#when-it-does-not-work)"),
                "{line}"
            );
        } else {
            assert_eq!(
                swept,
                Swept {
                    removed: 1,
                    ..Swept::default()
                }
            );
        }
        std::fs::remove_dir_all(&dir).expect("removing the scratch directory");
    }

    /// An entry another sweep took first is that sweep's to count: this one finds it
    /// gone and counts nothing, neither removed nor failed. The other sweep is played
    /// by the question itself, which removes the entry as it answers.
    #[test]
    fn an_entry_another_sweep_took_first_is_counted_by_neither_as_this_ones() {
        let dir = sweep_scratch("raced");
        let entry = dir.join("cavalry-bench-4000001-link");
        std::fs::create_dir_all(entry.join("nested")).expect("an entry");
        let swept = sweep_dead(
            &dir,
            |_| true,
            |_| {
                std::fs::remove_dir_all(&entry).expect("the other sweep");
                false
            },
        );
        assert_eq!(swept, Swept::default());
        assert_eq!(swept_line(&swept, &dir), None);
        assert_eq!(names_in(&dir), Vec::<String>::new());
        std::fs::remove_dir_all(&dir).expect("removing the scratch directory");
    }

    /// A name an earlier process with this one's id left in use is passed over, and
    /// the entry is taken under the next and removed.
    #[test]
    fn a_taken_name_already_in_use_is_passed_over() {
        let dir = sweep_scratch("names");
        let me = std::process::id();
        let stale = format!("cavalry-bench-{me}-swept-0");
        std::fs::create_dir_all(dir.join(&stale).join("nested")).expect("a stale entry");
        std::fs::create_dir_all(dir.join("cavalry-bench-4000001-link").join("nested"))
            .expect("an entry");
        assert_eq!(
            taken_name(&dir),
            Some(dir.join(format!("cavalry-bench-{me}-swept-1")))
        );
        let swept = sweep_dead(&dir, |_| true, |pid| pid == me);
        assert_eq!(
            swept,
            Swept {
                removed: 1,
                ..Swept::default()
            }
        );
        assert_eq!(names_in(&dir), [stale]);
        std::fs::remove_dir_all(&dir).expect("removing the scratch directory");
    }

    /// The way to ask finds this process, or it is `Unknown` and says every process
    /// runs. Where it can tell, an id no process has does not run, and neither does a
    /// child that has ended and been waited for; and it tells this process's user,
    /// which is the owner of what this process creates.
    #[test]
    fn a_running_process_is_told_from_an_ended_one_or_nothing_is_called_ended() {
        let processes = Processes::detect();
        assert!(processes.runs(std::process::id()));
        assert!(Processes::Unknown.runs(u32::MAX - 1));
        if processes == Processes::Unknown {
            return;
        }
        #[cfg(unix)]
        assert!(processes.runs(std::os::unix::process::parent_id()));
        assert!(!processes.runs(u32::MAX - 1));
        let mut child =
            std::process::Command::new(std::env::current_exe().expect("this test binary"))
                .arg("--list")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .expect("a child process");
        let pid = child.id();
        child.wait().expect("the child's end");
        assert!(!processes.runs(pid), "{pid}");
        #[cfg(unix)]
        {
            assert_eq!(Processes::Unknown.user(), None);
            let dir = sweep_scratch("user");
            assert_eq!(processes.user(), owning_user(&dir));
            std::fs::remove_dir_all(&dir).expect("removing the scratch directory");
        }
    }

    /// The harness's own sweep takes this user's entry of an ended process and leaves
    /// one named for this process.
    #[cfg(unix)]
    #[test]
    fn the_harnesss_sweep_removes_this_users_dead_entries_and_keeps_this_processs() {
        let processes = Processes::detect();
        if processes == Processes::Unknown {
            return;
        }
        let dir = sweep_scratch("own");
        let dead = format!("cavalry-bench-{}-recorded", u32::MAX - 1);
        let alive = format!("cavalry-bench-{}-udp", std::process::id());
        for name in [&dead, &alive] {
            std::fs::create_dir_all(dir.join(name).join("nested")).expect("an entry");
        }
        let swept = sweep_as(&dir, processes);
        assert_eq!(
            swept,
            Swept {
                removed: 1,
                ..Swept::default()
            }
        );
        assert_eq!(names_in(&dir), [alive]);
        std::fs::remove_dir_all(&dir).expect("removing the scratch directory");
    }

    /// Where no way to ask finds this process, nothing is touched, and the line says
    /// how many entries are named for other processes and that none was removed.
    #[test]
    fn a_sweep_that_cannot_tell_an_ended_process_touches_nothing_and_says_what_is_there() {
        let dir = sweep_scratch("untold");
        let mut names = vec![
            format!("cavalry-bench-{}-recorded", u32::MAX - 1),
            format!("cavalry-bench-{}-recorded.log", u32::MAX - 1),
            format!("cavalry-bench-{}-udp", std::process::id()),
            "cavalry-driver".to_string(),
        ];
        for name in &names {
            std::fs::create_dir_all(dir.join(name).join("nested")).expect("an entry");
        }
        names.sort();
        let swept = sweep_as(&dir, Processes::Unknown);
        assert_eq!(
            swept,
            Swept {
                untold: 2,
                ..Swept::default()
            }
        );
        assert_eq!(names_in(&dir), names);
        let line = swept_line(&swept, &dir).expect("a line");
        assert!(
            line.starts_with("cavalry-bench: 2 entries under ")
                && line.contains("cannot be told here")
                && line.contains("none was removed")
                && line.ends_with("(docs/how-to/run-the-bench.md#when-it-does-not-work)"),
            "{line}"
        );
        std::fs::remove_dir_all(&dir).expect("removing the scratch directory");
    }

    /// The recorded benchmark of [`Benchmark::ALL`], wherever it stands there.
    fn recorded_benchmark() -> Benchmark {
        *Benchmark::ALL
            .iter()
            .find(|benchmark| {
                matches!(
                    benchmark,
                    Benchmark::Client {
                        recorded: true,
                        purging: false,
                        ..
                    }
                )
            })
            .expect("the recorded benchmark")
    }

    /// `--purging` adds the recorded rows with a purge running, right after the
    /// recorded rows, and the rows grow by its two.
    #[test]
    fn purging_adds_the_recorded_purging_benchmark_after_recorded() {
        let without = options(&[]).rows().len();
        let options = options(&["--purging"]);
        let recorded = Benchmark::ALL
            .iter()
            .position(|benchmark| *benchmark == recorded_benchmark())
            .expect("the recorded benchmark");
        let mut expected = Benchmark::ALL.to_vec();
        expected.insert(recorded + 1, Benchmark::PURGING);
        assert_eq!(options.benchmarks(), expected);
        assert_eq!(options.rows().len(), without + 2);
        assert!(
            options
                .rows()
                .contains(&"one_way_32_recorded_purging".to_string())
        );
    }

    /// `--only recorded_purging` runs that benchmark alone under `--purging`, and is
    /// an error without the flag, since no row of such a run has the name; `--only
    /// recorded` under the flag admits both recorded benchmarks.
    #[test]
    fn only_recorded_purging_needs_purging() {
        assert_eq!(
            options(&["--only", "recorded_purging", "--purging"]).benchmarks(),
            vec![Benchmark::PURGING]
        );
        assert!(parsed(&["--only", "recorded_purging"]).is_err());
        assert_eq!(
            options(&["--only", "recorded", "--purging"]).benchmarks(),
            vec![recorded_benchmark(), Benchmark::PURGING]
        );
    }

    /// A pass keeps the purge's figures under the benchmark's name, and a run without
    /// a purge leaves none.
    #[test]
    fn a_pass_keeps_the_purge_figures_by_benchmark() {
        let empty = Summary {
            count: 0,
            p50_ns: 0,
            p99_ns: 0,
            p99_99_ns: 0,
            max_ns: 0,
            rate_per_s: 0.0,
        };
        let run = |purged| LogRun {
            claim_commit: empty.clone(),
            one_way: empty.clone(),
            retries: 0,
            allocations: 0,
            producer_position_loads: None,
            purged,
        };
        let purged = Purged {
            during: 3107,
            pile: 100_000,
            outlasted: true,
        };
        let mut pass = Pass::default();
        pass.insert_message_run(
            Benchmark::PURGING,
            "recorded_purging",
            32,
            run(Some(purged)),
            10,
        );
        pass.insert_message_run(recorded_benchmark(), "recorded", 32, run(None), 10);
        assert_eq!(
            pass.purged,
            BTreeMap::from([("recorded_purging".to_string(), purged)])
        );
    }
}
