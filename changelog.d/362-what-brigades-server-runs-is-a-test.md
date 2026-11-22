- What Brigade's server runs on an embedded driver is a test of `just test`. Two test
  programs, `crates/cavalry/tests/feed_shape.rs` and
  `crates/cavalry/tests/feed_held_still.rs`, run the server's own use of Cavalry, call
  for call, in the shape it is pinned to and in the one it moves to, `ssc=true` with
  `term-length=1m`: the second holds a feed process still for 12 s and then kills it.
  A change that breaks a call the server makes fails there. A new page lists those
  calls and what the server relies on in each, and a how-to has the steps for a change
  to one of them. The page also names two things that do not hold: a `start
  recording` sent beside an earlier client that the driver has not timed out yet,
  which is work item 372, and the server's own read of a recording that ends in
  padding
  ([What does Brigade's server rely on?](docs/explanation/what-brigades-server-relies-on.md);
  [Change a call that Brigade's server makes](docs/how-to/change-a-call-that-brigades-server-makes.md);
  [ADR 0138](docs/decisions/0138-draw-the-finish-line-at-a-beta-that-is-an-alternative-to-aeron-and-build-it-sixth.md),
  condition 9; work item 362).
