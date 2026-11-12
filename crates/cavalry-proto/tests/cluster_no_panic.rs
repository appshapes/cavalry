//! Every byte string, on every stream, either decodes to a message that encodes to the
//! same bytes and decodes again to the same message, or is rejected with one of the
//! spec's twelve reasons; every members string either parses or is refused with one of
//! its four; and every arithmetic function computes from extreme inputs, the
//! position after an entry `None` where it does not fit. Never a panic (invariant 2;
//! CLAUDE.md gate 1, "frame parsers are fuzzed"; ADR 0058). This runs on every
//! `just test`, in a debug build, where an overflow would panic.

use cavalry_proto::cluster::{
    CLOSED_STATE, CanvassTerms, EMPTY_MEMBER, LogReport, MAX_MEMBERS, MemberPosition, Message,
    Stream, TermFields, TermSpan, Vote, VoteRequest, answered_term, appointed_canvass,
    appointed_leader, ballot, candidate, canvass_position_action, commit_position, entry_position,
    leader_ready, log_at_least, new_leadership_term_action, next_fields, parse_members, quorum,
    replay_end, replication_record, replication_stop, request_vote,
};

/// Deterministic, so a failure reproduces: xorshift64.
struct XorShift(u64);

impl XorShift {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

const REASONS: [&str; 12] = [
    "too-short",
    "schema",
    "version",
    "template",
    "stream",
    "block-length",
    "length",
    "enum",
    "ascii",
    "member-id",
    "position",
    "term",
];
const MEMBERS_REASONS: [&str; 4] = [
    "members-grammar",
    "members-duplicate",
    "members-count",
    "members-self",
];
const STREAMS: [Stream; 5] = [
    Stream::Log,
    Stream::Consensus,
    Stream::Ingress,
    Stream::Egress,
    Stream::Snapshot,
];

fn check(bytes: &[u8]) {
    for stream in STREAMS {
        match Message::parse(stream, bytes) {
            Ok(message) => {
                let mut buffer = vec![0u8; bytes.len()];
                let length = message
                    .write(&mut buffer)
                    .expect("a decoded message encodes");
                assert_eq!(length, bytes.len(), "{bytes:02x?}");
                assert_eq!(
                    buffer, bytes,
                    "{bytes:02x?}: the message encodes to its bytes"
                );
                let again = Message::parse(stream, &buffer).expect("encoded bytes decode");
                assert_eq!(again, message, "{bytes:02x?}");
            }
            Err(error) => assert!(REASONS.contains(&error.reason()), "{bytes:02x?}: {error}"),
        }
    }
}

/// The valid vectors' bytes, the seeds every mutation starts from.
fn valid_messages() -> Vec<Vec<u8>> {
    let doc: serde_json::Value =
        serde_json::from_str(include_str!("../../../spec/vectors/cluster.json")).expect("vectors");
    doc["vectors"]
        .as_array()
        .expect("vectors")
        .iter()
        .filter(|v| v["kind"] == "message" && v.get("error").is_none())
        .map(|v| {
            let text = v["bytes"].as_str().expect("bytes");
            (0..text.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("hex"))
                .collect()
        })
        .collect()
}

#[test]
fn no_byte_string_panics_the_message_parser() {
    let mut random = XorShift(0x2545_F491_4F6C_DD1D);
    let seeds = valid_messages();
    assert!(seeds.len() >= 30);
    for seed in &seeds {
        check(seed);
        for cut in 0..seed.len() {
            check(&seed[..cut]);
        }
        for _ in 0..400 {
            let mut bytes = seed.clone();
            for _ in 0..1 + random.next() % 3 {
                let at = (random.next() as usize) % bytes.len();
                bytes[at] = random.next() as u8;
            }
            check(&bytes);
            bytes.extend_from_slice(&random.next().to_le_bytes()[..(random.next() % 8) as usize]);
            check(&bytes);
        }
    }
    for _ in 0..20_000 {
        let length = (random.next() % 160) as usize;
        let bytes: Vec<u8> = (0..length).map(|_| random.next() as u8).collect();
        check(&bytes);
    }
}

#[test]
fn no_byte_string_panics_the_members_parser() {
    let mut random = XorShift(0x9E37_79B9_7F4A_7C15);
    let alphabet = b"0123456789,|:.[]abh-";
    for _ in 0..20_000 {
        let mut members = [EMPTY_MEMBER; MAX_MEMBERS];
        let length = (random.next() % 120) as usize;
        let text: Vec<u8> = (0..length)
            .map(|_| alphabet[(random.next() as usize) % alphabet.len()])
            .collect();
        let self_id = if random.next().is_multiple_of(2) {
            None
        } else {
            Some((random.next() % 10) as i32)
        };
        match parse_members(&text, &mut members, self_id) {
            Ok(count) => assert!((1..=MAX_MEMBERS).contains(&count), "{text:?}"),
            Err(error) => assert!(
                MEMBERS_REASONS.contains(&error.reason()),
                "{text:?}: {error}"
            ),
        }
    }
    for _ in 0..2_000 {
        let bytes: Vec<u8> = (0..64).map(|_| random.next() as u8).collect();
        let mut members = [EMPTY_MEMBER; MAX_MEMBERS];
        let _ = parse_members(&bytes, &mut members, None);
    }
}

/// Positions, terms and lengths at the edges of their types, where an unchecked
/// arithmetic step would overflow.
const EXTREMES: [i64; 9] = [
    i64::MIN,
    i64::MIN + 1,
    -2,
    -1,
    0,
    1,
    2,
    i64::MAX - 1,
    i64::MAX,
];
const LENGTHS: [usize; 7] = [0, 1, 31, 32, 1408, usize::MAX - 1, usize::MAX];
const MTUS: [usize; 9] = [0, 1, 32, 33, 64, 1408, 65504, usize::MAX - 1, usize::MAX];

#[test]
fn no_extreme_input_panics_the_entry_position() {
    for position in EXTREMES {
        for length in LENGTHS {
            for mtu in MTUS {
                let after = entry_position(position, length, mtu);
                if mtu <= 32 {
                    assert_eq!(after, None, "an mtu of {mtu} leaves a fragment no payload");
                }
                if let Some(after) = after {
                    // Every entry is at least one 32-byte frame.
                    assert!(after.fragments >= 1);
                    assert!(
                        after.position_after >= position.saturating_add(32),
                        "{position} {length} {mtu}: {after:?}"
                    );
                }
            }
        }
    }
    assert_eq!(entry_position(i64::MAX, 1, 1408), None, "past i64::MAX");
    assert_eq!(
        entry_position(i64::MAX - 64, 1, 1408).map(|after| after.position_after),
        Some(i64::MAX),
        "one 64-byte frame that ends at i64::MAX"
    );
    assert_eq!(
        entry_position(0, usize::MAX, 1408),
        None,
        "more bytes than an i64"
    );
}

impl XorShift {
    /// One of [`EXTREMES`].
    fn extreme(&mut self) -> i64 {
        EXTREMES[(self.next() % EXTREMES.len() as u64) as usize]
    }

    /// A count below `bound`.
    fn below(&mut self, bound: u64) -> usize {
        (self.next() % bound) as usize
    }
}

#[test]
fn no_extreme_input_panics_the_election_arithmetic() {
    let mut random = XorShift(0xD1B5_4A32_D192_ED03);
    for members in [0, 1, 2, 3, MAX_MEMBERS, MAX_MEMBERS + 1, usize::MAX] {
        assert!(quorum(members) >= 1);
    }
    for _ in 0..20_000 {
        let r = &mut random;
        // Up to 10 reports, more than the 8 the ranking holds.
        let positions: Vec<MemberPosition> = (0..r.below(11))
            .map(|_| MemberPosition {
                position: r.extreme(),
                age_ns: r.extreme().unsigned_abs(),
                term: r.extreme(),
            })
            .collect();
        let leader_position = r.extreme();
        let commit = commit_position(
            &positions,
            leader_position,
            r.extreme(),
            r.extreme(),
            r.extreme().unsigned_abs(),
        );
        assert!(commit.commit_position <= leader_position);
        assert!(
            commit.quorum_position == 0
                || positions
                    .iter()
                    .any(|p| p.position == commit.quorum_position)
        );

        let own = LogReport {
            term: r.extreme(),
            position: r.extreme(),
        };
        assert!(log_at_least(own.term, own.position, own.term, own.position));
        let _ = log_at_least(own.term, own.position, r.extreme(), r.extreme());

        let members = [0, 1, 3, MAX_MEMBERS, usize::MAX][r.below(5)];
        let votes: Vec<Option<Vote>> = (0..members.min(MAX_MEMBERS + 1))
            .map(|_| {
                (r.below(3) != 0).then(|| Vote {
                    vote: r.next().is_multiple_of(2),
                    term: r.extreme(),
                })
            })
            .collect();
        let heard: Vec<bool> = (0..members.min(MAX_MEMBERS + 2))
            .map(|_| r.next().is_multiple_of(2))
            .collect();
        let outcome = ballot(
            members,
            r.extreme(),
            &votes,
            &heard,
            r.next().is_multiple_of(2),
        );
        assert!(!outcome.unanimous || outcome.leader);

        let reports: Vec<Option<LogReport>> = (0..members.min(MAX_MEMBERS + 1))
            .map(|_| {
                (r.below(3) != 0).then(|| LogReport {
                    term: r.extreme(),
                    position: r.extreme(),
                })
            })
            .collect();
        let nomination = candidate(members, &reports, own, r.next().is_multiple_of(2));
        assert!(!nomination.unanimous || nomination.nominate);

        // The appointed leader's rules, with any member count, any counts of members
        // heard, counted and ahead, more than there are among them, any reports, and
        // any ids, none among them.
        let deadline = r.next().is_multiple_of(2);
        let appointed = appointed_canvass(members, &reports, own, deadline);
        assert!(!appointed.leads || candidate(members, &reports, own, deadline).nominate);
        assert!(!appointed.leads || appointed.ahead == 0);
        let any = |r: &mut XorShift| [0, 1, 2, members, usize::MAX][r.below(5)];
        let (heard, counted, ahead) = (any(r), any(r), any(r));
        let closes = leader_ready(members, heard, counted, ahead, r.next().is_multiple_of(2));
        assert!(!closes || (ahead == 0 && counted >= quorum(members)));
        let ids: Vec<i32> = (0..r.below(10)).map(|_| r.extreme() as i32).collect();
        let leader = appointed_leader(&ids);
        assert!(if ids.is_empty() {
            leader == -1
        } else {
            ids.iter().all(|&id| leader <= id) && ids.contains(&leader)
        });

        let candidate_term = r.extreme();
        let state = r.next() as u8;
        let request = VoteRequest {
            candidate_term_id: r.extreme(),
            log_leadership_term_id: r.extreme(),
            log_position: r.extreme(),
        };
        let answer = request_vote(candidate_term, own, state, request);
        assert!(answer.candidate_term >= candidate_term);
        if state == 0 || state == CLOSED_STATE {
            assert_eq!(answer.vote, None);
        }

        let terms = CanvassTerms {
            leadership_term_id: r.extreme(),
            log_leadership_term_id: r.extreme(),
        };
        let _ = canvass_position_action(r.extreme(), state, terms);
        let _ = replication_stop(r.extreme(), r.extreme(), r.extreme());
        let fields = TermFields {
            leadership_term_id: r.extreme(),
            log_leadership_term_id: r.extreme(),
            next_leadership_term_id: r.extreme(),
            next_term_base_log_position: r.extreme(),
            next_log_position: r.extreme(),
            term_base_log_position: r.extreme(),
        };
        let in_canvass = r.next().is_multiple_of(2);
        let _ = new_leadership_term_action(
            r.extreme(),
            r.extreme(),
            r.extreme(),
            in_canvass,
            r.extreme(),
            fields,
        );
        let spans: Vec<TermSpan> = (0..r.below(5))
            .map(|_| TermSpan {
                term: r.extreme(),
                base: r.extreme(),
                end: r.extreme(),
            })
            .collect();
        let _ = next_fields(&spans, r.extreme(), r.extreme(), r.extreme());
        let reported = r.extreme();
        let answered = answered_term(&spans, reported);
        assert!(answered == -1 || answered <= reported);
        let _ = replay_end(&spans, r.extreme());
        let _ = replication_record(
            r.extreme(),
            r.extreme(),
            r.extreme(),
            r.extreme(),
            r.extreme(),
        );
    }
}
