# Take a snapshot of a service's state

A how-to page: one task, have a cluster's service take a snapshot of its program's
state and start from it the next time it runs ([glossary](../glossary.md), "service"
and "snapshot"). Without one, a service reads its member's log from position 0 at
every start, and no recording of that log can be purged. The page uses the Python
client; each of the five clients has these calls, in one
shape ([ADR 0149](../decisions/0149-give-each-clients-service-a-snapshot-and-a-start-from-one-with-the-state-copied-to-the-caller.md)),
and in Rust they are `ClusterService::restore`, `snapshot_due` and `snapshot`
(`crates/cavalry-client/README.md`). What the service writes is
[`spec/cluster.md`](../reference/cluster.md), "A service's snapshot". In Aeron, these
are a service's `onTakeSnapshot` and the snapshot image of its `onStart`.

## Prerequisites

- A cluster that runs, started as [Run a cluster of three members](run-a-cluster.md)
  starts one: three members on one host, with the directories `/tmp/cavalry-member-0`,
  `-1` and `-2`.
- The shared library, which the Python package loads: `cargo build --release -p
  cavalry-ffi --lib`.
- Python 3.9 or later. The package is `clients/python`, used from the checkout.

## Steps

1. Write the service. Save this as `counter_service.py`. Its state is one number,
   how many session messages the cluster has committed, and its snapshot is that
   number as eight bytes:

   ```python
   import sys
   import time

   import cavalry

   total = 0


   def on_entry(body, entry, service):
       global total
       if entry.type == cavalry.CAVALRY_CLUSTER_ENTRY_SESSION_MESSAGE:
           total += 1


   with cavalry.Client(sys.argv[1]) as client:
       service = client.cluster_service()
       try:
           # Where the service starts: from its last snapshot, or from the log's start.
           restored, state = service.restore()
           while restored == cavalry.CAVALRY_CLUSTER_RESTORED_NO_MEMBER:
               time.sleep(0.01)
               restored, state = service.restore()
           if state is not None:
               total = int.from_bytes(state, "little")
           print(f"started at position {service.position()} with a total of {total}", flush=True)
           while True:
               service.poll(on_entry, 10)
               # A poll that ended at the leader's request: the state as it is there.
               position = service.snapshot_due()
               if position is not None:
                   try:
                       written = service.snapshot(total.to_bytes(8, "little"))
                       print(f"snapshot at position {position}: written {written}", flush=True)
                   except cavalry.CavalryError as error:
                       # Nothing committed is lost: the next request asks again.
                       print(f"no snapshot at position {position}: {error}", flush=True)
               time.sleep(0.001)
       finally:
           service.close()   # a service is closed before its client
   ```

2. Run it beside one member, here member 0:

   ```sh
   PYTHONPATH=clients/python python3 counter_service.py /tmp/cavalry-member-0
   ```

   You will see `started at position 0 with a total of 0`: the member lists no
   snapshot of a service yet, so the service reads the log from its start.

3. Ask the cluster for a snapshot, as [Ask a cluster for a snapshot](ask-for-a-snapshot.md)
   says, on the host of the member that leads. Here member 1 leads:

   ```sh
   target/release/cavalry snapshot --dir /tmp/cavalry-member-1
   ```

   The command prints the snapshot's log position: 192 on a cluster that has
   committed nothing but its term's first entry, and a higher one on a cluster that
   has committed more. The service prints `snapshot at position 192: written
   True`, with the position the command printed, within a second: its poll ended
   at the leader's request, and it gave its state.

4. Stop the service with Ctrl-C and run it again, as in step 2. You will see
   `started at position 192 with a total of 0`, with that position again: the
   service took its state from its snapshot, and it reads the log from there on.

## How to verify it worked

- The second start printed the position the command printed, and not 0. A service
  is given a state only when its member lists the snapshot, so that line is the
  member's own word.
- The total the second start printed is the total at the snapshot: 0 here, since
  no session sent the cluster a message. This service answers no session, so a
  client that waits for a reply to each message, as the tutorial's does, is told of
  none; `clients/python/README.md`, "The cluster service", says how a service
  replies.
- `just conformance` runs this page's program as these steps do, in
  `conformance/python/howto.py`, and runs the three calls beside three members in
  `conformance/python/snapshot.py`.

## When it does not work

- **`started at position 0` after a snapshot was taken.** The service was started
  beside another member than the one whose service gave its state. Every member
  lists the snapshots of the service beside it, and of no other: run a service
  beside each member.
- **No `snapshot at position` line.** The command was run beside a member that does
  not lead, and said so: [Ask a cluster for a snapshot](ask-for-a-snapshot.md),
  "When it does not work".
- **`written False`.** The member lists a snapshot of a service at that position
  already. A service that reads from position 0 meets every request the log holds
  and is told so for each: nothing is wrong.
- **`no snapshot at position ...` with `CAVALRY_ERR_CLUSTER_SNAPSHOT_FAILED`'s
  message.** A step of the snapshot made no progress for a second, and the message
  names the step and what to check. Nothing committed is lost. The program above
  prints the message and polls on, and the next request asks again.
- **`CavalryError` with `CAVALRY_ERR_CLUSTER_RESTORE_TOO_LATE`.** `restore` was
  called after a poll. Call it once, before the first poll, and again only while it
  answers `CAVALRY_CLUSTER_RESTORED_NO_MEMBER`.
- A service that takes snapshots and never calls `restore` still reads from position
  0 at every start. Its member's `cluster.log_purge_position` rises all the same, so
  beside such a service purge nothing: `counters.toml` says so of that counter.
