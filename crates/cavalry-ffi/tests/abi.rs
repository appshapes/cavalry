//! The ABI's rules over a driver in the process (`spec/c-abi.md`; ADR 0051): the
//! version check, the strings, a claim and a poll through the C handler with the
//! header's values and the frame's position from the image's constants, the calls a
//! live claim or a running poll refuses, which claim a struct names, a refused
//! claim's struct, the closed codes and the long message's anchor.

use std::ffi::{CStr, CString, c_void};
use std::path::{Path, PathBuf};
use std::ptr;

use cavalry_driver::Driver;
use cavalry_ffi::*;

struct TestDir(PathBuf);

impl TestDir {
    fn new(test: &str) -> Self {
        let path = std::env::temp_dir().join(format!("cavalry-ffi-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        let _ = std::fs::remove_dir_all(path.with_extension("log"));
        TestDir(path)
    }
    fn path(&self) -> &Path {
        &self.0
    }
    fn log_dir(&self) -> PathBuf {
        self.0.with_extension("log")
    }
    fn c_string(&self) -> CString {
        CString::new(self.0.to_str().expect("a UTF-8 temp dir")).expect("no NUL")
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
        let _ = std::fs::remove_dir_all(self.log_dir());
    }
}

fn errmsg() -> String {
    // SAFETY: the message is NUL-terminated and valid until the next failing call.
    unsafe { CStr::from_ptr(cavalry_errmsg()) }
        .to_str()
        .expect("UTF-8")
        .to_string()
}

/// A client on a driver of its own, with a subscription and a publication on
/// `cavalry:ipc` stream 1001, ready to publish.
struct Setup {
    _driver: Driver,
    _dir: TestDir,
    context: *mut cavalry_context_t,
    client: *mut cavalry_t,
    subscription: *mut cavalry_subscription_t,
    publication: *mut cavalry_publication_t,
    received: Vec<Fragment>,
}

#[derive(Debug, Clone, PartialEq)]
struct Fragment {
    body: Vec<u8>,
    session_id: i32,
    term_id: i32,
    term_offset: i32,
    reserved_value: i64,
    flags: u8,
}

unsafe extern "C" fn collect(
    clientd: *mut c_void,
    buffer: *const u8,
    length: usize,
    header: *const cavalry_header_t,
) {
    // SAFETY: the test passes a `Vec<Fragment>`, and the body and header are valid for
    // the call.
    unsafe {
        let received = &mut *clientd.cast::<Vec<Fragment>>();
        let body = std::slice::from_raw_parts(buffer, length).to_vec();
        let mut values = std::mem::zeroed::<cavalry_header_values_t>();
        assert_eq!(cavalry_header_values(header, &mut values), 0);
        received.push(Fragment {
            body,
            session_id: values.frame.session_id,
            term_id: values.frame.term_id,
            term_offset: values.frame.term_offset,
            reserved_value: values.frame.reserved_value,
            flags: values.frame.flags,
        });
    }
}

impl Setup {
    fn new(test: &str) -> Self {
        let dir = TestDir::new(test);
        let driver = Driver::start(dir.path(), dir.log_dir()).expect("the driver");
        let dir_c = dir.c_string();
        let channel = CString::new("cavalry:ipc").unwrap();
        // SAFETY: the ABI's conventions, held by this test.
        unsafe {
            let mut context = ptr::null_mut();
            assert_eq!(cavalry_context_init_v(&mut context, CAVALRY_ABI_VERSION), 0);
            assert_eq!(cavalry_context_set_dir(context, dir_c.as_ptr()), 0);
            let mut client = ptr::null_mut();
            assert_eq!(cavalry_init(&mut client, context), 0, "{}", errmsg());
            let mut subscription = ptr::null_mut();
            assert_eq!(
                cavalry_add_subscription(&mut subscription, client, channel.as_ptr(), 1001),
                0,
                "{}",
                errmsg()
            );
            let mut publication = ptr::null_mut();
            assert_eq!(
                cavalry_add_publication(&mut publication, client, channel.as_ptr(), 1001),
                0,
                "{}",
                errmsg()
            );
            Setup {
                _driver: driver,
                _dir: dir,
                context,
                client,
                subscription,
                publication,
                received: Vec::new(),
            }
        }
    }

    fn poll(&mut self, limit: usize) -> i32 {
        let received: *mut Vec<Fragment> = &mut self.received;
        // SAFETY: the handle and the handler are this test's.
        unsafe {
            cavalry_subscription_poll(self.subscription, Some(collect), received.cast(), limit)
        }
    }

    /// Polls with `count` into `counted`, up to 4,096 fragments.
    fn poll_counting(&mut self, counted: &mut Count) -> i32 {
        let counted: *mut Count = counted;
        // SAFETY: the handle and the handler are this test's.
        unsafe { cavalry_subscription_poll(self.subscription, Some(count), counted.cast(), 4096) }
    }

    /// Claims and commits `body`, polling the subscription until the image joins.
    fn publish(&mut self, body: &[u8]) -> i64 {
        let mut claim = cavalry_buffer_claim_t {
            data: ptr::null_mut(),
            length: 0,
            position_after: 0,
            publication: ptr::null_mut(),
        };
        for _ in 0..100_000 {
            // SAFETY: the ABI's conventions.
            let position =
                unsafe { cavalry_publication_try_claim(self.publication, body.len(), &mut claim) };
            if position >= 0 {
                assert_eq!(claim.length, body.len());
                assert_eq!(claim.position_after, position);
                assert_eq!(claim.publication, self.publication);
                // SAFETY: `claim.data` is the frame's body of `length` bytes.
                unsafe {
                    ptr::copy_nonoverlapping(body.as_ptr(), claim.data, body.len());
                    assert_eq!(cavalry_buffer_claim_commit(&mut claim), position);
                }
                return position;
            }
            assert_ne!(position, CAVALRY_PUBLICATION_ERROR, "{}", errmsg());
            self.poll(1);
        }
        panic!("the publication never connected");
    }

    fn receive(&mut self, count: usize) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while self.received.len() < count {
            assert!(self.poll(10) >= 0, "{}", errmsg());
            assert!(
                std::time::Instant::now() < deadline,
                "received {} of {count}",
                self.received.len()
            );
        }
    }
}

impl Drop for Setup {
    fn drop(&mut self) {
        // SAFETY: each handle is closed once, here.
        unsafe {
            cavalry_publication_close(self.publication);
            cavalry_subscription_close(self.subscription);
            cavalry_close(self.client);
            cavalry_context_close(self.context);
        }
    }
}

#[test]
fn a_message_crosses_the_abi_once_with_its_header_and_its_position() {
    let mut setup = Setup::new("round-trip");
    let position = setup.publish(b"hello");
    setup.receive(1);
    let got = &setup.received[0];
    assert_eq!(got.body, b"hello");
    assert_eq!(got.flags, CAVALRY_FLAGS_UNFRAGMENTED);
    // The frame's position from the image's constants, as the page computes it.
    // SAFETY: the ABI's conventions.
    unsafe {
        assert_eq!(cavalry_subscription_image_count(setup.subscription), 1);
        let image = cavalry_subscription_image_at_index(setup.subscription, 0);
        assert!(!image.is_null());
        assert!(cavalry_subscription_image_at_index(setup.subscription, 1).is_null());
        let mut constants = std::mem::zeroed::<cavalry_image_constants_t>();
        assert_eq!(cavalry_image_constants(image, &mut constants), 0);
        assert_eq!(constants.session_id, got.session_id);
        assert_eq!(
            CStr::from_ptr(constants.source_identity).to_str().unwrap(),
            "cavalry:ipc"
        );
        let term_count = (got.term_id as u32).wrapping_sub(constants.initial_term_id as u32) as i32;
        let frame_position = (i64::from(term_count) << constants.position_bits_to_shift)
            + i64::from(got.term_offset);
        // A 5-byte body is a 37-byte frame, aligned to 64: the frame begins 64 before
        // the position after it.
        assert_eq!(frame_position, position - 64);
        assert_eq!(cavalry_image_position(image), position);
        assert!(!cavalry_image_is_end_of_stream(image));
        let mut sub_constants = std::mem::zeroed::<cavalry_subscription_constants_t>();
        assert_eq!(
            cavalry_subscription_constants(setup.subscription, &mut sub_constants),
            0
        );
        assert_eq!(sub_constants.stream_id, 1001);
        assert_eq!(
            CStr::from_ptr(sub_constants.channel).to_str().unwrap(),
            "cavalry:ipc"
        );
        let mut pub_constants = std::mem::zeroed::<cavalry_publication_constants_t>();
        assert_eq!(
            cavalry_publication_constants(setup.publication, &mut pub_constants),
            0
        );
        assert_eq!(pub_constants.session_id, got.session_id);
        assert!(!pub_constants.is_exclusive);
        assert!(cavalry_publication_is_connected(setup.publication));
        assert_eq!(cavalry_publication_position(setup.publication), position);
        assert!(cavalry_subscription_is_connected(setup.subscription));
        let mut fault = std::mem::zeroed::<cavalry_fault_t>();
        assert_eq!(
            cavalry_subscription_fault(setup.subscription, &mut fault),
            0
        );
        assert_eq!(fault.code, 0);
    }
}

#[test]
fn an_offer_and_a_reserved_value_reach_the_handler() {
    let mut setup = Setup::new("offer");
    setup.publish(b"first");
    setup.receive(1);
    // SAFETY: the ABI's conventions.
    unsafe {
        let message = b"offered";
        let position =
            cavalry_publication_offer(setup.publication, message.as_ptr(), message.len());
        assert!(position > 0, "{}", errmsg());
        let mut claim = std::mem::zeroed::<cavalry_buffer_claim_t>();
        let after = cavalry_publication_try_claim(setup.publication, 3, &mut claim);
        assert!(after > position);
        assert_eq!(cavalry_buffer_claim_set_reserved_value(&mut claim, 42), 0);
        ptr::copy_nonoverlapping(b"abc".as_ptr(), claim.data, 3);
        assert_eq!(cavalry_buffer_claim_commit(&mut claim), after);
    }
    setup.receive(3);
    assert_eq!(setup.received[1].body, b"offered");
    assert_eq!(setup.received[2].body, b"abc");
    assert_eq!(setup.received[2].reserved_value, 42);
}

#[test]
fn a_live_claim_refuses_the_calls_the_rust_borrow_would() {
    let mut setup = Setup::new("live-claim");
    setup.publish(b"warm");
    setup.receive(1);
    // SAFETY: the ABI's conventions.
    unsafe {
        let mut idle = std::mem::zeroed::<cavalry_publication_constants_t>();
        assert_eq!(
            cavalry_publication_constants(setup.publication, &mut idle),
            0
        );
        let mut claim = std::mem::zeroed::<cavalry_buffer_claim_t>();
        let after = cavalry_publication_try_claim(setup.publication, 4, &mut claim);
        assert!(after > 0, "{}", errmsg());
        // A second claim, with another struct: refused and that struct cleared.
        let mut other = std::mem::zeroed::<cavalry_buffer_claim_t>();
        other.data = ptr::dangling_mut();
        assert_eq!(
            cavalry_publication_try_claim(setup.publication, 4, &mut other),
            CAVALRY_PUBLICATION_ERROR
        );
        assert_eq!(cavalry_errcode(), CAVALRY_ERR_CLAIM_OUTSTANDING);
        assert!(other.data.is_null());
        assert_eq!(other.position_after, CAVALRY_NULL_POSITION);
        // A second claim with the struct that names the live claim: refused, untouched.
        assert_eq!(
            cavalry_publication_try_claim(setup.publication, 4, &mut claim),
            CAVALRY_PUBLICATION_ERROR
        );
        assert_eq!(claim.position_after, after);
        assert!(!claim.data.is_null());
        assert_eq!(
            cavalry_publication_offer(setup.publication, b"x".as_ptr(), 1),
            CAVALRY_PUBLICATION_ERROR
        );
        assert_eq!(cavalry_errcode(), CAVALRY_ERR_CLAIM_OUTSTANDING);
        assert_eq!(cavalry_publication_position(setup.publication), -1);
        assert_eq!(cavalry_errcode(), CAVALRY_ERR_CLAIM_OUTSTANDING);
        assert!(!cavalry_publication_is_connected(setup.publication));
        assert_eq!(cavalry_errcode(), CAVALRY_ERR_CLAIM_OUTSTANDING);
        // The constants answer during the claim, from the handle's copy, as before it.
        let mut constants = std::mem::zeroed::<cavalry_publication_constants_t>();
        assert_eq!(
            cavalry_publication_constants(setup.publication, &mut constants),
            0
        );
        assert_eq!(constants.channel, idle.channel);
        assert_eq!(constants.registration_id, idle.registration_id);
        assert_eq!(constants.session_id, idle.session_id);
        assert_eq!(constants.stream_id, 1001);
        assert_eq!(constants.max_payload_length, idle.max_payload_length);
        assert_eq!(constants.max_message_length, idle.max_message_length);
        assert!(constants.max_payload_length > 0 && constants.max_message_length > 0);
        assert_eq!(constants.is_exclusive, idle.is_exclusive);
        ptr::copy_nonoverlapping(b"live".as_ptr(), claim.data, 4);
        assert_eq!(cavalry_buffer_claim_commit(&mut claim), after);
        assert!(cavalry_publication_is_connected(setup.publication));
    }
    setup.receive(2);
    assert_eq!(setup.received[1].body, b"live");
}

/// ADR 0051, "Which claim a struct names": c1's released struct and its copy answer
/// `NO_CLAIM` beside c2 and again three terms later, beside the claim whose body is at
/// c1's address; each claim they stood beside is read once with its own body.
#[test]
fn a_stale_claim_struct_leaves_the_outstanding_claim_untouched() {
    let mut setup = Setup::new("stale-claim");
    setup.publish(b"warm");
    setup.receive(1);
    // SAFETY: the ABI's conventions.
    unsafe {
        let mut c1 = std::mem::zeroed::<cavalry_buffer_claim_t>();
        let after1 = cavalry_publication_try_claim(setup.publication, 2, &mut c1);
        assert!(after1 > 0);
        let copy = cavalry_buffer_claim_t { ..c1 };
        ptr::copy_nonoverlapping(b"c1".as_ptr(), c1.data, 2);
        assert_eq!(cavalry_buffer_claim_commit(&mut c1), after1);
        // The released struct: in a debug build poisoned, in release as the commit found it.
        let mut c2 = std::mem::zeroed::<cavalry_buffer_claim_t>();
        let after2 = cavalry_publication_try_claim(setup.publication, 2, &mut c2);
        assert!(after2 > after1);
        let body2 = std::slice::from_raw_parts(c2.data, 2).to_vec();
        for stale in [&mut c1, &mut cavalry_buffer_claim_t { ..copy }] {
            assert_eq!(cavalry_buffer_claim_commit(stale), -1);
            assert_eq!(cavalry_errcode(), CAVALRY_ERR_NO_CLAIM, "{}", errmsg());
            assert_eq!(cavalry_buffer_claim_abort(stale), -1);
            assert_eq!(cavalry_errcode(), CAVALRY_ERR_NO_CLAIM);
            assert_eq!(cavalry_buffer_claim_set_reserved_value(stale, 1), -1);
            assert_eq!(cavalry_errcode(), CAVALRY_ERR_NO_CLAIM);
        }
        assert_eq!(
            std::slice::from_raw_parts(c2.data, 2),
            body2.as_slice(),
            "c2's body is untouched"
        );
        ptr::copy_nonoverlapping(b"c2".as_ptr(), c2.data, 2);
        assert_eq!(cavalry_buffer_claim_commit(&mut c2), after2);
        // A commit of a struct with a null publication is NO_CLAIM, not NULL.
        let mut none = std::mem::zeroed::<cavalry_buffer_claim_t>();
        assert_eq!(cavalry_buffer_claim_commit(&mut none), -1);
        assert_eq!(cavalry_errcode(), CAVALRY_ERR_NO_CLAIM);
        // A refused claim, too long: the struct is cleared and the error names both numbers.
        let mut too_long = std::mem::zeroed::<cavalry_buffer_claim_t>();
        too_long.data = ptr::dangling_mut();
        assert_eq!(
            cavalry_publication_try_claim(setup.publication, 1 << 40, &mut too_long),
            CAVALRY_PUBLICATION_ERROR
        );
        assert_eq!(cavalry_errcode(), CAVALRY_ERR_TOO_LONG);
        assert!(errmsg().contains("too-long"), "{}", errmsg());
        assert!(too_long.data.is_null() && too_long.publication.is_null() && too_long.length == 0);

        setup.receive(3);
        assert_eq!(setup.received[1].body, b"c1");
        assert_eq!(setup.received[2].body, b"c2");

        // Three terms later the log is back in c1's partition, and the claim at c1's
        // term offset there has c1's body address: only the position tells the two
        // structs apart ("Which claim a struct names").
        let image = cavalry_subscription_image_at_index(setup.subscription, 0);
        assert!(!image.is_null(), "{}", errmsg());
        let mut image_constants = std::mem::zeroed::<cavalry_image_constants_t>();
        assert_eq!(cavalry_image_constants(image, &mut image_constants), 0);
        let term_length = 1i64 << image_constants.position_bits_to_shift;
        assert!(term_length >= 64 * 1024, "a term of {term_length} bytes");
        let mut constants = std::mem::zeroed::<cavalry_publication_constants_t>();
        assert_eq!(
            cavalry_publication_constants(setup.publication, &mut constants),
            0
        );
        // Frames of one size, 1,024 bytes, tile a term; 64-byte frames, c1's size, fill
        // to a 1,024-byte boundary after c2 and from the start of the fourth term to c1's
        // offset. c1's frame begins 64 bytes before the position after it.
        const LARGE: i64 = 1024;
        const HEADER: usize = 32;
        assert!(constants.max_payload_length >= LARGE as usize - HEADER);
        let target = after1 - 64 + 3 * term_length;
        let mut counted = Count {
            marker: b"c4",
            marked: 0,
        };
        let mut later = std::mem::zeroed::<cavalry_buffer_claim_t>();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        loop {
            let position = cavalry_publication_position(setup.publication);
            assert!(
                (0..=target).contains(&position),
                "position {position} of {target}: {}",
                errmsg()
            );
            let length = if position % LARGE == 0 && target - position >= LARGE {
                LARGE as usize - HEADER
            } else {
                2
            };
            let after = cavalry_publication_try_claim(setup.publication, length, &mut later);
            if after < 0 {
                // Back-pressured until the subscriber reads: poll, and try again.
                assert_ne!(after, CAVALRY_PUBLICATION_ERROR, "{}", errmsg());
                assert!(
                    std::time::Instant::now() < deadline,
                    "the publication reached {position} of {target}"
                );
                assert!(setup.poll_counting(&mut counted) >= 0, "{}", errmsg());
                continue;
            }
            if later.data == copy.data {
                break;
            }
            if length == 2 {
                ptr::copy_nonoverlapping(b"--".as_ptr(), later.data, 2);
            }
            assert_eq!(cavalry_buffer_claim_commit(&mut later), after);
        }
        let after_later = later.position_after;
        assert_eq!(
            after_later,
            after1 + 3 * term_length,
            "c1's offset, three terms on"
        );
        assert_ne!(after_later, copy.position_after);
        let body = std::slice::from_raw_parts(later.data, 2).to_vec();
        for stale in [&mut c1, &mut cavalry_buffer_claim_t { ..copy }] {
            assert_eq!(cavalry_buffer_claim_commit(stale), -1);
            assert_eq!(cavalry_errcode(), CAVALRY_ERR_NO_CLAIM, "{}", errmsg());
            assert_eq!(cavalry_buffer_claim_abort(stale), -1);
            assert_eq!(cavalry_errcode(), CAVALRY_ERR_NO_CLAIM);
            assert_eq!(cavalry_buffer_claim_set_reserved_value(stale, 1), -1);
            assert_eq!(cavalry_errcode(), CAVALRY_ERR_NO_CLAIM);
        }
        assert_eq!(
            std::slice::from_raw_parts(later.data, 2),
            body.as_slice(),
            "the new claim's body is untouched"
        );
        ptr::copy_nonoverlapping(b"c4".as_ptr(), later.data, 2);
        assert_eq!(cavalry_buffer_claim_commit(&mut later), after_later);
        // The subscriber reads the new claim once, with the body its own struct wrote.
        while counted.marked == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "the subscriber read c4"
            );
            assert!(setup.poll_counting(&mut counted) >= 0, "{}", errmsg());
        }
        assert_eq!(setup.poll_counting(&mut counted), 0, "nothing after c4");
        assert_eq!(counted.marked, 1);
        let image = cavalry_subscription_image_at_index(setup.subscription, 0);
        assert_eq!(cavalry_image_position(image), after_later);
    }
}

/// What `count` keeps across polls: how many fragments had `marker` as their body.
struct Count {
    marker: &'static [u8],
    marked: usize,
}

unsafe extern "C" fn count(
    clientd: *mut c_void,
    buffer: *const u8,
    length: usize,
    _header: *const cavalry_header_t,
) {
    // SAFETY: the test passes a `Count`, and the body is valid for the call.
    unsafe {
        let count = &mut *clientd.cast::<Count>();
        if std::slice::from_raw_parts(buffer, length) == count.marker {
            count.marked += 1;
        }
    }
}

/// A handler that calls back into its own subscription: every such call is refused
/// with `CAVALRY_ERR_POLLING`, and a call on another handle is allowed.
unsafe extern "C" fn reentrant(
    clientd: *mut c_void,
    _buffer: *const u8,
    _length: usize,
    header: *const cavalry_header_t,
) {
    // SAFETY: the test passes the subscription handle and a counter.
    unsafe {
        let (subscription, seen) = &mut *clientd.cast::<(*mut cavalry_subscription_t, usize)>();
        *seen += 1;
        assert_eq!(
            cavalry_subscription_poll(*subscription, Some(reentrant), clientd, 1),
            -1
        );
        assert_eq!(cavalry_errcode(), CAVALRY_ERR_POLLING);
        assert_eq!(cavalry_subscription_image_count(*subscription), -1);
        assert_eq!(cavalry_errcode(), CAVALRY_ERR_POLLING);
        assert!(cavalry_subscription_image_at_index(*subscription, 0).is_null());
        assert_eq!(cavalry_errcode(), CAVALRY_ERR_POLLING);
        assert!(!cavalry_subscription_is_connected(*subscription));
        assert_eq!(cavalry_errcode(), CAVALRY_ERR_POLLING);
        assert_eq!(cavalry_subscription_close(*subscription), -1);
        assert_eq!(cavalry_errcode(), CAVALRY_ERR_POLLING);
        let mut fault = std::mem::zeroed::<cavalry_fault_t>();
        assert_eq!(cavalry_subscription_fault(*subscription, &mut fault), -1);
        // The constants answer from inside the handler, from the handle's copy.
        let mut constants = std::mem::zeroed::<cavalry_subscription_constants_t>();
        assert_eq!(
            cavalry_subscription_constants(*subscription, &mut constants),
            0
        );
        assert_eq!(constants.stream_id, 1001);
        assert!(constants.registration_id >= 0);
        assert_eq!(
            CStr::from_ptr(constants.channel).to_str().unwrap(),
            "cavalry:ipc"
        );
        let mut values = std::mem::zeroed::<cavalry_header_values_t>();
        assert_eq!(cavalry_header_values(header, &mut values), 0);
        assert_eq!(values.frame.stream_id, 1001);
    }
}

#[test]
fn a_running_poll_refuses_calls_on_its_own_subscription() {
    let mut setup = Setup::new("polling");
    setup.publish(b"warm");
    setup.receive(1);
    setup.publish(b"again");
    let mut state = (setup.subscription, 0usize);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while state.1 == 0 {
        // SAFETY: the ABI's conventions.
        let polled = unsafe {
            cavalry_subscription_poll(
                setup.subscription,
                Some(reentrant),
                (&mut state as *mut (_, _)).cast(),
                10,
            )
        };
        assert!(polled >= 0, "{}", errmsg());
        assert!(std::time::Instant::now() < deadline);
    }
    // The subscription is still open and polls again after the refused close.
    assert!(setup.poll(1) >= 0);
}

#[test]
fn the_version_check_and_the_strings_refuse_what_the_page_says() {
    let dir = TestDir::new("strings");
    let driver = Driver::start(dir.path(), dir.log_dir()).expect("the driver");
    // SAFETY: the ABI's conventions.
    unsafe {
        let mut context = ptr::null_mut();
        assert_eq!(
            cavalry_context_init_v(&mut context, CAVALRY_ABI_VERSION + 1),
            -1
        );
        assert_eq!(cavalry_errcode(), CAVALRY_ERR_ABI_VERSION);
        assert!(
            errmsg().contains("version 1 and this library is version 0"),
            "{}",
            errmsg()
        );
        assert!(context.is_null());
        assert_eq!(
            cavalry_context_init_v(ptr::null_mut(), CAVALRY_ABI_VERSION),
            -1
        );
        assert_eq!(cavalry_errcode(), CAVALRY_ERR_NULL);
        assert_eq!(cavalry_context_init_v(&mut context, CAVALRY_ABI_VERSION), 0);
        let before = CStr::from_ptr(cavalry_context_get_dir(context)).to_owned();
        assert_eq!(cavalry_context_set_dir(context, c"".as_ptr()), -1);
        assert_eq!(cavalry_errcode(), CAVALRY_ERR_EMPTY_DIR);
        assert_eq!(
            CStr::from_ptr(cavalry_context_get_dir(context)),
            before.as_c_str(),
            "the directory it had"
        );
        let dir_c = dir.c_string();
        assert_eq!(cavalry_context_set_dir(context, dir_c.as_ptr()), 0);
        assert_eq!(
            CStr::from_ptr(cavalry_context_get_dir(context)),
            dir_c.as_c_str()
        );
        let mut client = ptr::null_mut();
        assert_eq!(cavalry_init(&mut client, context), 0, "{}", errmsg());
        assert_eq!(CStr::from_ptr(cavalry_dir(client)), dir_c.as_c_str());
        assert!(cavalry_client_id(client) >= 0);
        // The context may close before the client: the client copied the directory.
        assert_eq!(cavalry_context_close(context), 0);
        let bad = [b'c', b'a', b'v', 0xff, b'r', b'y', 0];
        let mut publication = ptr::null_mut();
        assert_eq!(
            cavalry_add_publication(&mut publication, client, bad.as_ptr().cast(), 1),
            -1
        );
        assert_eq!(cavalry_errcode(), CAVALRY_ERR_NOT_UTF8);
        assert!(publication.is_null());
        let nowhere = c"cavalry:nowhere";
        assert_eq!(
            cavalry_add_publication(&mut publication, client, nowhere.as_ptr(), 1),
            -1
        );
        assert_eq!(
            cavalry_errcode(),
            CAVALRY_ERR_INVALID_CHANNEL,
            "{}",
            errmsg()
        );
        assert!(
            errmsg().ends_with("docs/reference/client-protocol.md#error-codes"),
            "{}",
            errmsg()
        );
        assert_eq!(cavalry_closed(client), 0);
        assert!(!cavalry_is_closed(client));
        assert_eq!(cavalry_close(client), 0);
        // A directory with no driver.
        let mut other = ptr::null_mut();
        assert_eq!(cavalry_context_init_v(&mut other, CAVALRY_ABI_VERSION), 0);
        let nowhere_dir = CString::new(dir.path().join("nowhere").to_str().unwrap()).unwrap();
        assert_eq!(cavalry_context_set_dir(other, nowhere_dir.as_ptr()), 0);
        assert_eq!(cavalry_init(&mut client, other), -1);
        assert_eq!(cavalry_errcode(), CAVALRY_ERR_NOT_FOUND);
        assert!(
            errmsg().contains("; see docs/reference/client-protocol.md#the-driver-directory"),
            "{}",
            errmsg()
        );
        assert_eq!(cavalry_context_close(other), 0);
        assert_eq!(cavalry_abi_version(), CAVALRY_ABI_VERSION);
        assert_eq!(cavalry_version_major(), 0);
        assert_eq!(
            CStr::from_ptr(cavalry_version_full()).to_str().unwrap(),
            env!("CARGO_PKG_VERSION")
        );
    }
    driver.close().expect("the driver closes");
}

#[test]
fn a_closed_client_answers_closed_on_every_handle() {
    let dir = TestDir::new("closed");
    let driver = Driver::start(dir.path(), dir.log_dir()).expect("the driver");
    let dir_c = dir.c_string();
    let channel = c"cavalry:ipc";
    // SAFETY: the ABI's conventions.
    unsafe {
        let mut context = ptr::null_mut();
        assert_eq!(cavalry_context_init_v(&mut context, CAVALRY_ABI_VERSION), 0);
        assert_eq!(cavalry_context_set_dir(context, dir_c.as_ptr()), 0);
        let mut client = ptr::null_mut();
        assert_eq!(cavalry_init(&mut client, context), 0, "{}", errmsg());
        let mut publication = ptr::null_mut();
        assert_eq!(
            cavalry_add_publication(&mut publication, client, channel.as_ptr(), 7),
            0
        );
        let mut subscription = ptr::null_mut();
        assert_eq!(
            cavalry_add_subscription(&mut subscription, client, channel.as_ptr(), 7),
            0
        );
        assert_eq!(cavalry_close(client), 0);
        assert!(cavalry_publication_is_closed(publication));
        assert!(cavalry_subscription_is_closed(subscription));
        let mut claim = std::mem::zeroed::<cavalry_buffer_claim_t>();
        assert_eq!(
            cavalry_publication_try_claim(publication, 1, &mut claim),
            CAVALRY_PUBLICATION_CLOSED
        );
        assert_eq!(
            cavalry_subscription_poll(subscription, Some(collect), ptr::null_mut(), 1),
            -1
        );
        assert_eq!(cavalry_errcode(), CAVALRY_ERR_FAULT_CLOSED);
        let mut fault = std::mem::zeroed::<cavalry_fault_t>();
        assert_eq!(cavalry_subscription_fault(subscription, &mut fault), 0);
        assert_eq!(fault.code, CAVALRY_ERR_FAULT_CLOSED);
        assert_eq!(fault.position, CAVALRY_NULL_POSITION);
        // The handles still need their own close, and a null handle is NULL.
        assert_eq!(cavalry_publication_close(publication), 0);
        assert_eq!(cavalry_subscription_close(subscription), 0);
        assert_eq!(cavalry_publication_close(ptr::null_mut()), -1);
        assert_eq!(cavalry_errcode(), CAVALRY_ERR_NULL);
        assert_eq!(cavalry_context_close(context), 0);
    }
    driver.close().expect("the driver closes");
}

#[test]
fn a_recording_and_a_replay_cross_the_abi() {
    let mut setup = Setup::new("replay");
    let channel = c"cavalry:ipc";
    // SAFETY: the ABI's conventions.
    unsafe {
        let mut recording = ptr::null_mut();
        assert_eq!(
            cavalry_start_recording(&mut recording, setup.client, channel.as_ptr(), 1001),
            0,
            "{}",
            errmsg()
        );
        let mut constants = std::mem::zeroed::<cavalry_recording_constants_t>();
        assert_eq!(cavalry_recording_constants(recording, &mut constants), 0);
        assert_eq!(constants.stream_id, 1001);
        let recording_id = constants.recording_id;
        let position = setup.publish(b"recorded");
        setup.receive(1);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while cavalry_recording_synced_position(recording) < position {
            assert!(std::time::Instant::now() < deadline, "the recording synced");
            std::thread::yield_now();
        }
        assert_eq!(
            cavalry_stop_recording(setup.client, recording_id),
            0,
            "{}",
            errmsg()
        );
        assert_eq!(cavalry_recording_close(recording), 0);
        let mut replay_sub = ptr::null_mut();
        assert_eq!(
            cavalry_add_subscription(&mut replay_sub, setup.client, channel.as_ptr(), 2001),
            0
        );
        let mut replay = ptr::null_mut();
        assert_eq!(
            cavalry_start_replay(
                &mut replay,
                setup.client,
                recording_id,
                CAVALRY_NULL_POSITION,
                CAVALRY_NULL_LENGTH,
                2001,
                channel.as_ptr()
            ),
            0,
            "{}",
            errmsg()
        );
        let mut replay_constants = std::mem::zeroed::<cavalry_replay_constants_t>();
        assert_eq!(cavalry_replay_constants(replay, &mut replay_constants), 0);
        assert_eq!(replay_constants.recording_id, recording_id);
        assert_eq!(replay_constants.stream_id, 2001);
        let mut received: Vec<Fragment> = Vec::new();
        let mut ended = (0i64, -1i32);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let polled = cavalry_subscription_poll(
                replay_sub,
                Some(collect),
                (&mut received as *mut Vec<_>).cast(),
                10,
            );
            assert!(polled >= 0, "{}", errmsg());
            if !received.is_empty() && cavalry_replay_ended(replay, &mut ended.0, &mut ended.1) {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "the replay ended");
        }
        assert_eq!(received[0].body, b"recorded");
        assert_eq!(ended.0, position);
        assert_eq!(ended.1, CAVALRY_REPLAY_STOP_ENDED);
        assert_eq!(cavalry_replay_close(replay), 0);
        assert_eq!(cavalry_subscription_close(replay_sub), 0);
    }
}
