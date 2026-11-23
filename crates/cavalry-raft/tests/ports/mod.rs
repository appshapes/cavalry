//! Loopback ports for a test whose driver binds a port that the test chose: the
//! endpoints of a cluster, the endpoint of a `cavalry:udp` subscription, a control
//! channel. In a module of its own so that its rule is written once. This crate's
//! tests under `tests/` take it with `mod ports;`. The others take this same file by
//! path and are what breaks, by name, if the file moves: the module's own tests,
//! `src/module/tests.rs`, with `#[path = "../../tests/ports/mod.rs"] mod ports;`, and
//! tests of four more crates, `cavalry-client`, `cavalry-ffi`, `cavalry-driver` and
//! `cavalry`, each with `#[path = "../../cavalry-raft/tests/ports/mod.rs"]`. `git grep
//! -n 'cavalry-raft/tests/ports/mod.rs'` lists every one of them.
//!
//! # The rule
//!
//! A test chooses a port, names it in a channel, and a driver binds it later: at the
//! test's next command, or, for a follower's `log` endpoint, only when the member joins
//! the log, and again when it leaves the log and joins it again (ADR 0068). Until the
//! driver binds it the port is free, and whatever else binds it first makes the
//! driver's bind fail with `Address already in use`, error code 4. Three things could
//! bind it first, and the rule keeps each one away:
//!
//! - **A socket bound to port 0**, which the operating system gives a port of its own
//!   choice: a send socket of a driver, which every publication has, or a socket of
//!   any other process. The ports here are from 20000 to 31999, below the range the
//!   operating system chooses from, which starts at 32768 on Linux and at 49152 on
//!   macOS.
//! - **Another test of the same process.** Each port is chosen once in a process.
//! - **A test of another process**, when two sessions run `just test` on one host. The
//!   range is cut into [`SLOTS`] slots of [`SLOT`] ports. A process takes a slot by
//!   binding the slot's first port, keeps that socket for as long as it runs, and
//!   chooses its ports from the slots it holds alone. It takes one slot at its first
//!   choice, and another only when it has tried every port of the last it took, up to
//!   [`PER_PROCESS`] slots. The operating system closes the sockets when the process
//!   ends, however it ends, so a slot is never left taken.
//!
//! # The bound, and how near a process is to it
//!
//! A process chooses at most [`PER_PROCESS`] × ([`SLOT`] − 1) ports: 1,996. The
//! largest user is this crate's library, `src/module/tests.rs`, whose tests choose 475
//! ports in one process, most of them ten for each pair of drivers; on 2026-11-23 they
//! had nearly spent the one slot a process then held (work item 422). Each slot a process holds is
//! a socket bound to the slot's first port, 20000 plus a multiple of 500, for as long as
//! the process runs, so `lsof -nP -iUDP@127.0.0.1 -a -p <pid>` lists the slots a test
//! process holds while it runs: one that holds three is within a slot of its bound. A
//! process that has tried every port of its last slot panics and says what to change.
//! With 24 slots, six processes can each hold four at once.
//!
//! # What the rule does not cover
//!
//! - A process that is not one of these tests and binds a port from 20000 to 31999 by
//!   its number. A port in use when it is chosen is passed over, so only a bind
//!   between the choice and the driver's own is a failure.
//! - A host whose range for port 0 was changed to reach below 32000:
//!   `net.ipv4.ip_local_port_range` on Linux, `net.inet.ip.portrange.first` on macOS.
//! - A test that starts child processes while another thread of it chooses a port. A
//!   port is tried by binding a socket, and on macOS a socket is marked close-on-exec
//!   a moment after it is created, so a child started in that moment keeps the socket
//!   and the port. Two tests that take this file start child processes:
//!   `tests/throttle.rs`, whose three members are processes, and
//!   `crates/cavalry/tests/feed_held_still.rs`, whose six feeds are. Each is a program
//!   with no test harness that chooses all its ports on its one thread before it starts
//!   its first child; `feed_held_still` then starts a thread for each child, which reads
//!   the child's output and creates no socket. So no socket is being created when a
//!   child starts. A slot's lock is such a socket too, taken at a choice: at the first,
//!   and at the one that spends a slot. Both programs choose fewer ports than a slot
//!   holds, all before their first child, so they take their one lock first too.
//!   `crates/cavalry/tests/cli.rs` starts children from tests that run on several
//!   threads, and has a rule of its own for that reason: `loopback_ports`, from 10000
//!   to 19999, counted out and never tried.
//! - The bench harness, `bench/src/main.rs`, whose `free_port` binds port 0 and
//!   releases it. Its rows across two hosts name the port on the host's own address,
//!   which a loopback port chosen here says nothing about.
//!
//! A driver that bound port 0 itself and reported the port it was given would leave no
//! window at all; a channel whose port is 0 is refused in this version.
//!
//! `tests/ports_rule.rs` holds the rule to its numbers, in a small range of its own.

use std::net::UdpSocket;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};

/// The first port of the range.
pub const FIRST: u32 = 20_000;
/// The ports of one slot: its first is the slot's lock, the other 499 are a process's
/// to choose from.
pub const SLOT: u32 = 500;
/// The slots of the range, 24 of 500 ports: 20000 to 31999.
pub const SLOTS: u32 = 24;
/// The most slots one process holds: it takes the next only when it has tried every
/// port of the last.
pub const PER_PROCESS: usize = 4;

/// A socket bound to the loopback `port`, or `None` when the port is in use.
fn bind(port: u32) -> Option<UdpSocket> {
    let port = u16::try_from(port).ok()?;
    UdpSocket::bind(("127.0.0.1", port)).ok()
}

/// Takes one slot of a range of `slots` slots, each of `slot` ports, from the port
/// `first`: slot `start` when its first port can be bound, else the next that can,
/// each slot tried once. Returns the slot's first port and the socket bound to it,
/// which holds the slot for as long as it is kept, or `None` when every slot is taken.
pub fn take_slot(first: u32, slot: u32, slots: u32, start: u32) -> Option<(u32, UdpSocket)> {
    (0..slots)
        .map(|step| first + (start + step) % slots * slot)
        .find_map(|base| bind(base).map(|lock| (base, lock)))
}

/// The slots one process holds, at most `N`, each the first port of a slot and the
/// socket bound to it, taken one after another as the process chooses ports; and the
/// count of ports it has tried, across them all.
pub struct Held<const N: usize> {
    slots: [OnceLock<(u32, UdpSocket)>; N],
    next: AtomicU32,
}

impl<const N: usize> Held<N> {
    /// No slot held and no port tried.
    pub const fn new() -> Self {
        Self {
            slots: [const { OnceLock::new() }; N],
            next: AtomicU32::new(0),
        }
    }

    /// Chooses `count` ports from slots of `slot` ports, never a slot's first port,
    /// which is its lock. The ports are tried in order: those of the first slot held,
    /// then of the next, which `take` takes when the first port of it is tried, and so
    /// on to `N` slots. Each port is free when it is chosen and is never chosen again; a
    /// port in use is passed over and is not tried again either. The ports of one call
    /// are bound together and released together, so no two of them are the same.
    ///
    /// Panics when every port of the `N` slots has been tried, or when `take` finds no
    /// slot to take, and says what to do. A call that panics for want of a slot has
    /// spent the one try it counted, so a slot taken by a later call begins one port
    /// further in: one port of the bound, in a process that is failing already.
    pub fn choose(
        &self,
        slot: u32,
        count: usize,
        take: impl Fn() -> Option<(u32, UdpSocket)>,
    ) -> Vec<u16> {
        let ports = slot - 1;
        let mut sockets = Vec::with_capacity(count);
        while sockets.len() < count {
            let tried = self.next.fetch_add(1, Ordering::Relaxed);
            let index = usize::try_from(tried / ports).unwrap_or(usize::MAX);
            let Some(held) = self.slots.get(index) else {
                panic!(
                    "this test process has tried all {} ports of its {N} slots of {ports}: \
                     each was chosen before or was in use. Give a process more slots \
                     (PER_PROCESS in crates/cavalry-raft/tests/ports/mod.rs), or find the \
                     program that holds ports of its slots",
                    u32::try_from(N).unwrap_or(u32::MAX).saturating_mul(ports)
                )
            };
            let (base, _) = held.get_or_init(|| {
                take().unwrap_or_else(|| {
                    panic!(
                        "every slot of test ports is taken: end the test processes that \
                         hold them, which `lsof -nP -iUDP@127.0.0.1` lists, each with a \
                         socket on a slot's first port (crates/cavalry-raft/tests/ports/\
                         mod.rs)"
                    )
                })
            });
            if let Some(socket) = bind(base + tried % ports + 1) {
                sockets.push(socket);
            }
        }
        sockets
            .iter()
            .map(|socket| socket.local_addr().expect("bound").port())
            .collect()
    }
}

/// `count` loopback ports for a test's endpoints, each free when it is chosen, each
/// chosen once in this process, and all from slots that this process alone holds
/// (the module's documentation has the rule, its bound and what it does not cover).
///
/// The process takes its first slot on the first call, and another each time it has
/// tried every port of the last, up to [`PER_PROCESS`]. Its id picks the slot to try
/// first, so that two processes seldom try the same one; a slot this process holds
/// already is taken, and passed over like another process's.
pub fn free_ports(count: usize) -> Vec<u16> {
    static HELD: Held<PER_PROCESS> = Held::new();
    HELD.choose(SLOT, count, || {
        let taken = take_slot(FIRST, SLOT, SLOTS, std::process::id() % SLOTS);
        if taken.is_none() {
            panic!(
                "every one of the {SLOTS} slots of test ports is taken: a UDP socket is \
                 bound to 127.0.0.1 on each of {FIRST}, {}, and so on to {}. End the test \
                 processes that hold them, which `lsof -nP -iUDP@127.0.0.1` lists \
                 (crates/cavalry-raft/tests/ports/mod.rs)",
                FIRST + SLOT,
                FIRST + (SLOTS - 1) * SLOT
            );
        }
        taken
    })
}

/// One loopback port, as [`free_ports`] chooses it.
// The tests that run a cluster take their ports fifteen at a time and never call this.
#[allow(dead_code)]
pub fn free_port() -> u16 {
    free_ports(1)[0]
}
