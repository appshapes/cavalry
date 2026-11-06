# 0057. Take the C ABI to version 1 with add and remove destination

- **Status:** Accepted
- **Date:** 2026-11-06
- **Invariants affected:** 3, 6
- **Supersedes:** none. Amends `spec/c-abi.md` to version 1 with two functions in its
  "adds and commands" group and `CAVALRY_ABI_VERSION` 1; every declaration of version
  0 is unchanged (ADR 0051; ADR 0053)

## Context

ADR 0053 gives a publication on a control channel its destinations through the
client protocol's `add destination` and `remove destination`, and leaves their client
API, their ABI functions and the ABI's version bump to the card's later commit (ADR
0052, feature 1: "a version bump of the ABI before the first release, which ADR 0051
leaves open"). `spec/c-abi.md` version 0 lists destinations under "Out of this
version", and its rule is that a change to any declaration "is a new version of this
page, a new ADR and new vectors, before any code", a program built against another
version refused at its first call by `cavalry_context_init_v`. Nothing is released
(ADR 0003), so no built program meets the refusal; the first release is what freezes a
version. The Rust client's commands that name a driver-side thing by its id,
`stop_recording(id)` and `stop_replay(id)`, cross the ABI as `cavalry_stop_recording(client,
id)` and `cavalry_stop_replay(client, id)`, each `int` with the thread's error set on
refusal, through one helper that checks the client handle and runs the call. Aeron's
C client adds a destination through the client with the publication's handle,
`aeron_publication_async_add_destination(aeron, publication, uri)`, and its Java client
through the publication, `Publication.addDestination(String)`; both name the
destination by its channel string, and a remove names the string again (read
2026-11-06 at `master`, 1.55.0-SNAPSHOT).

## Decision

Version 1 of the page adds two functions to "adds and commands", `int
cavalry_add_destination(cavalry_t *client, const cavalry_publication_t *publication,
const char *channel)` and `int cavalry_remove_destination(cavalry_t *client, const
cavalry_publication_t *publication, const char *channel)`, each 0 on `operation
success` and −1 with the thread's error set on the driver's refusal, the code passing
through on its value as every driver code does, and `CAVALRY_ABI_VERSION` moves to 1
with `cavalry_abi_version()`. The functions take the client, which owns the command
ring and the answers, and read the publication handle's registration id, which the
handle copied when it was made, so they are allowed during a claim as
`cavalry_publication_constants` is and reach nothing the claim borrows. For the same
reason the page's "Threads" counts each as a call on the client and not on the
publication it names, since no function of the publication writes the copied id:
either may run in one thread beside any call on the publication in another, an offer
or a claim among them, but not beside the publication's close, which frees the
handle. Aeron's usual pattern, one thread offering while another adds a destination,
is then defined. The Rust client gains `Cavalry::add_destination(registration_id,
channel)` and `Cavalry::remove_destination(registration_id, channel)` beside
`stop_recording`, `Publication::registration_id()` giving the id. The five generated
clients gain the
method on their client object, `add_destination(publication, channel)` in Python's
spelling and each language's own, bound as every function of the header is, and a
package regenerated from version 1's header refuses a version 0 library when it loads,
as the clients README says. The header is what cbindgen writes from `cavalry-ffi`, the
vectors hold the header to the page, and the C runner sends one stream to two
destinations.

## Alternatives considered

- **The functions on the publication, `cavalry_publication_add_destination(publication,
  channel)`, as Aeron's Java.** Lost: the answer comes through the client's
  response path, which a publication handle does not hold, and the Rust client's
  commands that name a thing by its id already take the client; Aeron's C takes both.
- **A new ABI version only at the first release, the functions added under version 0
  until then.** Lost: the page's rule is that a changed declaration is a new version,
  and a program built against version 0's header that calls a function the library
  lacks would fail at its link, not at its first call with a message; the number is
  free to move until the release freezes it (ADR 0051).
- **The Rust API taking `&Publication`.** Lost: the ABI reads the handle's copied id,
  and the Rust method that takes the id serves both, as `stop_recording(id)` does.
- **A destination call counting as a call on the publication as well as on the
  client, as `cavalry_init` counts on its context.** Lost: the call reads only the
  copy that no function of the publication writes, so the rule would forbid what is
  safe, and a program that offers in one thread while another adds a destination
  would need a lock of its own around every offer, on the message's path.

## Evidence

None measured: two declarations and a number. What informed it: the page's own
version rule and `CAVALRY_ERR_ABI_VERSION`; the ABI's `command` helper; Aeron's C and
Java clients, read on 2026-11-06. The change that accepts this ADR carries the page
at version 1, the vectors with the two functions and the version, the header
regenerated, the functions in `cavalry-ffi` with their test, both called during a
claim and with each argument null in `tests/abi.rs`, the Rust client's two methods
with a test over the public API, `tests/destinations.rs`, the five generators and
their regenerated packages, and the C runner's two-destination round. `just
conformance` is the acceptance. On the laptop, and on the CI runner the review's
fixes ran on, the Rust suites and the C, Python, Node and Go runners passed, and the
JVM runner stopped at the machine's Java, a missing runtime on the laptop and `javac`
17 on the runner, so the .NET runner after it did not run on either. CI's `gates (linux)` and `gates
(macos)` jobs ran all six runners, the JVM's on Java 25 and the .NET one on .NET 10,
each to `ok` against ABI version 1, and are the acceptance for those two.

## Consequences

Easier: a client in any of the six languages gives a publication its destinations.

Harder: the ABI's version is 1 before anything is released, so a package generated
before this change refuses the library until it is regenerated, which its message
says; every later function moves the number again until the first release.

Revisit: at the first release, which freezes the version (ADR 0003, ADR 0051); when
a client needs the destinations of a subscription, Aeron's multi-destination
subscription, under ADR 0053's Revisit.

## References

- `spec/c-abi.md`, "Functions", "Constants", "Calls during a claim or a poll",
  "Threads", "Versions and what a reader rejects", "Out of this version";
  `spec/README.md`
- ADR 0046, ADR 0051, ADR 0052, ADR 0053
- `crates/cavalry-ffi/src/client.rs`, `cavalry_add_destination`,
  `cavalry_remove_destination`; `crates/cavalry-client/src/client.rs`,
  `add_destination`, `remove_destination`; `clients/generate*.py`;
  `conformance/c/quickstart.c`
- Aeron, read 2026-11-06 at `master` (1.55.0-SNAPSHOT):
  `aeron-client/src/main/c/aeronc.h`,
  `aeron-client/src/main/java/io/aeron/Publication.java`
- AppShapes Trello card 225
