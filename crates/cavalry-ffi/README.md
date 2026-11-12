# cavalry-ffi

The `extern "C"` ABI and its cbindgen header. Data plane, and the other crate where
`unsafe` is permitted.

## What this does

- Exposes the client API as a C ABI, `spec/c-abi.md` version 2, declared by the
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
  ADR 0080). The read below the member's live recording, which `spec/c-abi.md`'s
  row for the committed log describes, is not built yet (ADR 0103): until
  `cavalry-client` builds it, `cavalry_committed_log` gives no null handle for a
  member between two recordings, ends no handle at the stop of a recording below
  the live one, and reads no file, so it has no error of a file to give.
- Carries every non-Rust client's messages: a claim, a commit, an offer and a poll
  cross this boundary per message, which is why it is on the data plane.
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
  `cavalry_subscription_poll`, `cavalry_header_values`, `cavalry_committed_log_poll`
  and `cavalry_committed_log_entry_position`. The thread-local error is a fixed
  buffer, and `tests/allocations.rs` counts across all nine.

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
cavalry_context_init refused (abi-version): the program was built against ABI version 1 and this library is version 2; rebuild the program against this library's header; see docs/reference/c-abi.md#versions-and-what-a-reader-rejects
```

## What to read next

- [`spec/c-abi.md`](../../spec/c-abi.md): the ABI, every rule a C caller holds, and
  what a reader of the header rejects; `include/cavalry.h` is its artefact.
- [ADR 0051](../../docs/decisions/0051-expose-the-client-api-as-a-c-abi-with-aerons-names.md):
  why the ABI has this shape, and the alternatives that lost; [ADR 0046](../../docs/decisions/0046-build-the-c-abi-and-the-clients-fourth.md)
  for the slice and the boundary's three parts.
- [`crates/cavalry-client`](../cavalry-client/README.md): the Rust API this crate
  exposes, call for call.
