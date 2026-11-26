// The sleep probe (work item 338; ADR 0171;
// docs/how-to/find-a-wait-that-does-not-return.md): does a timed wait of at most a
// millisecond ever fail to return on this machine, in which call, and does the thread
// come back when it is woken? Standard library only, and the system's C library by
// name. It is no part of the workspace and no gate runs it:
//
//   rustc -O --edition 2021 scripts/sleep-probe.rs -o target/sleep-probe
//   target/sleep-probe <seconds> <sleepers> <spinners> [signals]
//
// Each sleeper thread repeats the backoff of Cavalry's idle loops, a wait of 1 µs
// doubling to 1 ms, starting again from 1 µs every 64 waits as a loop that found work
// does. The threads are dealt out over the kinds of wait in KINDS: the standard
// library's sleep, `nanosleep` called here with a place of its own for the time
// remaining, the standard library's park and its condition variable, and on macOS
// `mach_wait_until` and a `kevent` with a timeout.
//
// The main thread watches with a park of 200 ms. It prints STUCK for a sleeper whose
// one wait has lasted three seconds, takes a `sample` of the process at the first one
// (macOS), and after thirty seconds wakes the thread: `unpark` for the kind that
// parks, and for the others a signal that a handler which does nothing takes. RETURNED
// says when the thread came back, and whether by itself. Every minute it prints each
// kind's count of waits and its longest single wait.
//
// <spinners> threads spin beside the sleepers, to load the machine. With a fourth
// argument, one more thread sends every sleeper the signal all the time, so that
// `nanosleep` returns EINTR and the remainder it writes is checked: an "odd
// remainder" is one that is negative or more than was asked.
//
// It ends with 0 when no wait stuck, 1 when some did and all returned, and 2 when one
// was still stuck at the end, without joining the threads, since one cannot be joined.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Timespec {
    tv_sec: i64,
    tv_nsec: i64,
}

extern "C" {
    fn nanosleep(rqtp: *const Timespec, rmtp: *mut Timespec) -> i32;
    #[cfg(target_os = "macos")]
    fn __error() -> *mut i32;
    #[cfg(not(target_os = "macos"))]
    fn __errno_location() -> *mut i32;
    fn pthread_self() -> usize;
    fn pthread_kill(thread: usize, signal: i32) -> i32;
    fn signal(signal: i32, handler: extern "C" fn(i32)) -> usize;
}

#[cfg(target_os = "macos")]
#[repr(C)]
struct Timebase {
    numer: u32,
    denom: u32,
}

#[cfg(target_os = "macos")]
extern "C" {
    fn mach_absolute_time() -> u64;
    fn mach_wait_until(deadline: u64) -> i32;
    fn mach_timebase_info(info: *mut Timebase) -> i32;
    fn kqueue() -> i32;
    fn kevent(
        queue: i32,
        changes: *const u8,
        change_count: i32,
        events: *mut u8,
        event_count: i32,
        timeout: *const Timespec,
    ) -> i32;
}

extern "C" fn nothing(_: i32) {}

const EINTR: i32 = 4;
#[cfg(target_os = "macos")]
const SIGUSR1: i32 = 30;
#[cfg(not(target_os = "macos"))]
const SIGUSR1: i32 = 10;

const STUCK: Duration = Duration::from_secs(3);
const WAKE_AFTER: Duration = Duration::from_secs(30);

const SLEEP: &str = "std::thread::sleep";
const NANOSLEEP: &str = "nanosleep";
const PARK: &str = "std::thread::park_timeout";
const CONDVAR: &str = "Condvar::wait_timeout";
#[cfg(target_os = "macos")]
const MACH: &str = "mach_wait_until";
#[cfg(target_os = "macos")]
const KEVENT: &str = "kevent with a timeout";

#[cfg(target_os = "macos")]
const KINDS: &[&str] = &[SLEEP, NANOSLEEP, PARK, CONDVAR, MACH, KEVENT];
#[cfg(not(target_os = "macos"))]
const KINDS: &[&str] = &[SLEEP, NANOSLEEP, PARK, CONDVAR];

fn errno() -> i32 {
    #[cfg(target_os = "macos")]
    unsafe {
        *__error()
    }
    #[cfg(not(target_os = "macos"))]
    unsafe {
        *__errno_location()
    }
}

#[derive(Default)]
struct Sleeper {
    /// Nanoseconds since the start at which the wait in progress began; 0 between waits.
    began: AtomicU64,
    /// Nanoseconds asked of the wait in progress.
    asked: AtomicU64,
    waits: AtomicU64,
    interrupts: AtomicU64,
    /// The longest one wait took, in nanoseconds.
    worst: AtomicU64,
    /// A remainder an interrupted `nanosleep` wrote that is negative or more than was asked.
    odd: AtomicU64,
    odd_sec: AtomicI64,
    odd_nsec: AtomicI64,
    odd_asked: AtomicU64,
    thread: AtomicU64,
}

/// `nanosleep` as the standard library calls it, but with the remainder in a place of
/// its own, checked before it is followed.
fn nanosleep_checked(sleeper: &Sleeper, park: Duration) {
    let mut ask = Timespec { tv_sec: 0, tv_nsec: park.as_nanos() as i64 };
    loop {
        let mut left = Timespec { tv_sec: -7, tv_nsec: -7 };
        if unsafe { nanosleep(&ask, &mut left) } == 0 {
            return;
        }
        let errno = errno();
        assert_eq!(errno, EINTR, "nanosleep failed with errno {errno}");
        sleeper.interrupts.fetch_add(1, Ordering::Relaxed);
        let asked = ask.tv_sec as i128 * 1_000_000_000 + ask.tv_nsec as i128;
        let remains = left.tv_sec as i128 * 1_000_000_000 + left.tv_nsec as i128;
        if remains > asked || remains < 0 || left.tv_nsec < 0 || left.tv_nsec >= 1_000_000_000 {
            sleeper.odd_sec.store(left.tv_sec, Ordering::Relaxed);
            sleeper.odd_nsec.store(left.tv_nsec, Ordering::Relaxed);
            sleeper.odd_asked.store(asked as u64, Ordering::Relaxed);
            sleeper.odd.fetch_add(1, Ordering::Release);
            return; // an odd remainder is not followed into a long sleep
        }
        if remains == 0 {
            return;
        }
        ask = left;
    }
}

fn sleeper_loop(kind: &'static str, sleeper: &Sleeper, stop: &AtomicBool, start: Instant) {
    sleeper.thread.store(unsafe { pthread_self() } as u64, Ordering::Release);
    let pair = (Mutex::new(()), Condvar::new());
    #[cfg(target_os = "macos")]
    let (timebase, queue) = {
        let mut timebase = Timebase { numer: 1, denom: 1 };
        unsafe { mach_timebase_info(&mut timebase) };
        (timebase, unsafe { kqueue() })
    };
    let mut park = Duration::from_micros(1);
    while !stop.load(Ordering::Acquire) {
        let before = start.elapsed().as_nanos() as u64;
        sleeper.asked.store(park.as_nanos() as u64, Ordering::Release);
        sleeper.began.store(before.max(1), Ordering::Release);
        match kind {
            SLEEP => std::thread::sleep(park),
            NANOSLEEP => nanosleep_checked(sleeper, park),
            // A park may return early; an idle loop then only turns once more.
            PARK => std::thread::park_timeout(park),
            CONDVAR => {
                let guard = pair.0.lock().unwrap();
                drop(pair.1.wait_timeout(guard, park).unwrap());
            }
            #[cfg(target_os = "macos")]
            MACH => {
                let ticks = park.as_nanos() as u64 * timebase.denom as u64 / timebase.numer as u64;
                unsafe { mach_wait_until(mach_absolute_time() + ticks) };
            }
            #[cfg(target_os = "macos")]
            KEVENT => {
                let timeout = Timespec { tv_sec: 0, tv_nsec: park.as_nanos() as i64 };
                let mut event = [0u8; 64];
                unsafe { kevent(queue, std::ptr::null(), 0, event.as_mut_ptr(), 1, &timeout) };
            }
            _ => unreachable!("a kind of KINDS"),
        }
        let took = start.elapsed().as_nanos() as u64 - before;
        sleeper.began.store(0, Ordering::Release);
        let waits = sleeper.waits.fetch_add(1, Ordering::Relaxed) + 1;
        sleeper.worst.fetch_max(took, Ordering::Relaxed);
        park = (park * 2).min(Duration::from_millis(1));
        if waits % 64 == 0 {
            park = Duration::from_micros(1);
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let number = |at: usize, default: u64| args.get(at).and_then(|a| a.parse().ok()).unwrap_or(default);
    let (seconds, sleepers, spinners) = (number(1, 60), number(2, 32) as usize, number(3, 0) as usize);
    let signals = args.get(4).is_some();
    let start = Instant::now();
    let stop = Arc::new(AtomicBool::new(false));
    unsafe { signal(SIGUSR1, nothing) };

    let state: Vec<Arc<Sleeper>> = (0..sleepers).map(|_| Arc::new(Sleeper::default())).collect();
    let kind_of = |index: usize| KINDS[index % KINDS.len()];
    let mut handles = Vec::new();
    let mut threads = Vec::new();
    for (index, sleeper) in state.iter().enumerate() {
        let (sleeper, stop, kind) = (Arc::clone(sleeper), Arc::clone(&stop), kind_of(index));
        let handle = std::thread::Builder::new()
            .name(format!("sleeper-{index}"))
            .spawn(move || sleeper_loop(kind, &sleeper, &stop, start))
            .expect("a thread");
        threads.push(handle.thread().clone());
        handles.push(handle);
    }
    for _ in 0..spinners {
        let stop = Arc::clone(&stop);
        handles.push(std::thread::spawn(move || {
            let mut x = 0u64;
            while !stop.load(Ordering::Relaxed) {
                for _ in 0..100_000 {
                    x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
                }
                std::hint::black_box(x);
            }
        }));
    }
    if signals {
        let (state, stop) = (state.clone(), Arc::clone(&stop));
        handles.push(std::thread::spawn(move || {
            let mut at = 0usize;
            while !stop.load(Ordering::Acquire) {
                let thread = state[at % state.len().max(1)].thread.load(Ordering::Acquire) as usize;
                if thread != 0 {
                    unsafe { pthread_kill(thread, SIGUSR1) };
                }
                at += 1;
                std::thread::park_timeout(Duration::from_micros(137));
            }
        }));
    }

    let mut stuck = vec![false; sleepers];
    let mut woken = vec![false; sleepers];
    let mut stuck_total = 0u64;
    let mut sampled = false;
    let mut reported = Instant::now();
    while start.elapsed() < Duration::from_secs(seconds) {
        std::thread::park_timeout(Duration::from_millis(200));
        let now = start.elapsed().as_nanos() as u64;
        let at = now as f64 / 1e9;
        for (index, sleeper) in state.iter().enumerate() {
            let began = sleeper.began.load(Ordering::Acquire);
            let lasting = if began == 0 { 0 } else { now.saturating_sub(began) };
            let kind = kind_of(index);
            if lasting > STUCK.as_nanos() as u64 && !stuck[index] {
                stuck[index] = true;
                stuck_total += 1;
                if !sampled && cfg!(target_os = "macos") {
                    sampled = true;
                    let file = format!("sleep-probe-sample-{}.txt", std::process::id());
                    let _ = std::process::Command::new("sample")
                        .args([&std::process::id().to_string(), "1", "-file", &file])
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .status();
                    println!("SAMPLED into {file}");
                }
                println!(
                    "STUCK sleeper-{index} ({kind}) at {at:.1}s: one wait of {} ns asked has lasted {:.1}s; {} waits before it, {} interrupts",
                    sleeper.asked.load(Ordering::Acquire),
                    lasting as f64 / 1e9,
                    sleeper.waits.load(Ordering::Relaxed),
                    sleeper.interrupts.load(Ordering::Relaxed)
                );
            } else if stuck[index] && lasting < STUCK.as_nanos() as u64 {
                stuck[index] = false;
                println!(
                    "RETURNED sleeper-{index} ({kind}) at {at:.1}s, {}: its longest wait took {:.1}s",
                    if woken[index] { "after it was woken" } else { "by itself" },
                    sleeper.worst.load(Ordering::Relaxed) as f64 / 1e9
                );
                woken[index] = false;
            } else if stuck[index] && !woken[index] && lasting > WAKE_AFTER.as_nanos() as u64 {
                woken[index] = true;
                if kind == PARK {
                    threads[index].unpark();
                    println!("WOKE sleeper-{index} ({kind}) with unpark at {at:.1}s");
                } else {
                    unsafe { pthread_kill(sleeper.thread.load(Ordering::Acquire) as usize, SIGUSR1) };
                    println!("WOKE sleeper-{index} ({kind}) with a signal at {at:.1}s");
                }
            }
        }
        if reported.elapsed() > Duration::from_secs(60) {
            reported = Instant::now();
            summary(&state, start, "so far");
        }
    }
    summary(&state, start, "end");
    let still = stuck.iter().filter(|s| **s).count();
    println!("stuck sleeps seen: {stuck_total}; still stuck at the end: {still}");
    stop.store(true, Ordering::Release);
    if still > 0 {
        std::process::exit(2);
    }
    for handle in handles {
        handle.thread().unpark();
        let _ = handle.join();
    }
    std::process::exit(if stuck_total > 0 { 1 } else { 0 });
}

fn summary(state: &[Arc<Sleeper>], start: Instant, when: &str) {
    for (at, kind) in KINDS.iter().enumerate() {
        let of_kind: Vec<&Arc<Sleeper>> =
            state.iter().enumerate().filter(|(index, _)| index % KINDS.len() == at).map(|(_, s)| s).collect();
        let sum = |field: fn(&Sleeper) -> &AtomicU64| -> u64 { of_kind.iter().map(|s| field(s).load(Ordering::Relaxed)).sum() };
        let worst = of_kind.iter().map(|s| s.worst.load(Ordering::Relaxed)).max().unwrap_or(0);
        println!(
            "{when} {:.0}s {kind}: {} waits, {} interrupts, longest one wait {:.3} ms, odd remainders {}",
            start.elapsed().as_secs_f64(),
            sum(|s| &s.waits),
            sum(|s| &s.interrupts),
            worst as f64 / 1e6,
            sum(|s| &s.odd)
        );
        if let Some(s) = of_kind.iter().find(|s| s.odd.load(Ordering::Acquire) > 0) {
            println!(
                "  ODD remainder: asked {} ns, the interrupted call wrote tv_sec {} tv_nsec {}",
                s.odd_asked.load(Ordering::Relaxed),
                s.odd_sec.load(Ordering::Relaxed),
                s.odd_nsec.load(Ordering::Relaxed)
            );
        }
    }
}
