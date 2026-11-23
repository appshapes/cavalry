//! The rule of `tests/ports/mod.rs`, held to its numbers. One test: it first checks the
//! slot this process holds, then tries the bounds, every slot taken, a process that
//! spends one slot and takes the next, and a process that spends all it may hold, in
//! a small range of its own, the last forty ports of this process's slot. No other
//! process chooses from those, and this process's own count does not reach them, so
//! the test takes no slot from another test process and spends none.

mod ports;

use std::io::ErrorKind;
use std::net::UdpSocket;

use ports::{FIRST, Held, SLOT, SLOTS, free_port, free_ports, take_slot};

fn port(number: u32) -> u16 {
    u16::try_from(number).expect("below 32000")
}

/// A process's first ports are all of the one slot it holds, never the slot's first
/// port, and each is chosen once. A slot that is taken is passed over, the slots are
/// tried round from the first tried, and with every slot taken there is none to take.
/// A port in use is passed over. A process that has tried every port of its slot takes
/// the next free slot, passing over the one it holds; one that has tried every port of
/// all the slots it may hold panics and names the file to change, each time; and one
/// that finds no slot to take panics and says so (invariant 3: the bounds and what
/// happens at them; work item 422).
#[test]
fn a_process_chooses_from_one_slot_it_holds_and_both_bounds_say_when_they_are_met() {
    // The slot this process holds.
    let mut chosen = free_ports(15);
    chosen.extend(free_ports(15));
    chosen.push(free_port());
    let base = FIRST + (u32::from(chosen[0]) - FIRST) / SLOT * SLOT;
    assert!(base < FIRST + SLOTS * SLOT, "slot {base} is of the range");
    let held = UdpSocket::bind(("127.0.0.1", port(base)))
        .expect_err("this process holds its slot's first port");
    assert_eq!(held.kind(), ErrorKind::AddrInUse);
    for port in &chosen {
        let port = u32::from(*port);
        assert!(
            base < port && port < base + SLOT,
            "port {port} is of the slot at {base} and is not its lock"
        );
    }
    chosen.sort_unstable();
    chosen.dedup();
    assert_eq!(chosen.len(), 31, "each port is chosen once in a process");

    // Four slots of ten ports, in this test's own range. Slot 2 is tried first.
    let range = base + SLOT - 40;
    let take = || take_slot(range, 10, 4, 2);
    let first = take().expect("slot 2 is free");
    assert_eq!(first.0, range + 20);
    let second = take().expect("slot 2 is taken and passed over");
    assert_eq!(second.0, range + 30);
    let third = take().expect("the slots are tried round");
    assert_eq!(third.0, range);
    let fourth = take().expect("the last free slot");
    assert_eq!(fourth.0, range + 10);
    assert!(take().is_none(), "every slot is taken");
    // A slot is free again when the socket that held it closes, as a process's does
    // when the process ends.
    drop(second);
    assert_eq!(take().expect("slot 3 is free again").0, range + 30);

    // A process that holds at most two slots of ten. Its first is slot 2, its first
    // port chosen: three of the nine ports, then one in use passed over and one chosen,
    // then the other four.
    drop((first, third, fourth));
    let held = Held::<2>::new();
    let slot = range + 20;
    assert_eq!(
        held.choose(10, 3, take),
        [port(slot + 1), port(slot + 2), port(slot + 3)]
    );
    let _in_use = UdpSocket::bind(("127.0.0.1", port(slot + 4))).expect("a free port");
    assert_eq!(held.choose(10, 1, take), [port(slot + 5)]);
    assert_eq!(
        held.choose(10, 4, take),
        [
            port(slot + 6),
            port(slot + 7),
            port(slot + 8),
            port(slot + 9)
        ]
    );
    // Slot 2 is spent: the next port is of the next free slot, slot 3, which is taken
    // past slot 2, held by this process now.
    assert_eq!(held.choose(10, 1, take), [port(range + 31)]);
    for lock in [slot, range + 30] {
        let taken = UdpSocket::bind(("127.0.0.1", port(lock))).expect_err("a slot held");
        assert_eq!(taken.kind(), ErrorKind::AddrInUse);
    }
    assert_eq!(held.choose(10, 8, take).len(), 8, "the rest of slot 3");
    // Both slots spent: no third is taken, and the panic names the file to change.
    for _ in 0..2 {
        let spent =
            std::panic::catch_unwind(|| held.choose(10, 1, take)).expect_err("two spent slots");
        let message = spent.downcast_ref::<String>().expect("a message");
        assert!(
            message.contains("tried all 18 ports of its 2 slots of 9")
                && message.contains("PER_PROCESS in crates/cavalry-raft/tests/ports/mod.rs"),
            "{message}"
        );
    }
    // No slot to take at all: the panic says so.
    let none = Held::<1>::new();
    let found = std::panic::catch_unwind(|| none.choose(10, 1, || None)).expect_err("no slot");
    let message = found.downcast_ref::<String>().expect("a message");
    assert!(
        message.contains("every slot of test ports is taken"),
        "{message}"
    );
}
