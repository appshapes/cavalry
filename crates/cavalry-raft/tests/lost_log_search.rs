//! A search for a committed entry lost or replaced across lost log directories
//! (`spec/cluster.md`, "A member that lost its log"; ADR 0145; CLAUDE.md, invariant 1).
//! Three members in memory under a delivery seed of loss and delay, with members
//! halted and resumed, cut from each other one way or both, started again on the log
//! directory they kept, wiped, and stalled, each at a moment the seed draws, and the
//! check of `elected_cluster` after every step.
//!
//! A stall is a member's consensus module that stands still for up to 20 s while its
//! driver runs on (`Cluster::stall`; work item 467): the leader's half the time, so
//! that the append position its last duty cycle read falls behind what its recording
//! syncs and a replay reads (ADR 0163, Context).
//!
//! A wipe is drawn only while no other member is still short of its vote floor, and
//! while the two other members run and hold what was committed, so that no case ADR
//! 0145 accepts is reached: two members with no log while the third is stopped. Half
//! of the wipes made under another member's lead also hold what `spec/udp.md` lets
//! the transport hold for a receiver that went away: the leader's live sender, for up
//! to 5 s, and its next new leadership term to the wiped member, which reaches the
//! member up to 20 s later, once it has left init (work items 411 and 458).
//!
//! Each run of `just test` searches the seeds of [`SEEDS`]. A wider search names
//! its own:
//! `LOST_LOG_SEARCH_FROM`, `LOST_LOG_SEARCH_COUNT` and `LOST_LOG_SEARCH_MS`, for
//! example 3,000 seeds of 60 s each in a release build, as ADR 0145's Evidence ran. That
//! search found a member that voted for one candidate in a term, lost its log, and
//! voted for a second candidate of the same term, so that two members led one term;
//! `tests/vote_in_one_term.rs` holds the rule that ends it.
//!
//! The rules of ADRs 0161 and 0163 are not held here. With the clear at the join or
//! the floor answer taken out, or the answer read where the last duty cycle left it,
//! seeds 1 to 500 find nothing (work item 467): each case is several of these draws,
//! met in one order, on one member and one term. `tests/lost_log_directory.rs` holds
//! them step by step, with `tests/floor_answer.rs`.

mod elected_cluster;

use cavalry_raft::memory::Cluster;
use elected_cluster::Safety;

/// The members.
const IDS: [i32; 3] = [0, 1, 2];

/// The seeds `just test` runs: seed 1, and seeds 1765 and 1961, at which the search
/// finds two members leading one term with the term condition of ADR 0145's one case
/// in which a member that withholds votes taken out. Seeds 779, 1573 and 2983 did so
/// until the stalls and held frames of work item 467 moved the search's paths. Of
/// seeds 1 to 3,000, seed 804 fails as well, and with the condition in too: a member
/// that lost its log in a new cluster's first ballot, where every log is empty and it
/// does not withhold, votes again in the term it voted in (work item 476).
const SEEDS: [u64; 3] = [1, 1765, 1961];

/// A xorshift generator: the search is the seed's alone.
struct Draw(u64);

impl Draw {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// A value below `bound`.
    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }

    /// One of `items`.
    fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        let len = u64::try_from(items.len()).expect("a short list");
        let index = usize::try_from(self.below(len)).expect("an index of the list");
        items[index]
    }
}

fn setting(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

/// What one seed's run drew.
#[derive(Default)]
struct Drawn {
    wipes: u64,
    caught_up: u64,
    stalls: u64,
    leaders_stalled: u64,
    senders_held: u64,
    frames_held: u64,
}

/// Runs `seed` for `length_ms`, the check of every step failing the test where it
/// fails, and returns what it drew.
fn search(seed: u64, length_ms: u64) -> Drawn {
    let mut safety = Safety::new(&IDS);
    let mut cluster = Cluster::with_election(&IDS, seed).expect("three members");
    let mut draw = Draw(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let delay = draw.pick(&[0, 0, 20, 200, 2_000, 10_000]);
    let loss = draw.pick(&[0, 0, 200, 30]);
    cluster.set_delivery(loss, delay);
    let mut drawn = Drawn::default();
    let mut halted: Option<(i32, u64)> = None;
    let mut cut: Option<(i32, i32, u64)> = None;
    let mut wiped: Option<i32> = None;
    let mut stalled: Option<(i32, u64)> = None;
    // A leader's live sender that stands still, until when; and a new leadership term
    // held back for a member, released from when.
    let mut sender_held: Option<(i32, u64)> = None;
    let mut frame_held: Option<(i32, u64)> = None;
    let mut entry: u32 = 0;
    for ms in 0..length_ms {
        if draw.below(10) == 0 && cluster.offer(&entry.to_le_bytes()).is_ok() {
            entry += 1;
        }
        if let Some((member, until)) = halted
            && ms >= until
        {
            cluster.resume(member);
            halted = None;
        }
        if let Some((member, until)) = stalled
            && ms >= until
        {
            cluster.unstall(member);
            stalled = None;
        }
        if let Some((leader, until)) = sender_held
            && ms >= until
        {
            cluster.hold_sender(leader, None);
            sender_held = None;
        }
        // Released once the member has left init, as the receiver's new image takes the
        // frame (work item 458: the in-memory cluster reads a message in init).
        if let Some((member, from)) = frame_held
            && ms >= from
            && cluster.election_state(member) != Some(cavalry_raft::ElectionState::Init)
        {
            cluster.release_held_back(member);
            frame_held = None;
        }
        if let Some((a, b, until)) = cut
            && ms >= until
        {
            cluster.heal_between(a, b);
            cut = None;
        }
        if let Some(member) = wiped {
            let floor = cluster.member(member).expect("a member").vote_floor();
            if floor >= 0 && cluster.append_position(member) >= Some(floor) {
                wiped = None;
                drawn.caught_up += 1;
            }
        }
        let is_halted = |id: i32| halted.is_some_and(|(member, _)| member == id);
        let is_stalled = |id: i32| stalled.is_some_and(|(member, _)| member == id);
        match draw.below(3_000) {
            0 if halted.is_none() => {
                let member = draw.pick(&IDS);
                if wiped != Some(member) && !is_stalled(member) {
                    cluster.halt(member);
                    halted = Some((member, ms + 300 + draw.below(20_000)));
                }
            }
            1 if cut.is_none() => {
                let a = draw.pick(&IDS);
                let b = draw.pick(&[(a + 1) % 3, (a + 2) % 3]);
                let until = ms + 200 + draw.below(15_000);
                if draw.below(2) == 0 {
                    cluster.cut_one_way(a, b);
                } else {
                    cluster.cut_between(a, b);
                }
                cut = Some((a, b, until));
            }
            2 if wiped.is_none() => {
                let member = draw.pick(&IDS);
                let commit = IDS
                    .iter()
                    .filter_map(|&id| cluster.commit_position(id))
                    .max()
                    .unwrap_or(0);
                let others_hold = IDS
                    .iter()
                    .filter(|&&id| id != member)
                    .all(|&id| cluster.append_position(id) >= Some(commit) && !is_halted(id));
                if others_hold && !is_halted(member) {
                    // The member's receiver goes with its process. Half the time the
                    // transport does what `spec/udp.md` allows for a receiver that went
                    // away (ADR 0163, Context): the leader's live sender stands still,
                    // under the log stream's flow control, for up to 5 s, while its
                    // recording, which a replay reads, goes on; and the leader's next
                    // new leadership term to the member is held back on its consensus
                    // publication, to reach the member's next image, in this run or a
                    // later one, up to 20 s on (work item 411).
                    if let Some(leader) = cluster.leader()
                        && leader != member
                        && draw.below(2) == 0
                    {
                        if sender_held.is_none()
                            && let Some(at) = cluster.append_position(leader)
                        {
                            cluster.hold_sender(leader, Some(at));
                            sender_held = Some((leader, ms + 1 + draw.below(5_000)));
                            drawn.senders_held += 1;
                        }
                        if frame_held.is_none() {
                            cluster.hold_back_new_leadership_term(leader, member);
                            frame_held = Some((member, ms + 1 + draw.below(20_000)));
                            drawn.frames_held += 1;
                        }
                    }
                    safety.wipe(&mut cluster, member);
                    wiped = Some(member);
                    drawn.wipes += 1;
                    // A start ends a stall: the process that stood still is gone.
                    if is_stalled(member) {
                        stalled = None;
                    }
                }
            }
            3 => {
                let member = draw.pick(&IDS);
                if !is_halted(member) && wiped != Some(member) {
                    safety.restart(&mut cluster, member);
                    if is_stalled(member) {
                        stalled = None;
                    }
                }
            }
            // A module that stands still while its driver runs on: the leader's half
            // the time, so that it stalls with entries appended after its duty cycle
            // read its append position, as an offer above can leave them. Its driver's
            // recording syncs, its replays and its sender go on, and what is sent to it
            // waits, for 1 ms to 20 s: within a heartbeat interval, or long enough that
            // the others elect without it.
            4 if stalled.is_none() => {
                let member = match cluster.leader() {
                    Some(leader) if draw.below(2) == 0 => leader,
                    _ => draw.pick(&IDS),
                };
                if !is_halted(member) {
                    let span = draw.pick(&[2, 20, 200, 2_000, 20_000]);
                    if cluster.leader() == Some(member) {
                        drawn.leaders_stalled += 1;
                    }
                    cluster.stall(member);
                    stalled = Some((member, ms + 1 + draw.below(span)));
                    drawn.stalls += 1;
                }
            }
            _ => {}
        }
        safety.run(&mut cluster, 1);
    }
    drawn
}

/// The seeds of this run lose and replace nothing that was committed, and they wipe
/// members on the way: a search that wiped none would show nothing.
#[test]
fn no_committed_entry_is_lost_across_lost_log_directories() {
    let length_ms = setting("LOST_LOG_SEARCH_MS", 60_000);
    let seeds: Vec<u64> = match std::env::var("LOST_LOG_SEARCH_FROM") {
        Ok(_) => {
            let from = setting("LOST_LOG_SEARCH_FROM", 1);
            (from..from + setting("LOST_LOG_SEARCH_COUNT", 1)).collect()
        }
        Err(_) => SEEDS.to_vec(),
    };
    let mut all = Drawn::default();
    for &seed in &seeds {
        let drawn = search(seed, length_ms);
        all.wipes += drawn.wipes;
        all.caught_up += drawn.caught_up;
        all.stalls += drawn.stalls;
        all.leaders_stalled += drawn.leaders_stalled;
        all.senders_held += drawn.senders_held;
        all.frames_held += drawn.frames_held;
    }
    let Drawn {
        wipes,
        caught_up,
        stalls,
        leaders_stalled,
        senders_held,
        frames_held,
    } = all;
    assert!(wipes > 0, "seeds {seeds:?} wiped no member");
    assert!(
        stalls > 0 && leaders_stalled > 0,
        "seeds {seeds:?} stalled {stalls} modules, {leaders_stalled} of them a leader's"
    );
    assert!(
        senders_held > 0 && frames_held > 0,
        "seeds {seeds:?} held {senders_held} senders and {frames_held} frames at a wipe"
    );
    assert!(
        caught_up > 0,
        "no wiped member of seeds {seeds:?} reached its floor"
    );
}
