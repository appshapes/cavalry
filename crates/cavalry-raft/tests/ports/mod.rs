//! Loopback ports for the endpoints of a cluster in a test. One function, in a module
//! of its own so that its rule is written once. This crate's tests under `tests/` take
//! it with `mod ports;`. Two more take this same file by path and are what breaks, by
//! name, if the file moves: the module's own tests, `src/module/tests.rs`, with
//! `#[path = "../../tests/ports/mod.rs"] mod ports;`, and the tests of `cavalry-ffi`
//! that run cluster members, with `#[path = "../../cavalry-raft/tests/ports/mod.rs"]`.

use std::net::UdpSocket;
use std::sync::atomic::{AtomicU32, Ordering};

/// `count` loopback ports for a cluster's endpoints, from 20000 to 31999, each free
/// when it is chosen.
///
/// The range is below the one the operating system gives a socket bound to port 0,
/// which starts at 32768 on Linux and at 49152 on macOS. A member does not bind all
/// its endpoints when it starts: a follower binds its `log` endpoint only when it
/// joins the log, and again when it leaves the log and joins it again (ADR 0068).
/// While that endpoint is not bound, a port from the system's range can be given to
/// any socket bound to port 0: a send socket of a driver in the process, which every
/// publication has, or a socket of another test. So a port that was chosen by binding
/// port 0 and releasing it can be taken when the member comes to bind it: the bind
/// fails with `Address already in use`, error code 4, and the member's duty cycle
/// fails, or it never joins.
///
/// Each port is chosen once in a process, so that tests that run at once in one test
/// binary never share one, and the process id picks where the choice starts, so that
/// two test processes on one host start apart.
pub fn free_ports(count: usize) -> Vec<u16> {
    const FIRST: u32 = 20_000;
    const SPAN: u32 = 12_000;
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let start = (std::process::id() % 60) * 200;
    let mut sockets = Vec::with_capacity(count);
    for _ in 0..SPAN {
        if sockets.len() == count {
            break;
        }
        let offset = (start + NEXT.fetch_add(1, Ordering::Relaxed)) % SPAN;
        let port = u16::try_from(FIRST + offset).expect("below 32000");
        if let Ok(socket) = UdpSocket::bind(("127.0.0.1", port)) {
            sockets.push(socket);
        }
    }
    assert_eq!(
        sockets.len(),
        count,
        "free loopback ports from {FIRST} to {}",
        FIRST + SPAN - 1
    );
    sockets
        .iter()
        .map(|socket| socket.local_addr().expect("bound").port())
        .collect()
}
