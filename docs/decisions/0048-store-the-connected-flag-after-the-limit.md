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

The conductor stores the flag in the same duty cycle as the limit: a 1 after the
limit's raise, never before it, and a 0 before the limit's lowering, never after it.

- **It rises in the limit pass, after the raise.** The commands pass no longer stores
  a 1: not when an image joins (`add_image`), not when a recording starts
  (`start_recording`), and not when the sender's reply says a status message connected
  a `cavalry:udp` publication. The limit pass stores the 1 when a subscriber has a
  position, a recording is live or, over UDP, a receiver answers, and the limit stored
  is past the consumer position, which the first raise after a connection leaves it.
  A publisher that loads the flag before the limit and reads 1 therefore reads a
  limit raised for that subscriber or recording with it. Waiting for `is_connected()`
  and then claiming loads them in that order, so card 187's two tests, and a program
  written the same way, never read `back pressured` from a limit at the join.
- **It holds while connected.** It stays 1 while the publication stays connected,
  since a subscriber that reaches the limit in a pass whose cleaning holds the raise
  back still has its position.
- **It falls where the last connecting fact goes.** The commands pass stores the 0 at
  the moment the last subscriber, receiver or recording goes, through one helper,
  `lower_connected`, called from `drop_subscription` (which runs on `remove
  subscription`, client close and client timeout), from the sender's disconnected
  reply, and from a recording's stop and its failed start. The limit pass of the same
  cycle lowers the limit to the consumer position after it. So from the moment the
  last of them goes, a publisher at the old limit or at the lowered one reads 0 and
  is told `not connected`: a claim loads the limit before the flag
  ("Claim, commit and abort", step 1), and both stores are release stores. The limit
  pass also stores the 0, before its lowering, when it finds nothing connected; that
  store is a backstop.

The limit's raise and lowering stay in the limit pass, where the limit's rule lives.

What a claim's `back pressured` then tells a publisher:

- **It comes only while something reads the stream.** A subscriber has a position, a
  recording is live or, over UDP, a receiver answers, as of the claim's load of the
  flag, since the 0 is stored at the moment the last of them goes.
- **It means the consumer position is a window behind, with two exceptions.** In a
  cycle whose cleaning holds the raise back, the subscriber is at the limit, not a
  window behind (the hold above). And a claim loads the limit before the flag, the
  order the fall needs, so a claim whose two loads straddle a raise reads the old
  limit with the 1 stored after the raise. It returns `back pressured` once, and the
  next claim reads the raised limit. One load order cannot close both the rise and
  the fall; closing the rise is in Revisit.

The page says the order and both exceptions.

## Alternatives considered

- **The client's claim returns `not connected` until the limit is above its
  position.** Lost: the flag and the limit would still disagree for part of a cycle,
  and the client would be reading around a lie the driver could not tell instead.
- **Raise the limit where the flag was set.** Lost: the limit's rule, a raise of at
  least window ÷ 8 and only after cleaning, lives in the limit pass, and a second copy
  of it in three places is three places for it to drift.
- **Leave it to the tests to wait on the limit.** Lost: a user's program reads the
  same two words, and the page's `back pressured` would be false for them too.
- **Lower the flag in the limit pass only, before its lowering.** One place would
  store both values, from the facts the limit pass computes. Lost: the 0 would wait
  for the last step of the cycle. For that part of a cycle a publisher that a slow
  subscriber held at the limit reads the old limit with a 1 after the last subscriber
  went, and its claim returns `back pressured` with no subscriber at all: card 187's
  false result, for a disconnect. One place for the flag serves invariant 6, and
  invariant 1 outranks it. The driver before this decision stored the 0 in the
  commands pass, and this decision keeps that, with the connected facts in one
  helper.

## Evidence

The two failures in Context, and seven of the conductor's unit tests:

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
- `a_disconnect_stores_the_flag_in_the_commands_pass_before_the_limit_lowers`: a slow
  subscriber holds the publisher at the limit, where its claim reads `back pressured`.
  Removing the last subscription, in the commands pass, stores 0 and leaves the limit
  where it was, and the publisher's claim at the old limit reads `not connected`. The
  limit pass then lowers the limit to the subscriber's last position with the flag
  still 0.
- `a_recording_stop_stores_the_flag_before_the_limit_lowers` and
  `a_recording_that_fails_to_start_stores_the_flag_before_the_limit_lowers`: the same
  for a recording that connected a publication alone, at the recorder's stop and at
  its refusal to start.
- `a_udp_disconnect_stores_the_flag_before_the_limit_lowers`: the same for the
  sender's disconnected reply on a `cavalry:udp` publication.

The four disconnect tests fail when the 0 waits for the limit pass, as it did in this
decision's first form: each fails at its assertion that the flag is 0 before the limit
pass.

Nothing a message passes through changes: the publisher's claim reads the same two
words, and the conductor stores the flag at another step of its cycle. `just bench`
before and after is in the commit message.

## Consequences

Easier: a publisher's `back pressured` comes only while a subscriber, a recording or
a receiver reads the stream, and a publisher that waits for `is_connected()` before it
claims reads the raised limit. One place stores the 1, the limit pass, where three
places did, and one helper stores the 0 where the last connecting fact goes.

Harder: the flag follows the limit, so when a subscriber joins a publication whose
cleaning is behind, the publisher reads the flag a cycle or two after the subscriber's
position exists, and a claim at the limit reads `not connected` until then; the page
says so. A new site that removes a subscriber, a receiver or a recording must call
`lower_connected`; the limit pass's 0 is a backstop, at the end of the cycle. Once 1,
the flag holds while the publication stays connected, so a subscriber that reads up to
the limit in a pass whose cleaning stops at a term's end holds the raise back for that
cycle, and a claim at the limit reads `back pressured` with the subscriber at the limit
rather than a window behind, as it did before this decision. A claim whose two loads
straddle a raise reads `back pressured` once, with no subscriber a window behind. The
page says both. `Offer::BackPressured` in `cavalry-client` and `ClaimError::BackPressured`
in `cavalry-shm`, with their error messages, still say only "the slowest subscriber is
a window behind", which has the same two exceptions.

Revisit: when a client needs a counter for the flag's transitions over IPC, as
`publication.connected` counts them over UDP (ADR 0019's Revisit). And when a claim's
one `back pressured` across a raise matters to a user: on the refusal path only, when
the flag reads 1, the claim loads the limit again after the flag and retries if it
moved. That changes "Claim, commit and abort", step 1, `cavalry-shm` and the client,
with its own ADR.

## References

- `spec/log-buffer.md`, "Claim, commit and abort" and "The driver"; ADR 0013,
  ADR 0022, ADR 0041
- `crates/cavalry-driver/src/conductor.rs`, `keep_publication`
- AppShapes Trello card 187; pull request 91's `ci` run 41246978415; `master`'s run at
  c4e2489
