//! The room `tests/ports/mod.rs` gives one test process, held to the use it is for:
//! more than twice the pairs of drivers that this crate's library tests start, the
//! largest user of the rule, which chose 475 ports in one process on 2026-11-23, most
//! of them ten for each pair (work item 422). A test program of its own, so that its ports are
//! the only ones its process chooses, and so that it takes nothing from the range that
//! `tests/ports_rule.rs` tries its bounds in.

mod ports;

use std::io::ErrorKind;
use std::net::UdpSocket;

use ports::{FIRST, PER_PROCESS, SLOT, free_ports};

/// The ports of a hundred pairs of drivers, ten at a time as a pair takes them: a
/// thousand ports, each chosen once, none a slot's first port, from three slots that
/// this process holds while it runs; and room for more, since a process may hold
/// [`PER_PROCESS`] slots.
#[test]
fn a_process_chooses_the_ports_of_a_hundred_pairs_of_drivers() {
    let mut chosen: Vec<u16> = (0..100).flat_map(|_| free_ports(10)).collect();
    let base = |port: u16| FIRST + (u32::from(port) - FIRST) / SLOT * SLOT;
    let mut slots: Vec<u32> = chosen.iter().map(|port| base(*port)).collect();
    slots.sort_unstable();
    slots.dedup();
    assert!(
        slots.len() >= 3 && slots.len() <= PER_PROCESS,
        "a thousand ports from at least three slots and no more than a process holds: \
         {slots:?}"
    );
    for slot in &slots {
        let lock = u16::try_from(*slot).expect("below 32000");
        assert!(
            !chosen.contains(&lock),
            "slot {slot}'s lock is never chosen"
        );
        let held = UdpSocket::bind(("127.0.0.1", lock)).expect_err("a slot this process holds");
        assert_eq!(held.kind(), ErrorKind::AddrInUse);
    }
    chosen.sort_unstable();
    chosen.dedup();
    assert_eq!(chosen.len(), 1_000, "each port is chosen once in a process");
}
