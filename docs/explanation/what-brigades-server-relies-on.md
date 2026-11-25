# What does Brigade's server rely on?

This page answers one question: which calls and behaviours of Cavalry does Brigade's
server rely on, so that a change to one of them is made knowingly. Brigade is a
messaging service for coding sessions, `github.com/appshapes/brigade`. Its server is a
Rust program that embeds a Cavalry driver. It takes Cavalry as a git dependency pinned
to one commit
([ADR 0138](../decisions/0138-draw-the-finish-line-at-a-beta-that-is-an-alternative-to-aeron-and-build-it-sixth.md),
condition 9). Three builds of the server matter here, and they pin different commits:

- **Server 0.9.0**, Brigade's tag `server-v0.9.0`, pins Cavalry at 620a2e29. Two teams'
  servers ran it in production until 2026-11-24, each on a log directory it keeps.
- **Server 0.10.0**, the tag `server-v0.10.0` of 2026-11-22, pins Cavalry at d265af82.
  Both servers run it since 2026-11-24, each started in place on the directory that
  0.9.0 wrote.
- **Brigade's `master`** pins what its `server/crates/brigade-log/Cargo.toml` names.
  Each night's run of the server's suite prints it
  ([Read the nightly run of Brigade's server](../how-to/read-the-nightly-run-of-brigades-server.md),
  step 2).

The server's code was read for work item 362 on 2026-11-21, at Brigade's commit
5ddacc75, and again for work item 410, at the tag `server-v0.10.0`, Brigade's commit
020dc2fd: the file `server/crates/brigade-log/src/feed.rs`.

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
  for call from server 0.10.0. Two test programs run it in `just test`, in both shapes
  of the feed: `crates/cavalry/tests/feed_shape.rs`, and
  `crates/cavalry/tests/feed_held_still.rs` for what needs a process to stop. A change
  that breaks a call the server makes fails there, in the pull request that makes the
  change.
- A log directory that the servers in production wrote, with the tree at 620a2e29 and
  then with the tree at d265af82, opens under this tree in place: with no step by an
  operator, with every record read back at its position. `conformance/upgrade/log-directory.sh` holds that in `just
  conformance`.
- Two things the server's code expects do not hold. They are under "What does not
  hold". The first is work item 372 on the Brigade Board. The second was the server's
  own, a read of a recording that ends in padding: Brigade fixed it in server 0.10.0,
  and server 0.9.0, which the servers in production run, still has it.
- A change to a call in the table is either kept compatible or made on purpose.
  [Change a call that Brigade's server makes](../how-to/change-a-call-that-brigades-server-makes.md)
  has the steps.

## How the server keeps its feed

1. At its start the server checks that its driver directory's file system has room
   for the logs it will map, each three terms and a page, sized for the longest term
   that a recording in the catalog has. It then starts a driver on its driver
   directory and its log directory, and connects one client to it.
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

The server changed one thing between its release 0.9.0 and its release 0.10.0.
`tests/feed/mod.rs` calls it the shape. It names the first `PINNED`, after the pin that
the servers in production run, and the second `MOVED_TO`. This page calls them the
pinned shape and the shape the server moved to.

| | Server 0.9.0, pinned to 620a2e29 | Server 0.10.0, pinned to d265af82 |
| --- | --- | --- |
| Keys the channel adds | none | `ssc=true` and `term-length=1m` |
| Term length | 16 MiB, the length of every `cavalry:udp` term then | 1 MiB |
| What connects the publication | a subscription of the server's own on a loopback endpoint, added as a destination | the recording ([ADR 0098](../decisions/0098-let-a-recording-connect-a-cavalry-udp-publication-whose-channel-names-ssc.md)) |

The move happens on a log directory that the server keeps. So one read of the feed
crosses recordings of the two term lengths
([ADR 0099](../decisions/0099-let-a-cavalry-udp-publications-channel-name-its-term-length.md)).
A replay's log has the term length of its recording (`spec/client-protocol.md`, "start
replay"), so a directory that server 0.9.0 wrote needs room for logs of 16 MiB terms
until its recordings of that shape are purged. That is why the room of step 1 is sized
for the longest term in the catalog.

## What the server calls, and what it relies on

The server imports `Cavalry`, `Driver`, `Flags`, `Image`, `NULL_LENGTH`, `Offer`,
`Publication`, `Recording`, `Replay`, `ReplayStopReason` and `Subscription` from
`cavalry`, with the crate's default features off. It imports `catalog::CatalogReader` and
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
| Reads the catalog | `CatalogReader::open`, `slots`, `get`; a slot's `state`, `stream_id`, `is_live`, `start_position`, `stop_position`, `term_length`; `State::Valid` | The catalog is read while the driver runs, and before the driver starts. A purged recording's slot is not `Valid`. A recording that a kill left live has its stop position once the next driver has started, at the last record that was synced or past it. A slot's `term_length` is its recording's, in a catalog that an earlier tree wrote too. |
| Reads the feed back | `Cavalry::start_replay` with `NULL_LENGTH` onto `cavalry:ipc`, stream 11; `Replay::session_id`, `Replay::ended`, `ReplayStopReason::Ended`; `Subscription::images`, `Image::session_id`, `Image::positions`, `Image::position`; the header's `session_id`, `term_id`, `term_offset`, `aligned_length` and `flags`; `Flags::BEGIN`, `Flags::END` | A replay starts from any position that is the end of a record. The image's positions are the recording's. The frames of one record are consecutive, the first with `BEGIN` and the last with `END`. Padding at the end of a term reaches no handler and moves the image's position. The driver withdraws a replay's image once it has been read to the end of stream, and `replay stopped` may reach the client after that, so the server keeps the furthest position the image stood at. |
| Purges what a snapshot covers | `Cavalry::purge_recording` | The recording's files are removed and its id is never used again. A purge is refused while a replay reads the recording, and may be sent again. |

The two test programs hold every row, in both shapes where a row has two.
`conformance/upgrade/log-directory.sh` holds the catalog row on the directories that the
trees at 620a2e29 and d265af82 wrote: this tree reads each before its driver starts and
finds the longest term among its recordings, 16 MiB where 620a2e29 wrote one that is
left and 1 MiB where d265af82 wrote alone.
`feed_held_still.rs` holds what a stall and a kill show: `is_closed` after a stall, the
reconnect under an append, the three sentences above that name a kill, and a kill
after a term's padding. Each test says in its first comment what it holds.

## The log directory the server keeps

A server's log directory outlives the build that wrote it. The two servers in
production hold directories that the tree at 620a2e29 wrote, in version 2 of the
durable log's format, and that the tree at d265af82 has written on since 2026-11-24,
in version 5. Each server also keeps a copy taken under 0.9.0, which 620a2e29 alone
wrote. `master` writes version 6 since 2026-11-22
([ADR 0144](../decisions/0144-keep-a-recordings-synced-position-in-its-time-index-and-cut-a-recording-a-crash-left-live-to-it.md),
[ADR 0145](../decisions/0145-withhold-the-vote-of-a-member-that-lost-its-log-until-it-holds-what-the-cluster-held.md)). Version 6 adds a cluster member's vote floor to its
terms file, which a server's directory does not have.
So the server relies on one more
thing than its calls: a later driver opens its directory in place, with no step by an
operator, and the feed goes on in it.

Such a directory holds one recording for each run of the server and for each roll,
with the recordings that a snapshot covers purged. After a kill or a power cut its
last recording is still live in the catalog.

`conformance/upgrade/log-directory.sh` builds the trees at 620a2e29 and d265af82 on
every run of `just conformance` and has them write three such directories: 620a2e29's
alone, 620a2e29's and then d265af82's in place, as production's are, and d265af82's
alone, as a server first installed at 0.10.0 would hold. It writes each once with the
last run stopped and once with it killed. It writes the one that both trees write in
turn a third time, with 620a2e29's last run killed and d265af82's stopped, so that
d265af82 recovers the recording that 620a2e29 left live. It then holds that this
tree's `cavalry run` starts on each, that the feed begins at the last position the older trees answered and reads back every
record they left, that what was purged stays gone, and that the feed takes a new
recording in each shape.
[`conformance/README.md`](../../conformance/README.md), "The log directory's upgrade
gate", has the steps, and
[ADR 0141](../decisions/0141-open-in-place-a-log-directory-written-by-the-tree-that-servers-run-and-build-that-tree-on-every-run.md)
the decision. A change to the format of the durable log passes that gate in the pull
request that makes it. The gate builds each tree that the servers in production have
run, and keeps one while a server holds a directory that the tree wrote any part of,
alone or before another tree
([ADR 0156](../decisions/0156-hold-in-the-upgrade-gate-every-directory-a-server-holds-each-trees-alone-and-the-trees-in-turn.md), work item 441). On CI's macOS runner it builds
620a2e29 alone and writes server 0.9.0's directory alone, as it did before: with the
three directories written all seven ways, that runner was lost partway through `just
conformance`
([ADR 0160](../decisions/0160-keep-the-log-directorys-gate-on-one-tree-on-cis-macos-runner-and-track-the-lost-runner-as-a-fault-of-its-own.md), work item 456).

The gate builds the feed's files, `tests/feed/` and the example that drives them, in
each older tree too. So a change that makes those files use a call that 620a2e29 or
d265af82 lacks fails there. ADR 0141 says what the gate then keeps. The server's file of 0.10.0, which
the copy follows, uses none: the two names it added, the type `Replay` and a slot's
`term_length`, are both in 620a2e29.

Brigade holds the same from its side. Its test
`server/crates/brigade-server/tests/stored.rs` starts each build of the server on the
log directory that its release 0.9.0 wrote on the feed and was killed on, and since
Brigade's commit 5baebabd on the one that 0.10.0 left. Brigade keeps each directory in
its repository, under `server/testdata/stored/`, each recording file without its
trailing zeros. Cavalry's nightly job runs that test against `master`.

Nothing holds the way back: ADR 0141 promises no start of an older tree on a directory
that a newer one wrote. Brigade's session tried one once, a 0.9.0 started on a small
directory that the 0.10.0 build had written, and it read the recording back. Nothing
promises that. A copy of a server's log directory, taken before its first start on a
newer build, is the way back.

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
comes back, in all but a window of microseconds, and the server's reconnect meets a
driver that has dropped the client. The records the server's first append offers as
the process comes back can reach the recording first. Its recording then stops past
them, and the stop has synced them, so the reconnect finds them in the feed and the
server answers them at their positions: the feed goes on past the last record answered
before the stall, not at it. In the window, the client's keepalive reaches the driver
before the driver's first check, and the client is never dropped.
`feed_held_still.rs` holds each of the three orders (work item 413). The test
`a_start_beside_an_earlier_client_the_driver_still_holds_records_that_clients_publication`
holds the rule as it stands, so that a change to it is seen. Work item 372 on the
Brigade Board holds the question of whether the rule changes.

### A recording that ends in padding, read back by server 0.9.0

This one was the server's own. Brigade fixed it on its `master` at 5236ddd0 on
2026-11-22, and server 0.10.0 has the fix (Brigade's work item 375). Server 0.9.0,
which the servers in production run, still has it.

When server 0.9.0 reads a recording back, it takes the replay as read to its end when
the last frame it was handed ends there, or when the replay's image stands there in
the same pass of its loop. Padding at the end of a term reaches no handler, so for a
recording that ends in padding only the image's position says that the end was
reached. The driver withdraws a replay's image once every subscriber has read it to
the end of stream, and the client may learn that the replay ended after that. Server
0.9.0 keeps the frames' position across the passes and not the image's, so its read of
such a recording can end with "was let go of", and its start fails. Brigade's session
reproduced it: on the reader as it was, against the tree at 620a2e29, 9 of 12 starts
on one such recording failed with "the replay of recording 0 was let go of at position
16715616, before it was read to its end at 16777216". `Replay::ended` says on `master`
to keep the image's position from each poll; the tree at 620a2e29 behaves the same,
without that sentence.

The fix keeps the furthest position the image has stood at in any pass, read once a
pass right after the frames' poll, and takes the replay as read when that position or
the frames' has reached the end. The copy in `tests/feed/mod.rs` does the same. Its
tests hold it in the order that failed, by telling the reader that a replay ended only
once the replay's image has gone: `feed_shape.rs` on a recording that a stop left
ending in padding, and `feed_held_still.rs` on one that a kill left. Each fails on the
reader as it was.

A recording ends in padding when an offer met the end of a term and its client closed
before the offer was made again: a window of about 200 µs at each term's end. After a
kill in that window, it ends in padding only when the recorder's own sync took the
padding in before the kill, which the recorder does a second after the recording's
last sync. Otherwise the next start cuts the recording to the last synced position,
which lies before the padding
([ADR 0144](../decisions/0144-keep-a-recordings-synced-position-in-its-time-index-and-cut-a-recording-a-crash-left-live-to-it.md)).
So a reader of a killed feed may find either end, and the records are the same.
Under the tree at 620a2e29, which cuts nothing, a kill in the window leaves the padding.
Brigade's test
`a_feed_whose_process_was_killed_after_a_terms_padding_ends_at_its_last_record_or_at_the_terms_end`
holds both ends since its commit 9c9ac593 (Brigade's work item 387), and
`feed_held_still.rs` holds them here.

## Why a copy of the server's code, and not the server's own tests

Both exist, and they answer different questions.

- The copy runs in every pull request, in about 6 s for `feed_shape.rs` and 21 s for
  `feed_held_still.rs`, which holds its processes still for 12 s. It fails at the call
  that broke. It needs nothing but this repository. It can go stale: it is the server's
  file as Brigade released it in server 0.10.0, the tag `server-v0.10.0`, commit
  020dc2fd. `git log 020dc2fd.. -- server/crates/brigade-log/src/feed.rs` in Brigade's
  repository lists what changed since.
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
  the two keys of the shape the server moved to.
- [ADR 0031](../decisions/0031-truncate-purge-and-delete-a-stopped-recording.md): the
  purge.
