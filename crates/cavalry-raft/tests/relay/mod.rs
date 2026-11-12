//! A relay on the loopback between the members of a cluster under test (`CLAUDE.md`,
//! gate 3, the fault-injection gate; ADR 0077): every datagram one member sends
//! another goes through it, so a test can lose, hold back and cut what a real network
//! loses, reorders and partitions, with real drivers on both sides.
//!
//! # How a member comes to send through it
//!
//! A member sends to the endpoints its members string gives its peers
//! (`spec/cluster.md`, "Members and streams"): a peer's `consensus` endpoint, a
//! follower's `log` endpoint as a destination of the leader's log, and a follower's
//! `catchup` endpoint for a replay. The relay binds a port of its own for each of
//! those, for each pair of members and each direction, a link, and gives each member a
//! members string of its own: the member's five endpoints as they are, and each
//! peer's three as the ports of the links to that peer. What a link receives it sends
//! on to the peer's own endpoint from a second socket, and what comes back to that
//! socket, the receiver's status messages and NAKs, it sends back to the sender. The
//! page says a cluster is one string on every member. Here each member's string names
//! the same members at the same ids, and differs only in the road to them, which only
//! a test does. `ingress` and `archive` are left as they are: no member sends to them.
//!
//! # What a test can do to a link
//!
//! - [`Faults`], for the life of the relay: of the data frames a link carries, every
//!   `drop_every`th is lost and every `hold_every`th is held back until the next one
//!   has gone, or two milliseconds have, so that frames arrive out of order. Setup
//!   frames, heartbeats, status messages and NAKs pass untouched, as in the shim of
//!   `cavalry-client`'s `udp_receive` test, so that what a fault costs is mended by a
//!   NAK and a retransmission.
//! - [`Relay::cut`] and [`Relay::mend`]: while a member is cut, every datagram to it
//!   and from it is lost, in both directions, on every link. That is a partition
//!   with the member alone on its side.
//!
//! One thread serves every link, its sockets non-blocking.

use std::net::{SocketAddr, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use cavalry_proto::frame::FrameType;

/// The endpoints of a members string a peer sends to, by their place among a
/// member's five: `consensus`, `log` and `catchup`.
const RELAYED: [usize; 3] = [1, 2, 3];

/// How long a data frame is held back when no other follows it.
const HOLD: Duration = Duration::from_millis(2);

/// How many datagrams a socket gives up in one pass, so that no link starves another.
const BATCH: usize = 64;

/// What the relay does to the data frames each link carries, by their count on that
/// link. Zero turns a fault off.
#[derive(Clone, Copy)]
pub struct Faults {
    /// Every `drop_every`th data frame is lost.
    pub drop_every: usize,
    /// Every `hold_every`th data frame is held back until the next has gone.
    pub hold_every: usize,
}

/// What the relay did, over every link.
#[derive(Debug, Default, Clone, Copy)]
pub struct Totals {
    /// Datagrams sent on, in either direction.
    pub forwarded: usize,
    /// Data frames lost by [`Faults::drop_every`].
    pub dropped: usize,
    /// Data frames held back by [`Faults::hold_every`].
    pub held: usize,
    /// Datagrams lost because a member was cut.
    pub cut: usize,
}

#[derive(Default)]
struct Shared {
    stop: AtomicBool,
    cut: [AtomicBool; 8],
    forwarded: AtomicUsize,
    dropped: AtomicUsize,
    held: AtomicUsize,
    cut_count: AtomicUsize,
}

/// One direction between two members on one endpoint.
struct Link {
    /// The member that sends on this link, and the member that receives.
    from: usize,
    to: usize,
    /// The port the sender's members string names; the sender's datagrams arrive here.
    facing_sender: UdpSocket,
    /// The source of what the receiver is sent; its answers arrive here.
    facing_receiver: UdpSocket,
    /// The receiver's own endpoint.
    endpoint: SocketAddr,
    /// Where the sender sends from, learned from its last datagram.
    sender: Option<SocketAddr>,
    /// The data frames this link has carried.
    data: usize,
    /// The data frame held back, and when it was.
    holding: Option<(Vec<u8>, Instant)>,
}

/// The relay: see the module's doc.
pub struct Relay {
    strings: Vec<String>,
    shared: Arc<Shared>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Relay {
    /// Starts a relay between the members whose ports are `ports`, five for each
    /// member in the order of the members string, the members' ids being their places.
    pub fn start(ports: &[u16], faults: Faults) -> Self {
        let members = ports.len() / 5;
        assert!(members <= 8 && ports.len() == members * 5);
        let own = |member: usize, endpoint: usize| {
            SocketAddr::from(([127, 0, 0, 1], ports[member * 5 + endpoint]))
        };
        let mut links = Vec::with_capacity(members * (members - 1) * RELAYED.len());
        let mut strings = Vec::with_capacity(members);
        for from in 0..members {
            let mut string = String::new();
            for to in 0..members {
                let mut endpoints: Vec<String> = (0..5)
                    .map(|endpoint| own(to, endpoint).to_string())
                    .collect();
                if to != from {
                    for endpoint in RELAYED {
                        let facing_sender = UdpSocket::bind("127.0.0.1:0").expect("a socket");
                        let facing_receiver = UdpSocket::bind("127.0.0.1:0").expect("a socket");
                        for socket in [&facing_sender, &facing_receiver] {
                            socket.set_nonblocking(true).expect("a non-blocking socket");
                        }
                        endpoints[endpoint] =
                            facing_sender.local_addr().expect("bound").to_string();
                        links.push(Link {
                            from,
                            to,
                            facing_sender,
                            facing_receiver,
                            endpoint: own(to, endpoint),
                            sender: None,
                            data: 0,
                            holding: None,
                        });
                    }
                }
                if to > 0 {
                    string.push('|');
                }
                string.push_str(&format!("{to},{}", endpoints.join(",")));
            }
            strings.push(string);
        }
        let shared = Arc::new(Shared::default());
        let thread = {
            let shared = Arc::clone(&shared);
            std::thread::spawn(move || serve(&mut links, faults, &shared))
        };
        Self {
            strings,
            shared,
            thread: Some(thread),
        }
    }

    /// The members string of the member whose id is `member`.
    pub fn members_string(&self, member: i32) -> &str {
        &self.strings[usize::try_from(member).expect("a member id")]
    }

    /// Cuts the member off: every datagram to it and from it is lost until
    /// [`Relay::mend`].
    pub fn cut(&self, member: i32) {
        self.shared.cut[usize::try_from(member).expect("a member id")]
            .store(true, Ordering::Release);
    }

    /// Ends a [`Relay::cut`].
    pub fn mend(&self, member: i32) {
        self.shared.cut[usize::try_from(member).expect("a member id")]
            .store(false, Ordering::Release);
    }

    /// What the relay has done so far.
    pub fn totals(&self) -> Totals {
        Totals {
            forwarded: self.shared.forwarded.load(Ordering::Acquire),
            dropped: self.shared.dropped.load(Ordering::Acquire),
            held: self.shared.held.load(Ordering::Acquire),
            cut: self.shared.cut_count.load(Ordering::Acquire),
        }
    }
}

impl Drop for Relay {
    /// Stops the relay's thread.
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// True for a datagram that carries a data frame or a padding frame with a length: a
/// frame of the stream, and not a heartbeat, a setup or a control frame
/// (`spec/udp.md`, "The datagram").
fn is_data(datagram: &[u8]) -> bool {
    datagram.len() >= 8
        && i32::from_le_bytes([datagram[0], datagram[1], datagram[2], datagram[3]]) != 0
        && matches!(
            u16::from_le_bytes([datagram[6], datagram[7]]),
            t if t == FrameType::Data as u16 || t == FrameType::Pad as u16
        )
}

/// The relay's thread: each pass takes what every socket holds, up to [`BATCH`], and
/// sleeps a tenth of a millisecond when no socket held anything.
fn serve(links: &mut [Link], faults: Faults, shared: &Shared) {
    let mut buffer = vec![0u8; 65536];
    while !shared.stop.load(Ordering::Acquire) {
        let mut moved = 0;
        for link in links.iter_mut() {
            let cut = shared.cut[link.from].load(Ordering::Acquire)
                || shared.cut[link.to].load(Ordering::Acquire);
            // The sender's datagrams, on to the receiver's endpoint.
            for _ in 0..BATCH {
                let Ok((length, from)) = link.facing_sender.recv_from(&mut buffer) else {
                    break;
                };
                moved += 1;
                link.sender = Some(from);
                let datagram = &buffer[..length];
                if cut {
                    shared.cut_count.fetch_add(1, Ordering::AcqRel);
                    continue;
                }
                if !is_data(datagram) {
                    let _ = link.facing_receiver.send_to(datagram, link.endpoint);
                    shared.forwarded.fetch_add(1, Ordering::AcqRel);
                    continue;
                }
                link.data += 1;
                if faults.drop_every > 0 && link.data.is_multiple_of(faults.drop_every) {
                    shared.dropped.fetch_add(1, Ordering::AcqRel);
                    continue;
                }
                if faults.hold_every > 0
                    && link.data.is_multiple_of(faults.hold_every)
                    && link.holding.is_none()
                {
                    link.holding = Some((datagram.to_vec(), Instant::now()));
                    shared.held.fetch_add(1, Ordering::AcqRel);
                    continue;
                }
                let _ = link.facing_receiver.send_to(datagram, link.endpoint);
                shared.forwarded.fetch_add(1, Ordering::AcqRel);
                if let Some((late, _)) = link.holding.take() {
                    let _ = link.facing_receiver.send_to(&late, link.endpoint);
                    shared.forwarded.fetch_add(1, Ordering::AcqRel);
                }
            }
            // A frame held back with none after it goes once its time is up, and is
            // lost with the rest while a member is cut.
            if let Some((_, since)) = &link.holding
                && (cut || since.elapsed() >= HOLD)
                && let Some((late, _)) = link.holding.take()
            {
                if cut {
                    shared.cut_count.fetch_add(1, Ordering::AcqRel);
                } else {
                    let _ = link.facing_receiver.send_to(&late, link.endpoint);
                    shared.forwarded.fetch_add(1, Ordering::AcqRel);
                }
            }
            // The receiver's answers, back to where the sender sends from.
            for _ in 0..BATCH {
                let Ok((length, _)) = link.facing_receiver.recv_from(&mut buffer) else {
                    break;
                };
                moved += 1;
                if cut {
                    shared.cut_count.fetch_add(1, Ordering::AcqRel);
                    continue;
                }
                if let Some(sender) = link.sender {
                    let _ = link.facing_sender.send_to(&buffer[..length], sender);
                    shared.forwarded.fetch_add(1, Ordering::AcqRel);
                }
            }
        }
        if moved == 0 {
            std::thread::sleep(Duration::from_micros(100));
        }
    }
}
