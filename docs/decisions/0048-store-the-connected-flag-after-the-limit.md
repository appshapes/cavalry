# 0048. Raise a publication's connected flag after its limit and lower it before, in the same duty cycle

- **Status:** Accepted
- **Date:** 2026-11-05
- **Invariants affected:** 1, 4
- **Supersedes:** none. Amends `spec/log-buffer.md`, "The driver", within version 1
  with an order the page did not state: the connected flag rises after the limit's
  raise and falls before its lowering

## Context

A publication's log header carries two words the publisher reads on every claim: the
limit, `publication.limit`, and the flag is connected (`spec/log-buffer.md`, "Claim,
commit and abort", step 1). A claim at or past the limit fails with `back pressured`
when the flag is 1 and with `not connected` when it is 0, and the client's
`Offer::BackPressured` says what that means: the slowest subscriber is a window behind.

The driver's conductor set the flag where the thing that connects a publication
happened, in the pass that reads commands: `add_image` when an image joined,
`start_recording` when a recording started, and the sender's reply when a status
message connected a `cavalry:udp` publication; and it raised the limit in its limit
pass, `keep_publications`, the last step of the same duty cycle. Between the two a
publisher read connected and a limit still at its position, and its claim returned
`back pressured` with no subscriber behind at all. With the backoff idle strategy on a
loaded shared runner that part of a cycle is long enough for a test to land in it: card
187 saw `claim_writes_in_place_and_too_long_is_refused_before_anything_is_reserved` in
`crates/cavalry-client/tests/publication.rs` wait for `is_connected()`, claim at
position 0 and get `BackPressured` on the macOS gate of pull request 91 on 2026-10-31,
and `offers: BackPressured` at `crates/cavalry-client/tests/udp.rs:253` on the macOS
gate of `master`'s run at c4e2489 on 2026-11-04. The page said what the flag means, 1
"while a subscriber has a position or a recording is live", and nothing about its order
against the limit: unspecified behaviour, a bug in the spec before it is one in the
code (`CLAUDE.md`, Protocol).

## Decision

The conductor stores the flag in its limit pass, in the same duty cycle as the limit:
a 1 after the limit's raise, never before it, and a 0 before the limit's lowering,
never after it.

- **It rises after the raise.** The flag rises to 1 when a subscriber has a position
  or a recording is live and the limit stored is past the consumer position, which the
  first raise after a connection leaves it. A publisher that reads 1 therefore reads a
  limit raised for that subscriber or recording with it, so `back pressured` means
  what the client says it means.
- **It holds while connected.** It stays 1 while the publication stays connected,
  since a subscriber that reaches the limit in a pass whose cleaning holds the raise
  back still has its position.
- **It falls before the lowering.** It falls to 0 when neither a subscriber has a
  position nor a recording is live, and the limit lowers to the consumer position
  after it. A claim loads the limit before the flag ("Claim, commit and abort", step
  1), and both stores are release stores, so a publisher that reads the lowered limit
  reads 0 with it and is told `not connected`. The reverse order would tell a
  publisher between the consumer position and the old limit `back pressured` with no
  subscriber: card 187's false result in the other direction.

The commands pass, the start and stop of a recording and the sender's replies no
longer touch the flag; they change what the limit pass computes from. The page says
so.

## Alternatives considered

- **The client's claim returns `not connected` until the limit is above its
  position.** Lost: the flag and the limit would still disagree for part of a cycle,
  and the client would be reading around a lie the driver could not tell instead.
- **Raise the limit where the flag was set.** Lost: the limit's rule, a raise of at
  least window ÷ 8 and only after cleaning, lives in the limit pass, and a second copy
  of it in three places is three places for it to drift.
- **Leave it to the tests to wait on the limit.** Lost: a user's program reads the
  same two words, and the page's `back pressured` would be false for them too.
- **Lower the flag in the commands pass, where the subscriber or recording goes.** The
  driver did this before this decision, in `drop_subscription`, the sender's reply and
  a recording's stop, and it is a truthful order too. Lost: it keeps a copy of the
  connected facts at each of those sites, the places this decision makes one, and the
  limit pass stores the 0 in the same cycle, before the lowering, where the publisher
  reads the two words in order.

## Evidence

The two failures in Context, and four of the conductor's unit tests:

- `the_connected_flag_is_stored_after_the_limit_in_the_same_cycle`: after an image
  joins in the commands pass the publication is not connected and its limit has not
  moved, and the limit pass raises the limit by the window and then sets the flag.
- `a_recording_connects_a_publication_in_the_limit_pass`: the same for a recording
  that connects a publication alone.
- `the_connected_flag_holds_while_cleaning_holds_the_raise_back`: on a 64 KiB term the
  subscriber reads up to a limit of 72 KiB and cleaning stops at the first term's end.
  The pass that holds the raise back leaves the flag 1 and the publisher's claim
  `back pressured`, and the next pass raises the limit with the flag still 1. The
  rule this decision first had, 1 only while the limit is past the consumer position,
  fails it.
- `a_disconnect_stores_the_flag_then_lowers_the_limit_in_the_limit_pass`: the
  commands pass that removes the last subscription moves neither word, and the limit
  pass leaves the flag 0 and the limit at the subscriber's last position, below the
  publisher, whose claim reads `not connected`. The order of the two stores inside the
  pass is the code's; a test on one thread sees both words only after the pass.

Nothing a message passes through changes: the publisher's claim reads the same two
words, and the conductor stores the flag at another step of its cycle. `just bench`
before and after is in the commit message.

## Consequences

Easier: a publisher's `back pressured` is true whenever it reads it; one place sets
the flag, from what the limit pass computes, where four places did.

Harder: the flag follows the limit, so when a subscriber joins a publication whose
cleaning is behind, the publisher reads the flag a cycle or two after the subscriber's
position exists, and a claim at the limit reads `not connected` until then; the page
says so. A disconnect's 0 waits for the limit pass of the same cycle, where the driver
before this decision stored it in the commands pass. Once 1, the flag holds while the
publication stays connected, so a subscriber that reads up to the limit in a pass whose
cleaning stops at a term's end holds the raise back for that cycle, and a claim at the
limit reads `back pressured` with the subscriber at the limit rather than a window
behind, as it did before this decision; the page says that too.

Revisit: when a client needs a counter for the flag's transitions over IPC, as
`publication.connected` counts them over UDP (ADR 0019's Revisit).

## References

- `spec/log-buffer.md`, "Claim, commit and abort" and "The driver"; ADR 0013,
  ADR 0022, ADR 0041
- `crates/cavalry-driver/src/conductor.rs`, `keep_publication`
- AppShapes Trello card 187; pull request 91's `ci` run 41246978415; `master`'s run at
  c4e2489
