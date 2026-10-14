//! The log buffer on disk: `LogBuffers` creates and opens it with the spec's checks, a
//! `TermAppender` claims, commits and offers, a `TermReader` reads what was committed,
//! and the driver's duties, cleaning, unblocking, rotation, connection and end of
//! stream, do what `spec/log-buffer.md` says (ADR 0013). The full case comes first
//! (`.claude/rules/data-plane.md`, "Bounds"). Every test works in a directory of its own
//! under the system's temporary directory and removes it after. Miri has no file-backed
//! mapping, so `just miri` skips these and runs the layout, cell and conformance tests.

use std::fs;
use std::path::{Path, PathBuf};

use cavalry_proto::frame::{DataFrameHeader, Flags, FrameType};
use cavalry_shm::log::layout::{
    HEADER_LENGTH, META_DATA_LENGTH, MIN_TERM_LENGTH, NOT_ENDED, PARTITION_COUNT, encode_header,
    file_name,
};
use cavalry_shm::log::{
    ClaimError, HeaderFields, LogBuffers, LogError, LogType, Tail, UnblockOutcome,
};

struct Dir(PathBuf);

impl Dir {
    fn new(test: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("cavalry-shm-log-{}-{test}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("a temporary directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn file(&self, correlation_id: i64) -> PathBuf {
        self.0.join(file_name(correlation_id))
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

const T: u32 = MIN_TERM_LENGTH;
const MTU: u32 = 1408;
const CORRELATION: i64 = 42;

fn fields(log_type: LogType) -> HeaderFields {
    HeaderFields::new(log_type, CORRELATION, -5, T, MTU, T / 2, 1, 1001).unwrap()
}

/// A log whose publisher is never limited: the limit is the largest position.
fn unlimited() -> i64 {
    i64::MAX
}

/// Reads every committed frame from `reader`, returning the bodies.
fn drain(reader: &mut cavalry_shm::log::TermReader) -> Vec<Vec<u8>> {
    let mut bodies = Vec::new();
    loop {
        let before = reader.position();
        let poll = reader
            .poll(64, |body, _| bodies.push(body.to_vec()))
            .unwrap();
        if poll.position == before {
            return bodies;
        }
    }
}

#[test]
#[cfg_attr(miri, ignore)]
fn the_full_term_before_anything_else() {
    // A term fills, the claim that does not fit pads the rest and rotates, the retry
    // lands in the next term, and the reader follows across the term boundary.
    let dir = Dir::new("full-term");
    let log = LogBuffers::create(dir.path(), &fields(LogType::Concurrent)).unwrap();
    log.set_connected(true).unwrap();
    let mut appender = log.appender().unwrap();
    let mut reader = log.reader(0).unwrap();

    let body_len = 1000; // frame 1032, aligned 1056
    let per_term = (T / 1056) as usize; // 62 frames, 65472 bytes, 64 left
    let mut written = 0usize;
    for n in 0..per_term {
        let mut claim = appender.try_claim(body_len, unlimited()).unwrap();
        claim.body().fill(n as u8);
        let after = claim.commit();
        written += 1056;
        assert_eq!(after, written as i64, "position after frame {n}");
    }
    assert_eq!(log.tail(0), Tail::pack(-5, written as u32));
    assert_eq!(log.active_term_count(), 0);

    let refused = appender.try_claim(body_len, unlimited()).unwrap_err();
    assert_eq!(
        refused,
        ClaimError::AdminAction,
        "the frame does not fit: padding and rotation"
    );
    assert_eq!(refused.reason(), "admin-action");
    assert_eq!(log.active_term_count(), 1, "the stream rotated");
    assert_eq!(
        log.tail(1),
        Tail::pack(-4, 0),
        "the next partition holds the next term"
    );
    assert!(
        log.tail(0).raw_term_offset() > T,
        "the full term's tail is past its end"
    );
    assert_eq!(
        log.producer_position().unwrap(),
        i64::from(T),
        "the producer is at the next term's start"
    );

    let mut claim = appender.try_claim(body_len, unlimited()).unwrap();
    claim.body().fill(0xAB);
    assert_eq!(
        claim.commit(),
        i64::from(T) + 1056,
        "the retry lands in the next term"
    );

    let mut headers = Vec::new();
    let bodies = {
        let mut bodies = Vec::new();
        loop {
            let before = reader.position();
            let poll = reader
                .poll(1024, |body, header| {
                    bodies.push(body.to_vec());
                    headers.push(*header);
                })
                .unwrap();
            if poll.position == before {
                break;
            }
        }
        bodies
    };
    assert_eq!(
        bodies.len(),
        per_term + 1,
        "every data frame once, the padding skipped"
    );
    for (n, body) in bodies.iter().take(per_term).enumerate() {
        assert_eq!(body.len(), body_len);
        assert!(body.iter().all(|&b| b == n as u8), "frame {n} body");
    }
    assert!(bodies[per_term].iter().all(|&b| b == 0xAB));
    assert_eq!(headers[0].term_id(), -5);
    assert_eq!(headers[0].term_offset(), 0);
    assert_eq!(
        headers[per_term].term_id(),
        -4,
        "the last frame is in the next term"
    );
    assert_eq!(headers[per_term].term_offset(), 0);
    assert_eq!(reader.position(), i64::from(T) + 1056);
    assert_eq!(headers[0].session_id(), 1);
    assert_eq!(headers[0].stream_id(), 1001);
    assert_eq!(headers[0].flags(), Flags::UNFRAGMENTED);
}

#[test]
#[cfg_attr(miri, ignore)]
fn back_pressure_and_not_connected_at_the_limit() {
    let dir = Dir::new("limit");
    let log = LogBuffers::create(dir.path(), &fields(LogType::Concurrent)).unwrap();
    let mut appender = log.appender().unwrap();
    assert!(!log.is_connected());
    assert_eq!(
        appender.try_claim(8, 0).unwrap_err(),
        ClaimError::NotConnected
    );
    log.set_connected(true).unwrap();
    assert_eq!(
        appender.try_claim(8, 0).unwrap_err(),
        ClaimError::BackPressured
    );
    assert_eq!(appender.position().unwrap(), 0, "nothing was written");
    let claim = appender.try_claim(8, 64).unwrap();
    assert_eq!(claim.commit(), 64);
    assert_eq!(
        appender.try_claim(8, 64).unwrap_err(),
        ClaimError::BackPressured,
        "at the limit"
    );
    let claim = appender.try_claim(8, 65).unwrap();
    assert_eq!(
        claim.commit(),
        128,
        "below the limit, a claim may run past it"
    );
    let too_long = appender.try_claim(MTU as usize, unlimited()).unwrap_err();
    assert_eq!(
        too_long,
        ClaimError::TooLong {
            found: MTU as usize,
            limit: MTU as usize - 32
        }
    );
    assert_eq!(too_long.reason(), "too-long");
    assert!(
        too_long
            .to_string()
            .contains("docs/reference/log-buffer.md#"),
        "{too_long}"
    );
}

#[test]
#[cfg_attr(miri, ignore)]
fn create_then_open_round_trips_and_the_bytes_are_the_specs() {
    let dir = Dir::new("round-trip");
    let fields = fields(LogType::Exclusive);
    let log = LogBuffers::create(dir.path(), &fields).unwrap();
    assert_eq!(log.header(), fields);
    assert_eq!(
        fs::metadata(dir.file(CORRELATION)).unwrap().len(),
        fields.file_length()
    );
    assert_eq!(
        fields.file_length(),
        u64::from(T) * 3 + META_DATA_LENGTH as u64
    );
    assert_eq!(log.tail(0), Tail::pack(-5, 0));
    assert_eq!(log.tail(1), Tail::pack(-7, 0), "two terms back");
    assert_eq!(log.tail(2), Tail::pack(-6, 0), "one term back");
    assert_eq!(log.active_term_count(), 0);
    assert_eq!(log.end_of_stream_position(), NOT_ENDED);
    assert!(!log.is_connected());
    assert_eq!(log.producer_position().unwrap(), 0);

    let publisher = LogBuffers::open(dir.path(), CORRELATION, true).unwrap();
    assert_eq!(publisher.header(), fields);
    let subscriber = LogBuffers::open(dir.path(), CORRELATION, false).unwrap();
    assert_eq!(subscriber.header(), fields);
    let error = subscriber.appender().unwrap_err();
    assert!(
        matches!(error, LogError::Io(_)),
        "a read-only log has no appender: {error}"
    );
    assert!(matches!(
        subscriber.set_connected(true).unwrap_err(),
        LogError::Io(_)
    ));

    let bytes = fs::read(dir.file(CORRELATION)).unwrap();
    let meta = fields.meta_data_offset() as usize;
    assert_eq!(
        &bytes[meta..meta + HEADER_LENGTH],
        &encode_header(&fields)[..],
        "the header bytes are the spec's"
    );
    assert_eq!(
        &bytes[meta + 128..meta + 136],
        &Tail::pack(-5, 0).0.to_le_bytes(),
        "tail 0 at 128"
    );
    assert_eq!(
        &bytes[meta + 256..meta + 264],
        &Tail::pack(-7, 0).0.to_le_bytes(),
        "tail 1 at 256"
    );
    assert_eq!(
        &bytes[meta + 384..meta + 392],
        &Tail::pack(-6, 0).0.to_le_bytes(),
        "tail 2 at 384"
    );
    assert_eq!(
        &bytes[meta + 512..meta + 516],
        &0i32.to_le_bytes(),
        "active term count at 512"
    );
    assert_eq!(
        &bytes[meta + 640..meta + 648],
        &NOT_ENDED.to_le_bytes(),
        "end of stream at 640"
    );
    assert_eq!(
        &bytes[meta + 704..meta + 708],
        &0i32.to_le_bytes(),
        "is connected at 704"
    );
    assert!(bytes[..meta].iter().all(|&b| b == 0), "the terms are zero");
    assert_eq!(fields.positions().max_position(), i64::from(T) << 31);
}

#[test]
#[cfg_attr(miri, ignore)]
fn a_claim_commits_aborts_or_is_aborted_on_drop_and_the_reader_sees_it() {
    let dir = Dir::new("claim");
    let log = LogBuffers::create(dir.path(), &fields(LogType::Concurrent)).unwrap();
    log.set_connected(true).unwrap();
    let mut appender = log.appender().unwrap();
    let mut reader = log.reader(0).unwrap();

    let mut claim = appender.try_claim(5, unlimited()).unwrap();
    claim.body().copy_from_slice(b"hello");
    claim.set_reserved_value(77);
    assert_eq!(claim.position_after(), 64);
    assert_eq!(
        reader
            .poll(8, |_, _| panic!("nothing is committed"))
            .unwrap()
            .fragments,
        0
    );
    assert_eq!(claim.commit(), 64);

    let mut claim = appender.try_claim(3, unlimited()).unwrap();
    claim.body().copy_from_slice(b"bye");
    assert_eq!(claim.abort(), 128);

    {
        let mut claim = appender.try_claim(2, unlimited()).unwrap();
        claim.body().copy_from_slice(b"no");
        // dropped without commit: aborted
    }
    let mut claim = appender.try_claim(4, unlimited()).unwrap();
    claim.body().copy_from_slice(b"last");
    assert_eq!(claim.commit(), 256);

    let mut seen: Vec<(Vec<u8>, DataFrameHeader)> = Vec::new();
    let poll = reader
        .poll(8, |body, header| seen.push((body.to_vec(), *header)))
        .unwrap();
    assert_eq!(
        poll.fragments, 2,
        "the aborted and dropped claims are padding"
    );
    assert_eq!(poll.position, 256);
    assert_eq!(seen[0].0, b"hello");
    assert_eq!(seen[0].1.reserved_value(), 77);
    assert_eq!(seen[0].1.frame_type(), FrameType::Data);
    assert_eq!(seen[0].1.frame_length(), 37);
    assert_eq!(seen[1].0, b"last");
    assert_eq!(seen[1].1.term_offset(), 192);
    assert_eq!(reader.position(), 256);
    assert_eq!(appender.position().unwrap(), 256);
    assert_eq!(log.producer_position().unwrap(), 256);
}

#[test]
#[cfg_attr(miri, ignore)]
fn an_offer_fragments_a_long_message_and_the_reader_reassembles_it() {
    let dir = Dir::new("offer");
    let log = LogBuffers::create(dir.path(), &fields(LogType::Concurrent)).unwrap();
    log.set_connected(true).unwrap();
    let mut appender = log.appender().unwrap();
    let mut reader = log.reader(0).unwrap();
    let max_payload = appender.max_payload_length();
    assert_eq!(max_payload, MTU as usize - 32);
    assert_eq!(appender.max_message_length(), (T / 8) as usize);

    let short: Vec<u8> = (0..100u8).collect();
    assert_eq!(appender.offer(&short, unlimited(), 5).unwrap(), 160);
    let long: Vec<u8> = (0..3000u32).map(|i| (i % 251) as u8).collect();
    let fragments = long.len().div_ceil(max_payload);
    assert_eq!(fragments, 3);
    let framed = 2 * MTU as i64 + ((long.len() - 2 * max_payload + 32).div_ceil(32) * 32) as i64;
    assert_eq!(appender.offer(&long, unlimited(), 9).unwrap(), 160 + framed);
    let too_long = vec![0u8; appender.max_message_length() + 1];
    assert_eq!(
        appender.offer(&too_long, unlimited(), 0).unwrap_err(),
        ClaimError::TooLong {
            found: too_long.len(),
            limit: appender.max_message_length()
        }
    );

    let mut frames: Vec<(Vec<u8>, Flags, i64)> = Vec::new();
    let poll = reader
        .poll(16, |body, header| {
            frames.push((body.to_vec(), header.flags(), header.reserved_value()))
        })
        .unwrap();
    assert_eq!(poll.fragments, 4);
    assert_eq!(frames[0], (short.clone(), Flags::UNFRAGMENTED, 5));
    assert_eq!(frames[1].1, Flags::BEGIN);
    assert_eq!(frames[2].1, Flags::NONE);
    assert_eq!(frames[3].1, Flags::END);
    assert!(frames[1..].iter().all(|(_, _, reserved)| *reserved == 9));
    let reassembled: Vec<u8> = frames[1..]
        .iter()
        .flat_map(|(body, _, _)| body.clone())
        .collect();
    assert_eq!(reassembled, long);
    assert_eq!(frames[1].0.len(), max_payload);
}

#[test]
#[cfg_attr(miri, ignore)]
fn an_exclusive_log_advances_its_tail_with_stores() {
    let dir = Dir::new("exclusive");
    let log = LogBuffers::create(dir.path(), &fields(LogType::Exclusive)).unwrap();
    log.set_connected(true).unwrap();
    let mut appender = log.appender().unwrap();
    let mut reader = log.reader(0).unwrap();
    for n in 0..3u8 {
        let mut claim = appender.try_claim(1, unlimited()).unwrap();
        claim.body()[0] = n;
        claim.commit();
    }
    assert_eq!(
        log.tail(0),
        Tail::pack(-5, 192),
        "the tail is stored after each claim"
    );
    assert_eq!(appender.position().unwrap(), 192);
    assert_eq!(drain(&mut reader), vec![vec![0], vec![1], vec![2]]);

    // Fill the term; the rotation moves the exclusive tail to the next term.
    let per_term = (T / 64) as usize - 3;
    for _ in 0..per_term {
        appender.try_claim(1, unlimited()).unwrap().commit();
    }
    assert_eq!(log.tail(0), Tail::pack(-5, T));
    assert_eq!(
        appender.try_claim(1, unlimited()).unwrap_err(),
        ClaimError::AdminAction
    );
    assert_eq!(log.active_term_count(), 1);
    assert_eq!(appender.position().unwrap(), i64::from(T));
    appender.try_claim(1, unlimited()).unwrap().commit();
    assert_eq!(log.tail(1), Tail::pack(-4, 64));
    assert_eq!(drain(&mut reader).len(), per_term + 1);
    assert_eq!(reader.position(), i64::from(T) + 64);
}

#[test]
#[cfg_attr(miri, ignore)]
fn the_driver_cleans_consumed_terms_before_they_are_reused() {
    let dir = Dir::new("clean");
    let log = LogBuffers::create(dir.path(), &fields(LogType::Concurrent)).unwrap();
    log.set_connected(true).unwrap();
    let mut appender = log.appender().unwrap();
    let mut reader = log.reader(0).unwrap();
    for _ in 0..4 {
        appender.try_claim(8, unlimited()).unwrap().commit();
    }
    assert_eq!(drain(&mut reader).len(), 4);

    // Clean the first two frames: a fresh reader at 0 then sees nothing committed.
    assert_eq!(log.clean(0, 128).unwrap(), 128);
    let mut fresh = log.reader(0).unwrap();
    assert_eq!(
        fresh.poll(8, |_, _| panic!("cleaned")).unwrap().fragments,
        0
    );
    let mut from_two = log.reader(128).unwrap();
    assert_eq!(
        from_two.poll(8, |_, _| {}).unwrap().fragments,
        2,
        "the uncleaned frames remain"
    );
    let bytes = fs::read(dir.file(CORRELATION)).unwrap();
    assert!(
        bytes[..128].iter().all(|&b| b == 0),
        "the cleaned bytes are zero"
    );

    // Cleaning stops at the end of a term and reports where it got to.
    let end_of_term = i64::from(T);
    assert_eq!(
        log.clean(end_of_term - 64, end_of_term + 64).unwrap(),
        end_of_term
    );
    assert_eq!(
        log.clean(end_of_term, end_of_term + 64).unwrap(),
        end_of_term + 64
    );
    assert_eq!(log.clean(256, 256).unwrap(), 256, "nothing to clean");
}

#[test]
#[cfg_attr(miri, ignore)]
fn the_driver_unblocks_a_stale_claim_a_gap_and_the_end_of_a_term() {
    let dir = Dir::new("unblock");
    let log = LogBuffers::create(dir.path(), &fields(LogType::Concurrent)).unwrap();
    log.set_connected(true).unwrap();
    let mut appender = log.appender().unwrap();
    let mut reader = log.reader(0).unwrap();

    // A committed frame at the consumer position: nothing to do.
    appender.try_claim(8, unlimited()).unwrap().commit();
    assert_eq!(log.unblock(0).unwrap(), UnblockOutcome::NoAction);

    // A stale claim: the publisher claimed and never committed (std::mem::forget stands
    // for a dead process).
    let claim = appender.try_claim(8, unlimited()).unwrap();
    std::mem::forget(claim);
    appender.try_claim(8, unlimited()).unwrap().commit();
    assert_eq!(
        drain(&mut reader).len(),
        1,
        "the reader stops at the stale claim"
    );
    assert_eq!(reader.position(), 64);
    assert_eq!(log.unblock(64).unwrap(), UnblockOutcome::Unblocked);
    assert_eq!(
        drain(&mut reader).len(),
        1,
        "the padding is skipped and the later frame delivered"
    );
    assert_eq!(reader.position(), 192);

    // A gap of zeros before a later frame: the tail moved past bytes nobody wrote
    // (another publisher died before its claim header). Simulate with a second appender
    // whose claim is forgotten before the header is visible: we cannot, so clean the
    // frame's header instead.
    appender.try_claim(8, unlimited()).unwrap().commit(); // 192..256
    appender.try_claim(8, unlimited()).unwrap().commit(); // 256..320
    let zeroed = log.clean(192, 256).unwrap();
    assert_eq!(zeroed, 256);
    assert_eq!(
        log.unblock(192).unwrap(),
        UnblockOutcome::Unblocked,
        "a gap of zeros becomes padding"
    );
    assert_eq!(drain(&mut reader).len(), 1);
    assert_eq!(reader.position(), 320);

    // A gap of zeros to the end of the term, with the tail at the end: padding and a
    // rotation.
    let remaining = (T as usize - 320) / 64;
    for _ in 0..remaining {
        appender.try_claim(8, unlimited()).unwrap().commit();
    }
    assert_eq!(log.tail(0), Tail::pack(-5, T));
    assert_eq!(log.clean(320, i64::from(T)).unwrap(), i64::from(T));
    assert_eq!(log.unblock(320).unwrap(), UnblockOutcome::UnblockedToEnd);
    assert_eq!(log.active_term_count(), 1);
    assert_eq!(drain(&mut reader).len(), 0);
    assert_eq!(
        reader.position(),
        i64::from(T),
        "the reader moved over the padding into the next term"
    );

    // The consumer position at the start of the next term while the publisher has not
    // rotated: the driver rotates.
    appender.try_claim(8, unlimited()).unwrap().commit();
    assert_eq!(log.tail(1), Tail::pack(-4, 64));
    assert_eq!(
        log.unblock(i64::from(T) * 2).unwrap(),
        UnblockOutcome::Rotated
    );
    assert_eq!(log.active_term_count(), 2);
    assert_eq!(log.tail(2), Tail::pack(-3, 0));
}

#[test]
#[cfg_attr(miri, ignore)]
fn end_of_stream_and_connection_are_the_drivers() {
    let dir = Dir::new("eos");
    let log = LogBuffers::create(dir.path(), &fields(LogType::Concurrent)).unwrap();
    log.set_connected(true).unwrap();
    let mut appender = log.appender().unwrap();
    appender.try_claim(8, unlimited()).unwrap().commit();
    let mut reader = log.reader(0).unwrap();
    assert!(!reader.is_end_of_stream());
    log.set_end_of_stream_position(log.producer_position().unwrap())
        .unwrap();
    assert_eq!(log.end_of_stream_position(), 64);
    assert!(
        !reader.is_end_of_stream(),
        "not until the reader gets there"
    );
    drain(&mut reader);
    assert!(reader.is_end_of_stream());
    log.set_connected(false).unwrap();
    assert!(!log.is_connected());
}

#[test]
#[cfg_attr(miri, ignore)]
fn a_reader_checks_positions_and_frames() {
    let dir = Dir::new("reader-checks");
    let log = LogBuffers::create(dir.path(), &fields(LogType::Concurrent)).unwrap();
    log.set_connected(true).unwrap();
    let mut appender = log.appender().unwrap();
    for _ in 0..3 {
        appender.try_claim(8, unlimited()).unwrap().commit();
    }
    let error = log.reader(33).unwrap_err();
    assert!(
        matches!(error, LogError::Position { found: 33, .. }),
        "{error}"
    );
    assert_eq!(error.reason(), "position");
    assert!(matches!(
        log.reader(-32).unwrap_err(),
        LogError::Position { .. }
    ));
    assert!(matches!(
        log.reader(i64::from(T) << 31).unwrap_err(),
        LogError::Position { .. }
    ));

    let mut reader = log.reader(0).unwrap();
    reader.seek(64).unwrap();
    assert!(matches!(
        reader.seek(0).unwrap_err(),
        LogError::Position {
            found: 0,
            current: 64,
            ..
        }
    ));
    assert!(matches!(
        reader.seek(100).unwrap_err(),
        LogError::Position { found: 100, .. }
    ));
    assert!(
        matches!(
            reader.seek(i64::from(T) + 32).unwrap_err(),
            LogError::Position { .. }
        ),
        "beyond the end of the current term"
    );
    reader.seek(i64::from(T)).unwrap();
    reader.seek(i64::from(T) + 32).unwrap();
    let mut reader = log.reader(64).unwrap();
    assert_eq!(
        reader.poll(8, |_, _| {}).unwrap(),
        cavalry_shm::log::Poll {
            position: 192,
            fragments: 2
        }
    );

    // A corrupt frame: the version byte of the third frame is 1. The reader reports it
    // and stays there.
    let path = dir.file(CORRELATION);
    let mut bytes = fs::read(&path).unwrap();
    bytes[128 + 4] = 1;
    fs::write(&path, &bytes).unwrap();
    let log = LogBuffers::open(dir.path(), CORRELATION, false).unwrap();
    let mut reader = log.reader(0).unwrap();
    let error = reader.poll(8, |_, _| {}).unwrap_err();
    assert!(matches!(error, LogError::Frame(_)), "{error}");
    assert_eq!(error.reason(), "frame");
    assert!(
        error.to_string().contains("frame rejected (version)"),
        "{error}"
    );
    assert_eq!(reader.position(), 128, "the two good frames were delivered");
}

#[test]
#[cfg_attr(miri, ignore)]
fn a_block_holds_whole_committed_frames_up_to_the_limit_and_stops_at_a_claim() {
    let dir = Dir::new("block");
    let log = LogBuffers::create(dir.path(), &fields(LogType::Concurrent)).unwrap();
    log.set_connected(true).unwrap();
    let mut appender = log.appender().unwrap();
    let mut reader = log.reader(0).unwrap();

    let empty = reader.block(usize::MAX).unwrap();
    assert!(empty.is_empty() && !empty.padding && empty.bytes.is_empty());
    assert_eq!((empty.position, empty.end()), (0, 0));

    for n in 0..3u8 {
        let mut claim = appender.try_claim(5, unlimited()).unwrap();
        claim.body().copy_from_slice(&[n; 5]);
        claim.commit(); // frame 37, aligned 64
    }
    let mut claim = appender.try_claim(5, unlimited()).unwrap();
    claim.body().copy_from_slice(b"later");

    let file = fs::read(dir.file(CORRELATION)).unwrap();
    let block = reader.block(usize::MAX).unwrap();
    assert_eq!(
        (block.position, block.length, block.padding),
        (0, 192, false)
    );
    assert_eq!(block.bytes, &file[..192], "three frames, headers included");
    assert_eq!(block.end(), 192);
    assert_eq!(reader.position(), 0, "the position moves only on seek");
    assert_eq!(
        reader.block(100).unwrap().length,
        64,
        "the second frame does not fit"
    );
    assert_eq!(reader.block(128).unwrap().length, 128);
    assert_eq!(
        reader.block(0).unwrap().length,
        64,
        "the first frame is always taken"
    );

    assert_eq!(claim.commit(), 256);
    let block = reader.block(usize::MAX).unwrap();
    assert_eq!(block.length, 256);
    assert_eq!(&block.bytes[192 + 32..192 + 37], b"later");
    assert_eq!(
        format!("{block:?}"),
        "Block { position: 0, length: 256, padding: false }"
    );
    let end = block.end();
    reader.seek(end).unwrap();
    assert!(reader.block(usize::MAX).unwrap().is_empty());
}

#[test]
#[cfg_attr(miri, ignore)]
fn a_padding_frame_is_a_block_of_its_own_and_the_next_block_is_in_the_next_term() {
    let dir = Dir::new("block-padding");
    let log = LogBuffers::create(dir.path(), &fields(LogType::Concurrent)).unwrap();
    log.set_connected(true).unwrap();
    let mut appender = log.appender().unwrap();
    let mut reader = log.reader(0).unwrap();

    let body_len = 1000; // frame 1032, aligned 1056
    let per_term = (T / 1056) as usize; // 62 frames, 65472 bytes, 64 left
    for n in 0..per_term {
        let mut claim = appender.try_claim(body_len, unlimited()).unwrap();
        claim.body().fill(n as u8);
        claim.commit();
    }
    assert_eq!(
        appender.try_claim(body_len, unlimited()).unwrap_err(),
        ClaimError::AdminAction,
        "the frame does not fit: padding and rotation"
    );
    let mut claim = appender.try_claim(body_len, unlimited()).unwrap();
    claim.body().fill(0xAB);
    assert_eq!(claim.commit(), i64::from(T) + 1056);

    let file = fs::read(dir.file(CORRELATION)).unwrap();
    let data = reader.block(usize::MAX).unwrap();
    assert_eq!(
        (data.position, data.length, data.padding),
        (0, 65472, false)
    );
    assert_eq!(
        data.bytes,
        &file[..65472],
        "the block stops before the padding"
    );
    reader.seek(data.end()).unwrap();

    let padding = reader.block(usize::MAX).unwrap();
    assert_eq!(padding.position, 65472);
    assert!(padding.padding);
    assert_eq!(padding.bytes.len(), 32, "a padding frame's header alone");
    assert_eq!(padding.bytes, &file[65472..65472 + 32]);
    assert_eq!(padding.length, 64, "its aligned length in the stream");
    assert_eq!(padding.end(), i64::from(T));
    assert_eq!(
        &padding.bytes[..4],
        &64i32.to_le_bytes(),
        "the padding frame's length"
    );
    reader.seek(padding.end()).unwrap();

    let next = reader.block(usize::MAX).unwrap();
    assert_eq!(
        (next.position, next.length, next.padding),
        (i64::from(T), 1056, false)
    );
    assert_eq!(
        next.bytes,
        &file[T as usize..T as usize + 1056],
        "the next term's first frame"
    );
    assert_eq!(next.bytes[32], 0xAB);
}

#[test]
#[cfg_attr(miri, ignore)]
fn a_block_stops_at_a_bad_frame_and_at_its_limit_without_reading_past_it() {
    let dir = Dir::new("block-checks");
    let log = LogBuffers::create(dir.path(), &fields(LogType::Concurrent)).unwrap();
    log.set_connected(true).unwrap();
    let mut appender = log.appender().unwrap();
    for _ in 0..3 {
        appender.try_claim(8, unlimited()).unwrap().commit();
    }
    // The version byte of the third frame is 1: the block from 0 is refused, and the
    // block from 0 with a limit of two frames never reads the third.
    let path = dir.file(CORRELATION);
    let mut bytes = fs::read(&path).unwrap();
    bytes[128 + 4] = 1;
    fs::write(&path, &bytes).unwrap();
    let log = LogBuffers::open(dir.path(), CORRELATION, false).unwrap();
    let reader = log.reader(0).unwrap();
    let error = reader.block(usize::MAX).unwrap_err();
    assert!(matches!(error, LogError::Frame(_)), "{error}");
    assert!(
        error.to_string().contains("frame rejected (version)"),
        "{error}"
    );
    assert_eq!(reader.position(), 0);
    assert_eq!(reader.block(128).unwrap().length, 128);
    let reader = log.reader(128).unwrap();
    assert!(matches!(
        reader.block(usize::MAX).unwrap_err(),
        LogError::Frame(_)
    ));
}

/// A valid file whose metadata header is then changed by `patch`, for the header checks.
fn log_with_header(dir: &Dir, patch: impl FnOnce(&mut [u8])) {
    let log = LogBuffers::create(dir.path(), &fields(LogType::Concurrent)).unwrap();
    drop(log);
    let path = dir.file(CORRELATION);
    let mut bytes = fs::read(&path).unwrap();
    let meta = bytes.len() - META_DATA_LENGTH;
    patch(&mut bytes[meta..meta + HEADER_LENGTH]);
    fs::write(&path, bytes).unwrap();
}

#[test]
#[cfg_attr(miri, ignore)]
fn a_reader_rejects_a_bad_file_by_name() {
    let dir = Dir::new("rejects");
    let open = |dir: &Dir| LogBuffers::open(dir.path(), CORRELATION, false).unwrap_err();

    fs::create_dir_all(dir.path().join("publications")).unwrap();
    fs::write(dir.file(CORRELATION), [0u8; 100]).unwrap();
    assert_eq!(open(&dir).reason(), "too-short");
    fs::remove_file(dir.file(CORRELATION)).unwrap();

    log_with_header(&dir, |h| h[..8].copy_from_slice(&[0; 8]));
    assert_eq!(open(&dir).reason(), "not-ready");
    fs::remove_file(dir.file(CORRELATION)).unwrap();

    log_with_header(&dir, |h| h[..8].copy_from_slice(b"CVLYCNTR"));
    let error = open(&dir);
    assert_eq!(error.reason(), "magic");
    assert!(error.to_string().contains("43564c59434e5452"), "{error}");
    fs::remove_file(dir.file(CORRELATION)).unwrap();

    for (name, patch) in [
        (
            "version",
            Box::new(|h: &mut [u8]| h[8] = 1) as Box<dyn Fn(&mut [u8])>,
        ),
        ("type", Box::new(|h: &mut [u8]| h[12] = 2)),
        (
            "term-length",
            Box::new(|h: &mut [u8]| h[28..32].copy_from_slice(&100_000i32.to_le_bytes())),
        ),
        (
            "mtu",
            Box::new(|h: &mut [u8]| h[32..36].copy_from_slice(&1400i32.to_le_bytes())),
        ),
        (
            "window",
            Box::new(|h: &mut [u8]| h[36..40].copy_from_slice(&0i32.to_le_bytes())),
        ),
        ("frame-header", Box::new(|h: &mut [u8]| h[64 + 6] = 0)),
    ] {
        log_with_header(&dir, |h| patch(h));
        let error = open(&dir);
        assert_eq!(error.reason(), name, "{error}");
        assert!(
            error.to_string().contains("docs/reference/log-buffer.md#"),
            "{error}"
        );
        fs::remove_file(dir.file(CORRELATION)).unwrap();
    }

    // The metadata page is the file's last 4096 bytes, so a header whose term length
    // disagrees with the file's length is `length`: here a header for 128 KiB terms in a
    // file sized for 64 KiB terms.
    log_with_header(&dir, |h| {
        h[28..32].copy_from_slice(&(2 * T as i32).to_le_bytes())
    });
    let error = open(&dir);
    let full = u64::from(T) * 3 + META_DATA_LENGTH as u64;
    assert!(
        matches!(error, LogError::Length { found, needed } if found == full && needed == full + 3 * u64::from(T)),
        "{error}"
    );
    assert_eq!(error.reason(), "length");
    fs::remove_file(dir.file(CORRELATION)).unwrap();

    // A file padded or truncated after creation no longer ends with its header, so it
    // reads as not ready: the header is found from the file's size.
    let log = LogBuffers::create(dir.path(), &fields(LogType::Concurrent)).unwrap();
    drop(log);
    let path = dir.file(CORRELATION);
    fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(full + 4096)
        .unwrap();
    assert_eq!(open(&dir).reason(), "not-ready");
    fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(full - 4096)
        .unwrap();
    assert_eq!(
        open(&dir).reason(),
        "not-ready",
        "truncated, the last page is term bytes"
    );
    fs::remove_file(&path).unwrap();
    let log = LogBuffers::create(dir.path(), &fields(LogType::Concurrent)).unwrap();
    drop(log);

    let error = LogBuffers::create(dir.path(), &fields(LogType::Concurrent)).unwrap_err();
    assert_eq!(
        error.reason(),
        "io",
        "a log that exists is not recreated: {error}"
    );
    assert!(
        error
            .to_string()
            .contains("docs/reference/log-buffer.md#file"),
        "{error}"
    );
}

#[test]
#[cfg_attr(miri, ignore)]
fn header_fields_are_checked_in_the_specs_order() {
    let new =
        |term, mtu, window| HeaderFields::new(LogType::Concurrent, 1, 0, term, mtu, window, 1, 2);
    assert_eq!(new(100_000, 1408, 64).unwrap_err().reason(), "term-length");
    assert_eq!(new(T, 1400, 64).unwrap_err().reason(), "mtu");
    assert_eq!(new(T, 32, 64).unwrap_err().reason(), "mtu");
    assert_eq!(new(T, 65536, 64).unwrap_err().reason(), "mtu");
    assert_eq!(new(T, 1408, 0).unwrap_err().reason(), "window");
    assert_eq!(new(T, 1408, T / 2 + 1).unwrap_err().reason(), "window");
    assert_eq!(
        new(100_000, 1400, 0).unwrap_err().reason(),
        "term-length",
        "term-length comes first"
    );
    let fields = new(T, 1408, 1).unwrap();
    assert_eq!(fields.max_payload_length(), 1376);
    assert_eq!(fields.max_message_length(), T / 8);
    assert_eq!(
        HeaderFields::new(LogType::Concurrent, 1, 0, 1 << 30, 1408, 1, 1, 2)
            .unwrap()
            .max_message_length(),
        16 << 20
    );
    assert_eq!(fields.term_buffer_offset(2), 2 * u64::from(T));
    assert_eq!(PARTITION_COUNT, 3);
}

#[test]
#[cfg_attr(miri, ignore)]
fn every_error_names_the_page_that_explains_it() {
    let errors = [
        LogError::TooShort { given: 1 },
        LogError::NotReady,
        LogError::Magic {
            found: *b"CVLYCNTR",
        },
        LogError::Version { found: 1 },
        LogError::Type { found: 2 },
        LogError::TermLength { found: 100 },
        LogError::Mtu { found: 1400 },
        LogError::Window {
            found: 0,
            term_length: T,
        },
        LogError::FrameHeader,
        LogError::Length {
            found: 1,
            needed: 2,
        },
        LogError::Changed,
        LogError::Position {
            found: 33,
            current: 0,
            limit: 64,
        },
        LogError::Tail {
            found: 1,
            expected: 2,
        },
        LogError::Frame(cavalry_proto::frame::FrameError::Version { found: 1 }),
        LogError::TooLong { found: 5, limit: 4 },
        LogError::Io(std::io::Error::other("refused")),
    ];
    let reasons: Vec<&str> = errors.iter().map(LogError::reason).collect();
    assert_eq!(
        reasons,
        [
            "too-short",
            "not-ready",
            "magic",
            "version",
            "type",
            "term-length",
            "mtu",
            "window",
            "frame-header",
            "length",
            "changed",
            "position",
            "tail",
            "frame",
            "too-long",
            "io"
        ]
    );
    for error in &errors {
        let message = error.to_string();
        assert!(
            message.contains(&format!("({})", error.reason())),
            "{message}"
        );
        assert!(message.contains("docs/reference/"), "{message}");
    }
    for error in [
        ClaimError::NotConnected,
        ClaimError::BackPressured,
        ClaimError::AdminAction,
        ClaimError::MaxPositionExceeded,
        ClaimError::TooLong { found: 1, limit: 0 },
    ] {
        let message = error.to_string();
        assert!(
            message.contains(&format!("({})", error.reason())),
            "{message}"
        );
        assert!(
            message.ends_with("docs/reference/log-buffer.md#claim-commit-and-abort"),
            "{message}"
        );
    }
}
