//! Loopback ports for a test whose driver binds a port that the test chose: the
//! endpoints of a cluster, the endpoint of a `cavalry:udp` subscription, a control
//! channel. In a module of its own so that its rule is written once. This crate's
//! tests under `tests/` take it with `mod ports;`. The others take this same file by
//! path and are what breaks, by name, if the file moves: the module's own tests,
//! `src/module/tests.rs`, with `#[path = "../../tests/ports/mod.rs"] mod ports;`, and
//! these, each with `#[path = "../../cavalry-raft/tests/ports/mod.rs"]`: the tests of
//! `cavalry-ffi` that run cluster members, the conductor's unit tests in
//! `cavalry-driver/src/conductor.rs`, and six of `cavalry-client`'s tests,
//! `armed_recording`, `destinations`, `replay`, `udp_receive`,
//! `udp_receive_allocations` and `udp_resubscribe`.
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
//!   range is cut into [`SLOTS`] slots of [`SLOT`] ports. A process takes one slot by
//!   binding the slot's first port, keeps that socket for as long as it runs, and
//!   chooses its ports from that slot alone. The operating system closes the socket
//!   when the process ends, however it ends, so a slot is never left taken.
//!
//! # What the rule does not cover
//!
//! - A process that is not one of these tests and binds a port from 20000 to 31999 by
//!   its number. A port in use when it is chosen is passed over, so only a bind
//!   between the choice and the driver's own is a failure.
//! - A host whose range for port 0 was changed to reach below 32000:
//!   `net.ipv4.ip_local_port_range` on Linux, `net.inet.ip.portrange.first` on macOS.
//! - A test that starts child processes. A port is tried by binding a socket, and on
//!   macOS a socket is marked close-on-exec a moment after it is created, so a child
//!   started in that moment keeps the socket and the port. No test that takes this
//!   file starts one. `crates/cavalry/tests/cli.rs` does, and has a rule of its own
//!   for that reason: `loopback_ports`, from 10000 to 19999, counted out and never
//!   tried.
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

/// Chooses `count` ports from the slot of `slot` ports whose first port is `base`,
/// counting on from `next`, which starts at 1 because the first port is the slot's
/// lock. Each port is free when it is chosen and is never chosen again; a port in use
/// is passed over and is not tried again either. The ports of one call are bound
/// together and released together, so no two of them are the same.
///
/// Panics when the slot has no port left to try, and says what to do.
pub fn choose(base: u32, slot: u32, next: &AtomicU32, count: usize) -> Vec<u16> {
    let mut sockets = Vec::with_capacity(count);
    while sockets.len() < count {
        let offset = next.fetch_add(1, Ordering::Relaxed);
        assert!(
            offset < slot,
            "this test process has tried all {} ports of its slot, {} to {}: each was \
             chosen before or was in use. Give a slot more ports and the range fewer \
             slots (SLOT and SLOTS in crates/cavalry-raft/tests/ports/mod.rs), or find \
             the program that holds ports of this slot",
            slot - 1,
            base + 1,
            base + slot - 1
        );
        if let Some(socket) = bind(base + offset) {
            sockets.push(socket);
        }
    }
    sockets
        .iter()
        .map(|socket| socket.local_addr().expect("bound").port())
        .collect()
}

/// `count` loopback ports for a test's endpoints, each free when it is chosen, each
/// chosen once in this process, and all from the slot that this process alone holds
/// (the module's documentation has the rule and what it does not cover).
///
/// The process takes its slot on the first call. Its id picks the slot to try first,
/// so that two processes seldom try the same one. A process that finds every slot
/// taken, or that has tried all 499 ports of its slot, panics and says what to do.
pub fn free_ports(count: usize) -> Vec<u16> {
    static HELD: OnceLock<(u32, UdpSocket)> = OnceLock::new();
    static NEXT: AtomicU32 = AtomicU32::new(1);
    let (base, _) = HELD.get_or_init(|| {
        take_slot(FIRST, SLOT, SLOTS, std::process::id() % SLOTS).unwrap_or_else(|| {
            panic!(
                "every one of the {SLOTS} slots of test ports is taken: a UDP socket is \
                 bound to 127.0.0.1 on each of {FIRST}, {}, and so on to {}. End the \
                 test processes that hold them (crates/cavalry-raft/tests/ports/mod.rs)",
                FIRST + SLOT,
                FIRST + (SLOTS - 1) * SLOT
            )
        })
    });
    choose(*base, SLOT, &NEXT, count)
}

/// One loopback port, as [`free_ports`] chooses it.
// The tests that run a cluster take their ports fifteen at a time and never call this.
#[allow(dead_code)]
pub fn free_port() -> u16 {
    free_ports(1)[0]
}
