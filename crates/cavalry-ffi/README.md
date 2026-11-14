# cavalry-ffi

The `extern "C"` ABI and its cbindgen header. Data plane, and the other crate where
`unsafe` is permitted.

## What this does

- Exposes the client API as a C ABI, `spec/c-abi.md` version 5, declared by the
  committed header `include/cavalry.h`, so one Rust core serves every language. The
  thin clients under `clients/` for Python, Node, Go, the JVM and .NET are generated
  over it and released together.
- Feeds a cluster's committed log to a service in C: `cavalry_committed_log` opens the
  log of the member on the client's driver, `cavalry_committed_log_poll` hands the
  fragment handler the entries a quorum has committed and none past the commit
  position, and `cavalry_committed_log_entry_position` gives the handler each entry's
  position in the log. Once `cavalry_committed_log_is_ended` returns `true`,
  `cavalry_committed_log_failed_at`, `cavalry_committed_log_withdrawn_at` and
  `cavalry_committed_log_faulted` say whether the handle ended short of what it was
  owed, and where: a service is never left to take a short end for a whole one, nor
  to remember what one poll returned (`spec/cluster.md`, "The service"; ADR 0070;
  ADR 0080). A handle reads one recording of the member's log, the one that holds
  the position it was opened at; after a whole end `cavalry_committed_log` at
  `cavalry_committed_log_position` gives the member's next recording, so a service
  in any language reads a log that is in several recordings from position 0, with
  no call added (ADR 0103).
- Holds a session with a cluster for a program in C: `cavalry_connect_cluster` asks
  the members in turn and returns once the leader has opened the session,
  `cavalry_cluster_session_offer` sends the leader a message of one frame with the
  result codes of a publication's offer, and `cavalry_cluster_session_poll` hands
  the fragment handler each reply's payload and is the session's duty cycle, called
  at least every 500 ms. The handler reads a reply's leadership term and the
  cluster's time with `cavalry_cluster_reply_values(header, values)`. The terms of
  the replies it is given never fall: a stale reply is passed over and counted, and
  `cavalry_cluster_session_stale_replies` reads the count.
  `cavalry_cluster_session_closed` returns null while the session is open, and why
  it is closed from then on
  (`spec/cluster.md`, "Sessions"; ADR 0121; ADR 0122).
- Runs a cluster's service for a program in C: `cavalry_cluster_service` makes one
  beside the member on the client's driver, and `cavalry_cluster_service_poll` hands
  the fragment handler each committed entry as what it is and is the service's duty
  cycle. The handler reads the entry with `cavalry_cluster_entry_values(header,
  values)`: its type, its session, its position, its term and the cluster's time,
  the one clock a service reads. A session message's payload is the handler's
  buffer. From inside the handler `cavalry_cluster_service_reply` answers a session
  and returns what became of the reply, and `cavalry_cluster_service_action` says
  whether the entry is taken: `CAVALRY_ACTION_ABORT` leaves it for the next poll,
  which is what the reply code `CAVALRY_CLUSTER_REPLY_RETRY` asks for. The service
  beside every member runs the same handler on the same entries, and the one beside
  the entry's leader answers. The poll holds the client it was called with: a
  function that takes that client is refused from inside the handler
  (`spec/cluster.md`, "The service" and "Replies"; ADR 0118; ADR 0123).
- Carries every non-Rust client's messages: a claim, a commit, an offer and a poll
  cross this boundary per message, a cluster session's and a cluster service's
  among them, which is why it is on the data plane.
- Is the one surface frozen between releases. Rust-to-Rust integration goes through
  crates with semver; everything else goes through this ABI.
- Builds three ways: `cdylib`, the shared library the generated clients load,
  `staticlib`, which `conformance/c/` links, and `rlib`, which the bench and this
  README's example call. The `harness` module is the bench's way in: the
  `extern "C"` functions the `ffi` rows call, behind safe methods, since `bench/` may
  hold no `unsafe`; it is not part of the ABI, and the header declares none of it. Its
  `refusal` turns a claim's result code into the message a run that stops on it
  prints: the `CAVALRY_PUBLICATION_*` define, what to do next, the thread's error
  after `CAVALRY_PUBLICATION_ERROR`, and the page's anchor.

In Aeron, this is the C client, which re-implements the Java one; here the Rust core is
the only implementation and the C ABI is a thin layer over it
([glossary](../../docs/glossary.md)).

## What it must never do

- Change the ABI without an accepted ADR. It is one of the three things that cannot be
  undone, with the wire format and the on-disk log format: a change to the page is a
  new version, a new ADR and new vectors, before any code, and `just lint-header`
  holds the committed header to what cbindgen writes from this crate.
- Carry an `unsafe` block without a `// SAFETY:` comment naming the lifetime or thread
  rule of `spec/c-abi.md` that makes it sound.
- Let a released buffer be reused silently. Buffer lifetimes across the ABI are by
  convention, not the borrow checker, so debug builds poison a committed or aborted
  claim's struct and a closed handle's memory, and a stale claim struct is refused by
  its position (`spec/c-abi.md`, "Which claim a struct names").
- Behave differently from the Rust client. Every function is the Rust API's as it
  stands, and every client passes the conformance suite in CI.
- Allocate, lock, make a syscall, await or panic per message (invariant 2), in the
  functions a message passes through: `cavalry_publication_try_claim`,
  `cavalry_publication_offer`, `cavalry_buffer_claim_commit`,
  `cavalry_buffer_claim_abort`, `cavalry_buffer_claim_set_reserved_value`,
  `cavalry_subscription_poll`, `cavalry_header_values`, `cavalry_committed_log_poll`,
  `cavalry_committed_log_entry_position`, `cavalry_cluster_session_offer`,
  `cavalry_cluster_session_poll`, `cavalry_cluster_reply_values`,
  `cavalry_cluster_service_poll`, `cavalry_cluster_entry_values`,
  `cavalry_cluster_service_reply` and `cavalry_cluster_service_action`. The
  thread-local error is a fixed buffer, and `tests/allocations.rs` counts across
  all sixteen. A service's poll sends a command to the driver at a session's
  opening, where its answering changes and at a recording's end, and none for a
  session message.
- Hand a function a header's values that the header does not carry. A header is of
  four kinds, by the poll that made it, and a function asked for what a kind lacks
  sets `CAVALRY_ERR_WRONG_HEADER` or `CAVALRY_ERR_NO_POSITION` and fills nothing
  (`spec/c-abi.md`, "What a header carries").
- Let two calls reach one Rust client at once. A cluster service's poll holds its
  client for the run of the handler, as the Rust borrow does, and every function
  that takes that client answers `CAVALRY_ERR_POLLING` from inside the handler
  (`spec/c-abi.md`, "Calls during a claim or a poll"; ADR 0123).

## Where the data plane ends

Invariant 2's per-message rules stop at the boundary in three parts (ADR 0046):

- **The Rust core behind the ABI** holds all five rules: every function above, from
  its call to its return, except the time inside the C fragment handler the poll was
  given.
- **The client's own code**, what a language's generator writes between the user's
  code and this header, holds them too: one call to the ABI per offer, claim, commit
  or abort and poll, on the caller's thread, with no lock, no syscall and no `await`
  around it; `Offer`'s codes returned as values, never raised; one view per
  publication and per subscription reused across messages. Each client's card
  records what it could not avoid.
- **The runtime's side**, the trampoline a runtime builds for each call and
  call-back, the objects it makes for the arguments, and its own step as a thread
  crosses the boundary, is the runtime's code and stays outside the rules and their
  gate; each client's runner reports its count beside its one-way latency.

## Example

One process publishes and subscribes through the ABI, from Rust, since the crate's
doctest has no C compiler: the same calls a C program makes through the header, in the
order a C program makes them. `cavalry run` in another terminal is the usual driver;
the doctest embeds one.

```rust
use std::ffi::{c_void, CStr, CString};
use std::ptr;

use cavalry_ffi::*;

// The C fragment handler: the body as a view into the log, and the frame's header.
unsafe extern "C" fn on_fragment(clientd: *mut c_void, buffer: *const u8, length: usize, header: *const cavalry_header_t) {
    let received = unsafe { &mut *clientd.cast::<Vec<(Vec<u8>, i32)>>() };
    let body = unsafe { std::slice::from_raw_parts(buffer, length) }.to_vec();
    let mut values = unsafe { std::mem::zeroed::<cavalry_header_values_t>() };
    unsafe { cavalry_header_values(header, &mut values) };
    received.push((body, values.frame.session_id));
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // A driver of our own for the doctest; `cavalry run` is the usual shape.
    let dir = std::env::temp_dir().join(format!("cavalry-ffi-readme-{}", std::process::id()));
    let log_dir = dir.with_extension("log");
    let driver = cavalry_driver::Driver::start(&dir, &log_dir)?;
    let dir_c = CString::new(dir.to_str().unwrap())?;
    let channel = CString::new("cavalry:ipc")?;

    unsafe {
        // The context checks the header's version first; a Rust caller has no macro,
        // so it passes the crate's own.
        let mut context = ptr::null_mut();
        assert_eq!(cavalry_context_init_v(&mut context, CAVALRY_ABI_VERSION), 0);
        assert_eq!(cavalry_context_set_dir(context, dir_c.as_ptr()), 0);
        let mut client = ptr::null_mut();
        assert_eq!(cavalry_init(&mut client, context), 0, "{}", CStr::from_ptr(cavalry_errmsg()).to_str()?);
        let mut subscription = ptr::null_mut();
        assert_eq!(cavalry_add_subscription(&mut subscription, client, channel.as_ptr(), 1001), 0);
        let mut publication = ptr::null_mut();
        assert_eq!(cavalry_add_publication(&mut publication, client, channel.as_ptr(), 1001), 0);

        // Publish by claim and commit: write in place, nothing copied, nothing allocated.
        // Until the driver has made the subscription's image, it answers not connected.
        // The subscription takes the image on a later poll, so a poll right after the
        // first accepted claim can deliver nothing; nothing is lost, and the loop below
        // polls until the message arrives. `cavalry_subscription_poll` says how to wait
        // for the image.
        let mut claim = std::mem::zeroed::<cavalry_buffer_claim_t>();
        let mut received: Vec<(Vec<u8>, i32)> = Vec::new();
        loop {
            let position = cavalry_publication_try_claim(publication, 5, &mut claim);
            if position >= 0 {
                ptr::copy_nonoverlapping(b"hello".as_ptr(), claim.data, 5);
                assert_eq!(cavalry_buffer_claim_commit(&mut claim), position);
                break;
            }
            assert!(position != CAVALRY_PUBLICATION_ERROR, "{}", CStr::from_ptr(cavalry_errmsg()).to_str()?);
            cavalry_subscription_poll(subscription, Some(on_fragment), (&mut received as *mut Vec<_>).cast(), 1);
        }

        // Subscribe by polling: each fragment reaches the handler with its header.
        while received.is_empty() {
            assert!(cavalry_subscription_poll(subscription, Some(on_fragment), (&mut received as *mut Vec<_>).cast(), 10) >= 0);
        }
        let (body, session) = &received[0];
        println!("received {:?} from session {session}", std::str::from_utf8(body)?);

        assert_eq!(cavalry_publication_close(publication), 0);
        assert_eq!(cavalry_subscription_close(subscription), 0);
        assert_eq!(cavalry_close(client), 0);
        assert_eq!(cavalry_context_close(context), 0);
    }
    driver.close()?;
    std::fs::remove_dir_all(&dir).ok();
    std::fs::remove_dir_all(&log_dir).ok();
    Ok(())
}
```

Running it prints one line, such as `received "hello" from session -1716223105`. A
program built against another version of the header is refused at its first call:

```text
cavalry_context_init refused (abi-version): the program was built against ABI version 4 and this library is version 5; rebuild the program against this library's header; see docs/reference/c-abi.md#versions-and-what-a-reader-rejects
```

A session with a cluster is the same calls in C, in this order. The block is
compiled and not run: it needs a cluster of three members on three hosts.
[Run a cluster](../../docs/how-to/run-a-cluster.md) runs one on one host, on the
loopback.

```no_run
use std::ffi::{CStr, CString, c_void};
use std::ptr;

use cavalry_ffi::*;

// The C fragment handler: a reply's payload, and its term and time from the header.
unsafe extern "C" fn on_reply(clientd: *mut c_void, buffer: *const u8, length: usize, header: *const cavalry_header_t) {
    let replies = unsafe { &mut *clientd.cast::<Vec<(Vec<u8>, i64)>>() };
    let mut values = unsafe { std::mem::zeroed::<cavalry_cluster_reply_values_t>() };
    unsafe { cavalry_cluster_reply_values(header, &mut values) };
    replies.push((unsafe { std::slice::from_raw_parts(buffer, length) }.to_vec(), values.leadership_term_id));
}

# fn main() -> Result<(), Box<dyn std::error::Error>> {
let members = CString::new("0,10.0.0.1:9010,10.0.0.1:9020,10.0.0.1:9030,10.0.0.1:9040,10.0.0.1:9050\
                           |1,10.0.0.2:9010,10.0.0.2:9020,10.0.0.2:9030,10.0.0.2:9040,10.0.0.2:9050\
                           |2,10.0.0.3:9010,10.0.0.3:9020,10.0.0.3:9030,10.0.0.3:9040,10.0.0.3:9050")?;
let response_channel = CString::new("cavalry:udp?endpoint=10.0.0.9:9100")?;
unsafe {
    let mut context = ptr::null_mut();
    assert_eq!(cavalry_context_init_v(&mut context, CAVALRY_ABI_VERSION), 0);
    let mut client = ptr::null_mut();
    assert_eq!(cavalry_init(&mut client, context), 0);
    let mut session = ptr::null_mut();
    let opened = cavalry_connect_cluster(&mut session, client, members.as_ptr(), response_channel.as_ptr());
    assert_eq!(opened, 0, "{}", CStr::from_ptr(cavalry_errmsg()).to_str()?);
    assert!(cavalry_cluster_session_offer(session, b"hello".as_ptr(), 5) > 0);
    // The session's duty cycle: the cluster's replies, and a keepalive when due.
    let mut replies: Vec<(Vec<u8>, i64)> = Vec::new();
    while cavalry_cluster_session_closed(session).is_null() && replies.is_empty() {
        assert!(cavalry_cluster_session_poll(session, Some(on_reply), (&mut replies as *mut Vec<_>).cast(), 10) >= 0);
    }
    assert_eq!(cavalry_cluster_session_close(session), 0);
    assert_eq!(cavalry_close(client), 0);
    assert_eq!(cavalry_context_close(context), 0);
}
# Ok(())
# }
```

A cluster's service is these calls, in this order. The block is compiled and not
run: it needs a cluster member on the client's driver.
[Run a cluster](../../docs/how-to/run-a-cluster.md) starts one.

```no_run
use std::ffi::{CStr, c_void};
use std::ptr;

use cavalry_ffi::*;

// What the handler is given as its `clientd`: the service it answers through, and
// the service's own state, which the service beside every member reaches alike.
struct Echo {
    service: *mut cavalry_cluster_service_t,
    total: u64,
}

// The C fragment handler: a committed entry, its values from the header, and a
// session message's payload as the buffer.
unsafe extern "C" fn on_entry(clientd: *mut c_void, buffer: *const u8, length: usize, header: *const cavalry_header_t) {
    let echo = unsafe { &mut *clientd.cast::<Echo>() };
    let mut values = unsafe { std::mem::zeroed::<cavalry_cluster_entry_values_t>() };
    unsafe { cavalry_cluster_entry_values(header, &mut values) };
    if values.r#type != CAVALRY_CLUSTER_ENTRY_SESSION_MESSAGE {
        return;
    }
    // Reply first, then change the state: an entry that is aborted is handed over again.
    let reply = unsafe { cavalry_cluster_service_reply(echo.service, values.session_id, buffer, length) };
    if reply == CAVALRY_CLUSTER_REPLY_RETRY {
        unsafe { cavalry_cluster_service_action(echo.service, CAVALRY_ACTION_ABORT) };
        return;
    }
    echo.total += length as u64;
}

# fn main() -> Result<(), Box<dyn std::error::Error>> {
unsafe {
    let mut context = ptr::null_mut();
    assert_eq!(cavalry_context_init_v(&mut context, CAVALRY_ABI_VERSION), 0);
    let mut client = ptr::null_mut();
    assert_eq!(cavalry_init(&mut client, context), 0);
    let mut service = ptr::null_mut();
    assert_eq!(cavalry_cluster_service(&mut service, client), 0);
    let mut echo = Echo { service, total: 0 };
    // The service's duty cycle: the entries a quorum committed, in the log's order.
    while echo.total < 5 {
        let taken = cavalry_cluster_service_poll(service, client, Some(on_entry), (&mut echo as *mut Echo).cast(), 10);
        assert!(taken >= 0, "{}", CStr::from_ptr(cavalry_errmsg()).to_str()?);
    }
    assert_eq!(cavalry_cluster_service_close(service, client), 0);
    assert_eq!(cavalry_close(client), 0);
    assert_eq!(cavalry_context_close(context), 0);
}
# Ok(())
# }
```

## What to read next

- [`spec/c-abi.md`](../../spec/c-abi.md): the ABI, every rule a C caller holds, and
  what a reader of the header rejects; `include/cavalry.h` is its artefact.
- [ADR 0051](../../docs/decisions/0051-expose-the-client-api-as-a-c-abi-with-aerons-names.md):
  why the ABI has this shape, and the alternatives that lost; [ADR 0046](../../docs/decisions/0046-build-the-c-abi-and-the-clients-fourth.md)
  for the slice and the boundary's three parts.
- [`crates/cavalry-client`](../cavalry-client/README.md): the Rust API this crate
  exposes, call for call.
