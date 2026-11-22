//! A search for a committed entry lost or replaced across lost log directories
//! (`spec/cluster.md`, "A member that lost its log"; ADR 0145; CLAUDE.md, invariant 1).
//! Three members in memory under a delivery seed of loss and delay, with members
//! halted and resumed, cut from each other one way or both, started again on the log
//! directory they kept, and wiped, each at a moment the seed draws, and the check of
//! `elected_cluster` after every step.
//!
//! A wipe is drawn only while no other member is still short of its vote floor, and
//! while the two other members run and hold what was committed, so that no case ADR
//! 0145 accepts is reached: two members with no log while the third is stopped. The
//! frame that flow control holds back (work item 411) has no place in this model.
//!
//! Each run of `just test` searches the four seeds of [`SEEDS`]. A wider search names
//! its own:
//! `LOST_LOG_SEARCH_FROM`, `LOST_LOG_SEARCH_COUNT` and `LOST_LOG_SEARCH_MS`, for
//! example 3,000 seeds of 60 s each in a release build, as ADR 0145's Evidence ran. That
//! search found a member that voted for one candidate in a term, lost its log, and
//! voted for a second candidate of the same term, so that two members led one term;
//! `tests/vote_in_one_term.rs` holds the rule that ends it.

mod elected_cluster;

use cavalry_raft::memory::Cluster;
use elected_cluster::Safety;

/// The members.
const IDS: [i32; 3] = [0, 1, 2];

/// The seeds `just test` runs: seed 1, and three at which the search first found two
/// members leading one term, which fail with the term condition of ADR 0145's one case
/// in which a member that withholds votes taken out.
const SEEDS: [u64; 4] = [1, 779, 1573, 2983];

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
        match draw.below(3_000) {
            0 if halted.is_none() => {
                let member = draw.pick(&IDS);
                if wiped != Some(member) {
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
                    safety.wipe(&mut cluster, member);
                    wiped = Some(member);
                    drawn.wipes += 1;
                }
            }
            3 => {
                let member = draw.pick(&IDS);
                if !is_halted(member) && wiped != Some(member) {
                    safety.restart(&mut cluster, member);
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
    let (mut wipes, mut caught_up) = (0, 0);
    for &seed in &seeds {
        let drawn = search(seed, length_ms);
        wipes += drawn.wipes;
        caught_up += drawn.caught_up;
    }
    assert!(wipes > 0, "seeds {seeds:?} wiped no member");
    assert!(
        caught_up > 0,
        "no wiped member of seeds {seeds:?} reached its floor"
    );
}
