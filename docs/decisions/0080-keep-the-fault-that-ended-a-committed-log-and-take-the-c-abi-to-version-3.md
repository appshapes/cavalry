# 0080. Keep the fault that ended a committed log, and take the C ABI to version 3

- **Status:** Accepted
- **Date:** 2026-11-11
- **Invariants affected:** 1, 6
- **Supersedes:** none. Amends `spec/c-abi.md` to version 3 with one function in the
  "committed log" group; every declaration of version 2 is unchanged (ADR 0070)

## Context

A committed log's handle ends in one of four ways (`CommittedLog::is_ended`, ADR 0066):
its member stopped naming the recording and the polls read to the bound; the replay
ended and the polls read to its end; the driver withdrew the replay's image; or the
replay's image faulted. An image faults when a frame of it fails a check, when it
cannot join because the subscription already holds 256 images, or, since ADR 0078,
when its log was gone before the client's conductor read its announcement, the reason
`gone`.

Two of the short ends have a report that lasts as long as the handle: `failed_at` and
`withdrawn_at`, each a position. The fault has none. The poll that meets it returns it
once, `Err(Fault)` in Rust and −1 with `CAVALRY_ERR_FAULT_IMAGE` or
`CAVALRY_ERR_FAULT_IMAGES_FULL` in the C ABI, `cavalry_committed_log_fault` answers
it until the next poll, and the next poll returns 0 and clears it. `withdrawn_at` is
`None` for such a log, and so is `failed_at` unless the driver's replay failed as
well. `spec/c-abi.md` says so, "is reported once ... so a service keeps that answer",
and so do the five clients' READMEs.

So an ended handle delivered every entry it was owed only when three things hold, and
the third is the caller's memory: no poll of this handle ever returned that fault. A
service that did not keep the answer cannot ask again, and reads a short end as a
whole one. That is invariant 1 resting on a convention. The first review of pull
request 194 (card 227, the ABI's version 2) named it, not blocking, and card 259
holds it.

`spec/c-abi.md` says that a change to any declaration "is a new version of this page,
a new ADR and new vectors, before any code". Nothing is released (ADR 0003), so the
number is free to move.

## Decision

A committed log keeps the fault of its own image for the life of the handle, and a
third report reads it.

In Rust, `CommittedLog::faulted()` returns `Option<Fault>`: `None` for a log whose
image has not faulted, and from the poll that returns the fault on, that `Fault`,
`Fault::Image` with its session id, its position and its reason or
`Fault::ImagesFull` with its session id. It is the fault that poll returned, and it
does not change or clear. A fault of another session's image on the stream is not
this log's and is not kept, as it is not returned. `Fault::Closed` is the client's
state, which `Cavalry::closed` reads, and is not kept either.

In the C ABI, version 3 adds one function beside the two reports:
`bool cavalry_committed_log_faulted(const cavalry_committed_log_t *log,
cavalry_fault_t *fault)`. It returns `true` with the fault written through the
pointer, as `cavalry_committed_log_fault` wrote it for the poll that met it: the code,
`CAVALRY_ERR_FAULT_IMAGE` or `CAVALRY_ERR_FAULT_IMAGES_FULL`, the image's session id,
and the position, `CAVALRY_NULL_POSITION` where the fault has none. It returns `false`
with nothing written for a log whose image has not faulted. It cannot answer for a
null argument, `CAVALRY_ERR_NULL`, or from inside the log's own handler,
`CAVALRY_ERR_POLLING`, and is then `false` with the error set, as the two reports
are. `CAVALRY_ABI_VERSION` is 3.

What a poll does is unchanged, under every name version 2 has: the poll that meets
the fault returns it once, `cavalry_committed_log_fault` answers it until the next
poll, and later polls return 0.

The end of a log is then one rule with no memory in it. Once `is_ended` is true, the
handle delivered every entry it was owed when `failed_at`, `withdrawn_at` and
`faulted` all report nothing. In every case what it delivered ends at `position`.

Each of the five generated clients gains the read on its committed log object when
its package is regenerated from the header, and refuses a library of version 2:
`faulted()` in Python, which returns `(code, session_id, position)` or `None`;
`faulted()` in Node, an object or `null`; `Faulted()` in Go, the three values and a
`bool`; `Faulted()` in .NET, a `Subscription.Fault?`; and `faulted()` on the JVM, an
`Optional<Subscription.Fault>`.

## Alternatives considered

- **`cavalry_committed_log_fault` keeps the log's own fault and no poll clears it.**
  Lost. The function is the subscription's under the log's name, "the fault its last
  poll stopped on", and a poll that returns a count clears it. Keeping one kind of
  fault and clearing the others makes one function two things, and it changes what a
  version 2 name answers, which the page forbids: "never with a different behaviour
  under the old names".
- **Every poll after the fault returns −1 again.** Lost for the same reason, and for
  what it does to a service's loop. A poll of an ended log returns 0 for each of the
  other three ends, and a service stops on `is_ended`, not on a count. A poll that
  fails for ever would make the fourth end the one that a loop written for the other
  three spins on.
- **`failed_at` reports the image's fault too, at the fault's position.** Lost. The
  two are different failures with different remedies: `failed_at` is the driver's
  replay of the member's recording, a file on the member's disk, and the fault is
  this client's read of the replay's image. An image that could not join, or that
  was `gone`, has no position to report.
- **One read that says whether the end was whole, in place of three reports.** Lost
  for now. The two reports exist, and each tells an operator a different thing to
  look at. A read that combines the three is a line of the caller's code once the
  third can be read at all, and it can be added without changing these.
- **Say it in the documents again, more plainly.** Lost. The page and five READMEs
  already say that a service keeps the poll's answer. Invariant 1 does not rest on a
  sentence where a read can hold it.

## Evidence

Before this change no test at any layer made a committed log's own image fault: the
two places in `CommittedLog::poll` that marked the handle as faulted ran in no test.
The tests below make the fault the same way. A log's replay plays into a log buffer
of its own in the member's driver directory, the one file that appears under
`publications/` when the log is opened, and the test changes one byte of a frame the
replay wrote there, its version, to one no reader accepts (`spec/frame.md`, "What a
reader rejects"), before the service's poll reaches it. A log's reader reports any
frame that fails a check under the reason `frame`.

- Rust, `a_service_whose_image_faulted_reads_the_fault_for_the_life_of_its_handle`
  in `crates/cavalry-raft/tests/cluster.rs`, over three members. A service reads
  five of eleven committed entries, to position 480, and the sixth's frame is
  changed. The next poll returns `Fault::Image` with the replay's session id,
  position 480 and the reason `frame`. `faulted()` was `None` before it and is that
  fault after it and after each of fifty more polls, each of which returns 0. The
  handle is ended, `failed_at()` and `withdrawn_at()` are `None`, its position is
  480, and its client is open. A log opened again at 480 reads the six entries the
  first was not given and five the leader appends then, with no fault.
- C, the test of the same name in `crates/cavalry-ffi/tests/committed_log.rs`. The
  poll that meets the frame returns −1 with `CAVALRY_ERR_FAULT_IMAGE`, and
  `cavalry_committed_log_fault` answers code 120 at position 480.
  `cavalry_committed_log_faulted` writes the same code, session id and position
  after each of fifty more polls, each of which returns 0 and clears what
  `cavalry_committed_log_fault` answers, and it leaves the thread's error as the
  call before left it. Before the fault it returns `false` and writes nothing. A
  null log or a null `fault` is `false` with `CAVALRY_ERR_NULL`, and a call from
  inside the log's own handler is `false` with `CAVALRY_ERR_POLLING`, each with
  nothing written.
- The five clients, in `conformance/python`, `node`, `go`, `jvm` and `dotnet`. Each
  committed log round changes the frame at position 0 of a log it has not polled,
  and reads code 120 at position 0 from the poll that meets it and the same code,
  session id and position from `faulted()` after each of twenty more polls. Each
  round also makes the call from inside the log's own handler, the ninth it
  refuses there, and reads a live log's answer, which is none.
  `clients/generate-test.py` reads each wrapper.
- `crates/cavalry-ffi/tests/conformance.rs` holds the committed header to
  `spec/vectors/c-abi.json`: 72 functions, and `CAVALRY_ABI_VERSION` 3.

## Consequences

The ABI is at version 3, and a generated client's package of version 2 refuses a
version 3 library when it loads until it is regenerated; the five are regenerated in
the same change, since `just lint` holds them to the header. Nothing is released, so
no built program meets the refusal.

The handle holds one `Option<Fault>` where it held a flag: no allocation, and nothing
on the path of an entry.

The page, the Rust rustdoc and the five READMEs say the end of a log in the new
words: three reports, read once `is_ended` is true.

Card 228 means to make a member's log more than one recording, so that a service's
replay ends at a recording's end and goes on in the next. That changes how a handle
ends and may change what `failed_at` and `withdrawn_at` mean; the fault is the
client's read of an image and is the same there. Revisit this decision if a service
then holds one handle over several replays, since "the life of the handle" would
span more than one image.

## References

- `spec/c-abi.md`, "Functions" (the committed log), "When a function cannot answer",
  "Calls during a claim or a poll", "Versions and what a reader rejects";
  `spec/vectors/c-abi.py` and `c-abi.json`.
- `crates/cavalry-client/src/committed_log.rs`, `CommittedLog::faulted`;
  `crates/cavalry-ffi/src/committed_log.rs`, `cavalry_committed_log_faulted`.
- ADR 0066 (the committed log), ADR 0070 (the ABI's version 2), ADR 0078 (the reason
  `gone`), ADR 0046 (the generated clients).
- Card 259; the first review of pull request 194, note 2.
