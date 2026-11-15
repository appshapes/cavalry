# Run a cluster in ten minutes

A tutorial: in ten minutes, on a laptop or a cloud VM, start a cluster of three members
on one host, run a service beside each member, send the cluster numbers from a client
and read its replies, stop the leader and see another member's service answer with the
same state, and start the stopped member again. You type the commands of ten steps in
two terminals and see a result after each. Nothing is configured but the list of
members. In Aeron, this is Cluster with a clustered service and an `AeronCluster`
client ([glossary](../glossary.md)).

A cluster is three or more drivers, its members, that keep one log between them. The
members elect one of them, the leader, which writes the log. An entry is committed once
two of the three members hold it. A service is a program beside each member that reads
the committed entries, in order, and changes its state by them alone, so every member's
service holds the same state. A client sends the cluster a message through a session
and reads the reply of one service: the one beside the leader.

The service of this page is an adder. Its state is one number, the total. A message is
a number; the service adds it to the total and replies with the total.

The same path is `scripts/run-a-cluster.sh`, which `just quickstart` runs after the
README's path, and CI runs that on Linux and macOS, so what this page says happens is
what the repository checks.

## Before you start

- A Rust toolchain and a checkout of this repository, as for
  [Publish and subscribe in ten minutes](publish-and-subscribe.md). That page comes
  first: this one uses `cavalry run` and `cavalry doctor` as that one teaches them.
- No directory `/tmp/cavalry-tutorial`. Every program of this page keeps its files
  there, and step 10 removes it.
- The UDP ports 9000 to 9004, 9010 to 9014, 9020 to 9024 and 9100 free on this host.
  They are free unless a cluster of [Run a cluster of three members](../how-to/run-a-cluster.md)
  still runs here.
- Two terminals, both in the checkout. The first is for the cluster and the client, the
  second for the services.

## 1. Build

In the first terminal:

```sh
cargo build --release -p cavalry --bin cavalry --example adder-service --example adder-client
```

You will see `Finished` and nothing else, in a few seconds when the first tutorial's
build is there.

## 2. Start three members

In the first terminal, make the directory, name the three members, and start them:

```sh
mkdir /tmp/cavalry-tutorial
export CAVALRY_CLUSTER_MEMBERS='0,127.0.0.1:9000,127.0.0.1:9001,127.0.0.1:9002,127.0.0.1:9003,127.0.0.1:9004|1,127.0.0.1:9010,127.0.0.1:9011,127.0.0.1:9012,127.0.0.1:9013,127.0.0.1:9014|2,127.0.0.1:9020,127.0.0.1:9021,127.0.0.1:9022,127.0.0.1:9023,127.0.0.1:9024'
CAVALRY_CLUSTER_MEMBER_ID=0 target/release/cavalry run --dir /tmp/cavalry-tutorial/member-0 --log-dir /tmp/cavalry-tutorial/member-0-log &
CAVALRY_CLUSTER_MEMBER_ID=1 target/release/cavalry run --dir /tmp/cavalry-tutorial/member-1 --log-dir /tmp/cavalry-tutorial/member-1-log &
CAVALRY_CLUSTER_MEMBER_ID=2 target/release/cavalry run --dir /tmp/cavalry-tutorial/member-2 --log-dir /tmp/cavalry-tutorial/member-2-log &
```

The second line is the members string: each member's id and its five UDP endpoints, with
`|` between the members. Every member is given the same string and its own id. A member
is a driver, so each has a driver directory and a log directory of its own. The `&`
runs a program in the background, so that the terminal takes the next command; the
shell prints a line of its own for each, such as `[1] 63648`.

You will see two lines from each member. For member 0: `cavalry run: driver started on
/tmp/cavalry-tutorial/member-0 (pid <pid>), recording into
/tmp/cavalry-tutorial/member-0-log, conductor idle strategy backoff; Ctrl-C or SIGTERM
stops it`, then a line that begins `cavalry run: cluster member 0 started`. Leave the
three running.

## 3. See which member leads

In the first terminal, ask each member what it is:

```sh
CAVALRY_CLUSTER_MEMBER_ID=0 target/release/cavalry doctor --dir /tmp/cavalry-tutorial/member-0 --log-dir /tmp/cavalry-tutorial/member-0-log | grep 'cluster member'
CAVALRY_CLUSTER_MEMBER_ID=1 target/release/cavalry doctor --dir /tmp/cavalry-tutorial/member-1 --log-dir /tmp/cavalry-tutorial/member-1-log | grep 'cluster member'
CAVALRY_CLUSTER_MEMBER_ID=2 target/release/cavalry doctor --dir /tmp/cavalry-tutorial/member-2 --log-dir /tmp/cavalry-tutorial/member-2-log | grep 'cluster member'
```

You will see one line from each. One member says that it leads, here member 2:

```text
  note cluster-member         this driver is cluster member 2, the leader in term 0, election closed: append position 96, commit position 96; its last election began because the member started; its members string gives a vote to members 0, 1 and 2; see docs/how-to/run-a-cluster.md#when-it-does-not-work
```

The other two say `a follower in term 0, election closed` and `it follows member 2`.
Which member leads is not the same at every start: this page writes 2 for the leader,
and you read your own leader's id in its place. A term, in full a leadership term,
is the time one leader leads; it is 0 here, or 1 when two members asked for votes at
the same moment and voted again.
The log already holds one entry, the term's first, which is why the positions are 96.

## 4. Start a service beside each member

In the second terminal:

```sh
target/release/examples/adder-service --dir /tmp/cavalry-tutorial/member-0 &
target/release/examples/adder-service --dir /tmp/cavalry-tutorial/member-1 &
target/release/examples/adder-service --dir /tmp/cavalry-tutorial/member-2 &
```

Each service is a client of its member's driver, which `--dir` names. You will see two
lines from each, which begin with the last part of its directory's name. For the
service beside member 0:

```text
adder-service member-0: beside the driver at /tmp/cavalry-tutorial/member-0, reading its member's committed log
adder-service member-0: position 0: term 0 began, led by member 2
```

The second line is the log's first entry, which every service has now read. The
program is
[`crates/cavalry/examples/adder-service.rs`](../../crates/cavalry/examples/adder-service.rs):
it makes a `ClusterService`, and its handler is called for each committed entry.

## 5. Start a driver for the client

A client is a program beside a driver on its own host, and that driver is no member.
Here it is on the same host. In the second terminal:

```sh
target/release/cavalry run --dir /tmp/cavalry-tutorial/client --log-dir /tmp/cavalry-tutorial/client-log &
```

You will see one line: `cavalry run: driver started on /tmp/cavalry-tutorial/client
(pid <pid>), recording into /tmp/cavalry-tutorial/client-log, conductor idle strategy
backoff; Ctrl-C or SIGTERM stops it`. This driver starts in the second terminal because
`CAVALRY_CLUSTER_MEMBERS` is not set there: a `cavalry run` that finds the members
string and no member id does not start, and says so.

## 6. Send the cluster two numbers

In the first terminal:

```sh
target/release/examples/adder-client --dir /tmp/cavalry-tutorial/client 5 7
```

You will see four lines:

```text
adder-client: session 0 is open; the leader is member 2, in term 0
adder-client: sent 5; the cluster answered 5 (term 0, cluster time 1794672965141 ms)
adder-client: sent 7; the cluster answered 12 (term 0, cluster time 1794672965167 ms)
adder-client: session 0 is closed
```

The cluster time is the leader's clock when it wrote the entry, in milliseconds since
1970-01-01T00:00:00Z, so yours is later. The program is
[`crates/cavalry/examples/adder-client.rs`](../../crates/cavalry/examples/adder-client.rs):
it reads the members string from `CAVALRY_CLUSTER_MEMBERS`, opens a session with
`connect_cluster`, offers each number as one message, and polls the session for the
reply. The cluster answers the session on UDP port 9100 of this host.

In the second terminal, each of the three services has printed four lines. For the
service beside the leader:

```text
adder-service member-2: position 96, term 0: session 0 opened
adder-service member-2: position 224, term 0: session 0 sent 5; the total is 5; this service replied
adder-service member-2: position 320, term 0: session 0 sent 7; the total is 12; this service replied
adder-service member-2: position 416, term 0: session 0 closed
```

The other two print the same positions and the same totals, and `the reply is not this
service's to send` where the leader's service says `this service replied`. All three
ran the same code on the same entries. One of them sent each reply.

## 7. Stop the leader

In the first terminal, stop the member that step 3 named as the leader. For member 2:

```sh
kill '%?member-2'
```

`%?member-2` names the background program of this terminal whose command holds
`member-2`. For member 0 or member 1, write its id in place of the 2.

You will see `cavalry run: stopping` and then `cavalry run: closed;
/tmp/cavalry-tutorial/member-2 stays for doctor, and the next driver remakes it`. In the
second terminal the service beside that member ends, with a line that begins
`adder-service: the driver at /tmp/cavalry-tutorial/member-2 closed this service's
client (driver-closed) with the total at 12`.

The two members that are left elect one of them. That takes about 10 s: a follower
starts an election when it has heard nothing from its leader for 10 s. When it is done,
the two services that still run each print one line in the second terminal, here with
member 1 as the next leader:

```text
adder-service member-0: position 512: term 1 began, led by member 1
adder-service member-1: position 512: term 1 began, led by member 1
```

The term is 2 or more when the two members voted more than once. Wait for these two
lines.

## 8. Send a number to the cluster that is left

In the first terminal:

```sh
target/release/examples/adder-client --dir /tmp/cavalry-tutorial/client 30
```

You will see three lines:

```text
adder-client: session 1 is open; the leader is member 1, in term 1
adder-client: sent 30; the cluster answered 42 (term 1, cluster time 1794672976620 ms)
adder-client: session 1 is closed
```

The total went on from 12. The service that answered is not the one that answered in
step 6: it held the total 12 already, because it read the same log. In the second
terminal the two services print `session 1 sent 30; the total is 42`, and the one
beside the new leader `this service replied`.

When the member you stopped was member 0, the first line comes after 5 s: the client
asks the members in the order of the members string, and waits that long for the first
before it asks the next.

## 9. Start the stopped member again

In the first terminal, start the member you stopped, with its command of step 2. For
member 2:

```sh
CAVALRY_CLUSTER_MEMBER_ID=2 target/release/cavalry run --dir /tmp/cavalry-tutorial/member-2 --log-dir /tmp/cavalry-tutorial/member-2-log &
```

You will see its two lines of step 2. The member reads the log its log directory
holds, follows the new leader, and takes from it what was committed while it was away.
Then start its service, in the second terminal:

```sh
target/release/examples/adder-service --dir /tmp/cavalry-tutorial/member-2 &
```

You will see the whole log, read from its start:

```text
adder-service member-2: beside the driver at /tmp/cavalry-tutorial/member-2, reading its member's committed log
adder-service member-2: position 0: term 0 began, led by member 2
adder-service member-2: position 96, term 0: session 0 opened
adder-service member-2: position 224, term 0: session 0 sent 5; the total is 5; the reply is not this service's to send
adder-service member-2: position 320, term 0: session 0 sent 7; the total is 12; the reply is not this service's to send
adder-service member-2: position 416, term 0: session 0 closed
adder-service member-2: position 512: term 1 began, led by member 1
adder-service member-2: position 608, term 1: session 1 opened
adder-service member-2: position 736, term 1: session 1 sent 30; the total is 42; the reply is not this service's to send
adder-service member-2: position 832, term 1: session 1 closed
```

The service reached the total 42 from the log alone, the entries of term 1 among them,
which were committed while its member was stopped. It sends no reply for an entry that
was in the log before it started: a service before it answered those.

Read the total, in the first terminal:

```sh
target/release/examples/adder-client --dir /tmp/cavalry-tutorial/client 0
```

You will see `adder-client: sent 0; the cluster answered 42 (term 1, cluster time
<time> ms)` between the lines that open and close session 2, and all three services
print `session 2 sent 0; the total is 42`.

## 10. Stop everything

In the second terminal, stop the three services and the client's driver:

```sh
kill '%?member-0'
kill '%?member-1'
kill '%?member-2'
kill '%?client'
```

In the first terminal, stop the three members, wait until they have closed, and
remove the directory:

```sh
kill '%?member-0'
kill '%?member-1'
kill '%?member-2'
wait
rm -rf /tmp/cavalry-tutorial
```

Each member prints `cavalry run: stopping` and `cavalry run: closed`, and so does the
client's driver in the second terminal. `wait` returns when the three members have
ended, so that no member is still writing in the directory when it is removed. The
shell prints a line for each program that ended.

## What you saw

Three drivers kept one log. Each session, each message and each change of leader is an
entry of it, at a position that is the same on every member. A service is the program
that gives the entries a meaning: it ran beside every member, read the same entries in
the same order, and so held the same total three times. One service answered each
message, the one beside the leader of the entry's term. When the leader stopped, the
other two members elected a leader within seconds, and its service answered the next
message from the state it already had. The member that came back took the entries it
had missed from the leader, and its service reached the same total by reading the log
from its start. Nothing of the state was copied between the services: the log is the
state.

## What to read next

- [Run a cluster of three members](../how-to/run-a-cluster.md): the same cluster as an
  operator's task, on three hosts, with what `cavalry doctor` says when a member cannot
  join, and what a member's log directory must be kept on.
- [Read the committed log beside a member](../how-to/read-the-committed-log.md): the
  log's entries as they are, from a shell.
- [`spec/cluster.md`](../reference/cluster.md): what the members say to each other,
  "Sessions" for what a client and the leader say, and "The service" and "Replies" for
  which service answers and when none does.
- [`crates/cavalry-client/README.md`](../../crates/cavalry-client/README.md): the
  client API, `connect_cluster` and `ClusterSession` for a client, `cluster_service` and
  `ClusterService` for a service.
- [`clients/README.md`](../../clients/README.md): the same session and the same service
  from Python, Node, Go, the JVM and .NET.
