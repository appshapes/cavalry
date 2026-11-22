# What does Brigade's server rely on?

This page answers one question: which calls and behaviours of Cavalry does Brigade's
server rely on, so that a change to one of them is made knowingly. Brigade is a
messaging service for coding sessions, `github.com/appshapes/brigade`. Its server is a
Rust program that embeds a Cavalry driver. It takes Cavalry as a git dependency pinned
at commit 620a2e29, and two teams' servers run that build in production, each on a log
directory it keeps
([ADR 0138](../decisions/0138-draw-the-finish-line-at-a-beta-that-is-an-alternative-to-aeron-and-build-it-sixth.md),
condition 9). The server's code was read for work item 362 on 2026-11-21, at Brigade's
`master`, commit 5ddacc75: the file `server/crates/brigade-log/src/feed.rs`.

Four terms of this page. The *feed* is the server's ordered log: one recorded
publication on one stream, in a driver that the server's own process runs. A *record*
is one message of the feed. A record's *position* is the feed's position after the
record's last frame. The server's *snapshot* is a file of its own, which holds its
state as of one position of the feed; it is not the snapshot of a cluster member.
Cavalry's own terms on this page, among them exclusive publication, term,
destination, recording, catalog, replay and image, are in the
[glossary](../glossary.md).

## Answer

- The server uses 11 methods of `Cavalry`, 3 of `Driver`, the handles they return and
  the catalog's reader. The table below lists them with what the server relies on in
  each.
- `crates/cavalry/tests/feed/mod.rs` is the server's use of those calls, copied call
  for call. Two test programs run it in `just test`, in both shapes of the feed:
  `crates/cavalry/tests/feed_shape.rs`, and `crates/cavalry/tests/feed_held_still.rs`
  for what needs a process to stop. A change that breaks a call the server makes fails
  there, in the pull request that makes the change.
- A log directory that the server's pinned tree wrote opens under this tree in place:
  with no step by an operator, with every record read back at its position.
  `conformance/upgrade/log-directory.sh` holds that in `just conformance`.
- Two things the server's code expects do not hold. They are under "What does not
  hold". The first is work item 372 on the Brigade Board. The second is the server's
  to change.
- A change to a call in the table is either kept compatible or made on purpose.
  [Change a call that Brigade's server makes](../how-to/change-a-call-that-brigades-server-makes.md)
  has the steps.

## How the server keeps its feed

1. At its start the server starts a driver on its driver directory and its log
   directory, and connects one client to it.
2. It reads the log directory's catalog, the file that lists the recordings. Its feed
   begins where the last recording stopped, or at position 0 in a new directory.
3. It adds an exclusive publication at that position and records it. Every run of the
   server is a new recording, and the feed is still one line of positions.
4. It reads the earlier recordings back in order, each through a replay, to rebuild its
   state. With a snapshot it reads from the snapshot's position.
5. An append offers its records, waits until the recorder has written them, asks for a
   sync and returns when the sync has. The server answers a request only after that,
   so what it answered is on the disk.
6. When it takes a snapshot, it rolls the feed: it stops the recording and begins the
   next where that one stopped, with no restart. It then purges the stopped recordings
   that the snapshot covers, but for the newest.
7. When its client was closed under it, it connects a new client to the driver it
   still runs, stops the earlier recording if that is still live, and adds the feed
   again where that recording stopped.

## The two shapes

The server changes one thing between the Cavalry it is pinned to and the one it moves
to. `tests/feed/mod.rs` calls it the shape.

| | Pinned, at 620a2e29 | What the server moves to |
| --- | --- | --- |
| Keys the channel adds | none | `ssc=true` and `term-length=1m` |
| Term length | 16 MiB, the length of every `cavalry:udp` term then | 1 MiB |
| What connects the publication | a subscription of the server's own on a loopback endpoint, added as a destination | the recording ([ADR 0098](../decisions/0098-let-a-recording-connect-a-cavalry-udp-publication-whose-channel-names-ssc.md)) |

The move happens on a log directory that the server keeps. So one read of the feed
crosses recordings of the two term lengths
([ADR 0099](../decisions/0099-let-a-cavalry-udp-publications-channel-name-its-term-length.md)).

## What the server calls, and what it relies on

The server imports `Cavalry`, `Driver`, `Flags`, `Image`, `NULL_LENGTH`, `Offer`,
`Publication`, `Recording`, `ReplayStopReason` and `Subscription` from `cavalry`, with
the crate's default features off. It imports `catalog::CatalogReader` and
`layout::State` from `cavalry-log`, because the client lists no recordings.

| The server | Calls | Relies on |
| --- | --- | --- |
| Starts and stops its driver | `Driver::start`, `Driver::dir`, `Driver::close` | A driver starts again on the directories that its last run left, stopped or killed, with no step by an operator. |
| Connects its client | `Cavalry::connect_to`, `Cavalry::is_closed`, `Cavalry::close` | `is_closed` is true once the driver or the client's own conductor closed the client, as after a stall of 12 s. A second client connects to the same driver in the same process, after the first has gone or beside it. |
| Adds the feed at a position | `Cavalry::add_exclusive_publication` on `cavalry:udp?control=<address>\|control-mode=manual\|init-term-id=0\|term-id=<n>\|term-offset=<n>`, stream 10 | The publication's first frame is at position `term-id` × the term length + `term-offset`. The position may be in any term, and need not be where a term begins. |
| Connects the feed, pinned shape | `Cavalry::add_subscription` on `cavalry:udp?endpoint=<address>`, `Cavalry::add_destination`, `Cavalry::remove_destination`, `Subscription::poll`, `Subscription::close` | A destination that is a subscription of the same driver connects the publication. A destination added again after the publication was closed and added again is accepted. |
| Connects the feed, other shape | the channel keys `ssc=true` and `term-length=1m` | A live recording connects the publication, with no subscriber at all. |
| Offers a record | `Publication::offer`, `Publication::is_connected`, `Publication::max_message_length`, `Publication::registration_id`, `Publication::close`, `Offer::reason` | `offer` returns the position after the record. `Offer::NotConnected`, `Offer::BackPressured` and `Offer::AdminAction` pass when the offer is made again. A record of `max_message_length` bytes is taken. |
| Records the feed | `Cavalry::start_recording` with the publication's channel and stream | The recording is of the publication that this client added a moment before, from that publication's position. |
| Syncs before it answers | `Recording::position`, `Recording::sync`, `Recording::synced_position` | `position` reaches the position `offer` returned. After `sync` returns, `synced_position` is at least that position, and those bytes survive a kill of the process. |
| Stops a recording | `Recording::stop`, `Recording::stop_position`, `Cavalry::stop_recording` with a recording id from the catalog | A stop cuts the recording at what the recorder has written. `stop_position` is `None` until the stop is done. Any client may stop a recording. |
| Reads the catalog | `CatalogReader::open`, `slots`, `get`; a slot's `state`, `stream_id`, `is_live`, `start_position`, `stop_position`; `State::Valid` | The catalog is read while the driver runs. A purged recording's slot is not `Valid`. A recording that a kill left live has its stop position once the next driver has started, at the last record that was synced or past it. |
| Reads the feed back | `Cavalry::start_replay` with `NULL_LENGTH` onto `cavalry:ipc`, stream 11; `Replay::session_id`, `Replay::ended`, `ReplayStopReason::Ended`; `Subscription::images`, `Image::session_id`, `Image::positions`, `Image::position`; the header's `session_id`, `term_id`, `term_offset`, `aligned_length` and `flags`; `Flags::BEGIN`, `Flags::END` | A replay starts from any position that is the end of a record. The image's positions are the recording's. The frames of one record are consecutive, the first with `BEGIN` and the last with `END`. Padding at the end of a term reaches no handler and moves the image's position. |
| Purges what a snapshot covers | `Cavalry::purge_recording` | The recording's files are removed and its id is never used again. A purge is refused while a replay reads the recording, and may be sent again. |

The two test programs hold every row, in both shapes where a row has two.
`feed_held_still.rs` holds what a stall and a kill show: `is_closed` after a stall, the
reconnect under an append, and the three sentences above that name a kill. Each test
says in its first comment what it holds.

## The log directory the server keeps

A server's log directory outlives the build that wrote it. The two servers in
production hold directories that the tree at 620a2e29 wrote, in version 2 of the
durable log's format, and `master` writes version 4. So the server relies on one more
thing than its calls: a later driver opens its directory in place, with no step by an
operator, and the feed goes on in it.

Such a directory holds one recording for each run of the server and for each roll,
with the recordings that a snapshot covers purged. After a kill or a power cut its
last recording is still live in the catalog.

`conformance/upgrade/log-directory.sh` builds the tree at 620a2e29 on every run of
`just conformance` and has it write such a directory, once with the last run stopped
and once with it killed. It then holds that this tree's `cavalry run` starts on each,
that the feed begins at the last position the older tree answered and reads back every
record it left, that what was purged stays gone, and that the feed takes a new
recording in each shape.
[`conformance/README.md`](../../conformance/README.md), "The log directory's upgrade
gate", has the steps, and
[ADR 0141](../decisions/0141-open-in-place-a-log-directory-written-by-the-tree-that-servers-run-and-build-that-tree-on-every-run.md)
the decision. A change to the format of the durable log passes that gate in the pull
request that makes it.

The gate builds the feed's files, `tests/feed/` and the example that drives them, in
the older tree too. So a change that makes those files use a call that 620a2e29 lacks
fails there. ADR 0141 says what the gate then keeps.

## What does not hold

### A start beside an earlier client that the driver still has

The server's reconnect, step 7 above, expects that the recording it starts is of the
publication it added. That holds once the driver has dropped the earlier client. It
does not hold while the driver still has the earlier client: one that closed on its own
side and that the driver has not timed out yet, which lasts up to the client liveness
timeout of 10 s.

`start recording` takes the live publication with the lowest registration id that is
not yet being recorded, whoever sends the command (`spec/client-protocol.md`,
"Commands";
[ADR 0025](../decisions/0025-record-a-stream-in-the-driver.md)).
The earlier client's publication is live, the server stopped its recording a moment
before, and it has the lower registration id. So the new recording is of the earlier
client's publication. In the pinned shape the server's next append waits 15 s for a
recorder that never reaches its records, and fails. In the other shape the feed never
connects. Nothing the server answered is lost, because the append never returns: the
server ends, and its next start is correct.

After a stall of the whole process the driver times the client out as the process
comes back, so the server's reconnect meets a driver that has dropped the client:
`feed_held_still.rs` holds that. The test
`a_start_beside_an_earlier_client_the_driver_still_holds_records_that_clients_publication`
holds the rule as it stands, so that a change to it is seen. Work item 372 on the
Brigade Board holds the question of whether the rule changes.

### A recording that ends in padding, read back

This one is the server's own, read from its code and not reproduced. When the server
reads a recording back, it takes the replay as read to its end when the last frame it
was handed ends there, or when the replay's image stands there in the same pass of its
loop. Padding at the end of a term reaches no handler, so for a recording that ends in
padding only the image's position says that the end was reached. The driver withdraws
a replay's image once every subscriber has read it to the end of stream, and the
client may learn that the replay ended after that (`Replay::ended`, which says to keep
the image's position from each poll). The server keeps the frames' position and not
the image's. So its read of such a recording can end with "was let go of", and its
start fails.

A recording ends in padding when an offer met the end of a term and the process was
killed, or its client closed, before the offer was made again: a window of about
200 µs at each term's end. The copy in `tests/feed/mod.rs` keeps the server's code as
it is, with a comment where the gap is. No test holds it, because what happens
depends on which of two threads is first.

## Why a copy of the server's code, and not the server's own tests

Both exist, and they answer different questions.

- The copy runs in every pull request, in about 1 s for `feed_shape.rs` and 13 s for
  `feed_held_still.rs`, which holds its processes still for 12 s. It fails at the call
  that broke. It needs nothing but this repository. It can go stale: it is the server's file
  as it was at Brigade's commit 5ddacc75. `git log 5ddacc75.. --
  server/crates/brigade-log/src/feed.rs` in Brigade's repository lists what changed
  since.
- The server's own suite is the truth about the server. It needs Brigade's repository
  and its toolchain, so it cannot be a check of every pull request here. A workflow
  builds the server at its `master` against this `master` each night and runs that
  suite:
  [Read the nightly run of Brigade's server](../how-to/read-the-nightly-run-of-brigades-server.md)
  says what a red night means and who acts.

The copy is a test of `cavalry`, not of `cavalry-client`, because the server depends on
`cavalry`. A name that `cavalry` stops re-exporting breaks the server, and breaks the
copy in the same way.

## The decisions behind this page

- [ADR 0138](../decisions/0138-draw-the-finish-line-at-a-beta-that-is-an-alternative-to-aeron-and-build-it-sixth.md):
  the beta's ninth condition, "What Brigade's server runs is a gate here", and feature
  2 of its twelve, which is work item 362.
- [ADR 0141](../decisions/0141-open-in-place-a-log-directory-written-by-the-tree-that-servers-run-and-build-that-tree-on-every-run.md):
  the log directory's upgrade gate.
- [ADR 0142](../decisions/0142-build-brigades-server-against-master-each-night-through-a-cargo-patch.md):
  the nightly build of the server against `master`, and who reads a red night.
- [ADR 0025](../decisions/0025-record-a-stream-in-the-driver.md): which publication
  `start recording` takes.
- [ADR 0059](../decisions/0059-create-a-publication-at-a-position-and-arm-a-recording.md):
  a publication created at a position.
- [ADR 0098](../decisions/0098-let-a-recording-connect-a-cavalry-udp-publication-whose-channel-names-ssc.md)
  and
  [ADR 0099](../decisions/0099-let-a-cavalry-udp-publications-channel-name-its-term-length.md):
  the two keys of the shape the server moves to.
- [ADR 0031](../decisions/0031-truncate-purge-and-delete-a-stopped-recording.md): the
  purge.
