# Publish between two hosts

A how-to page: one task, a publisher on one host and a subscriber on another, over
UDP, with a driver on each host and nothing configured but the channel. What goes on
the wire is [`spec/udp.md`](../reference/udp.md); what a stream looks like from a
program is [the tutorial](../tutorials/publish-and-subscribe.md), whose program with
the channel changed is this task on one host. In Aeron, this is a publication and a subscription on an
`aeron:udp` channel ([glossary](../glossary.md)).

## Prerequisites

- A driver on each host, the same version on both, started with `cavalry run`
  ([Run a driver and read doctor](run-the-driver.md)). The publisher's driver puts the
  stream on the wire from its sender thread; the subscriber's driver rebuilds it into
  an image on its receiver thread, and the subscription reads the image as it reads a
  shared-memory log.
- A route between the hosts, and the subscriber's host's address as the publisher's
  host reaches it: an IPv4 address, or a name both hosts resolve the same way. A port
  for the stream, 40123 below; any port no other process on the subscriber's host
  binds.
- A firewall on the subscriber's host, when one is on, that admits UDP to that port
  and the replies to the publisher's socket (step 3).
- On Linux, a session that outlives the driver, on a host reached over SSH. When a
  user's last session ends, logind removes the user's files under `/dev/shm` by default
  (`RemoveIPC=yes`), the driver's directory among them, so a driver started in one SSH
  session loses its directory when that session closes, and `doctor` and new clients
  find no driver while the old ones run on ([Run a driver and read
  doctor](run-the-driver.md#when-it-does-not-work), "The directory vanished under a
  running driver"). Keep one session open on each host for as long as its driver runs,
  through "How to verify it worked", which reads the directory with `cavalry doctor`
  and from the counters file, or run `sudo loginctl enable-linger <user>` there first.

## Steps

1. Write the channel once, the same on both hosts:
   `cavalry:udp?endpoint=<subscriber's address>:40123`. The endpoint is where the
   subscriber's driver receives; the publisher's driver sends to it, and the
   subscriber's driver binds it. A host with two paths to the other names the one to
   use with `|interface=<address>` after the endpoint
   ([the channel URI](../reference/udp.md#the-channel-uri)).

2. On the subscriber's host, start the driver and the program that subscribes. The
   program is the tutorial's with the channel changed, so a complete one is
   [`crates/cavalry/examples/quickstart.rs`](../../crates/cavalry/examples/quickstart.rs);
   the lines that matter are these:

   ```rust,ignore
   // ignore: needs a driver on each of two hosts, which the book's tests do not have
   use cavalry::Cavalry;

   let mut client = Cavalry::connect()?;
   let mut subscription = client.add_subscription("cavalry:udp?endpoint=10.0.0.2:40123", 1001)?;
   loop {
       subscription.poll(|body, header| println!("{} bytes from session {}", body.len(), header.session_id()), 64)?;
       if subscription.is_connected() { /* the image has joined */ }
   }
   ```

   `add_subscription` returns once the driver has bound the port; the subscription has
   no image until the publisher's first setup frame arrives, and `is_connected()` is
   false until then. `cavalry doctor` on this host now lists `note udp-endpoint receive
   endpoint cavalry:udp?endpoint=10.0.0.2:40123: status active, 0 publication(s), 0
   image(s)` and warns `udp-subscription-no-image`, which is right until step 3 has run.

3. On the publisher's host, start the driver and the program that publishes on the same
   channel: `client.add_publication("cavalry:udp?endpoint=10.0.0.2:40123", 1001)?`, then
   the tutorial's claim-and-commit loop. The driver resolves the name once, as the
   command is handled, and refuses one it cannot resolve with error code 9; it sends a
   setup frame to the endpoint every 100 ms until the subscriber's driver answers with a
   status message, and until then `try_claim` is refused with `not-connected`, which
   the loop polls through. When the firewall on the subscriber's host is on, it must
   admit UDP to port 40123 and the replies: the subscriber's driver answers every
   status message and NAK to the datagram's source, which is the publisher's socket on
   a port its operating system chose, so a stateful firewall, `ufw` among them, admits
   the replies by itself, and a stateless ruleset must open the reply path as well.

4. Publish. Each message goes from the publisher's log to the wire in the sender's
   next burst, and from the subscriber's image to the handler on its next poll; a lost
   datagram is NAKed by the receiver and sent again from the sender's log, and the
   subscriber reads nothing past the gap until it is filled
   ([the receiver](../reference/udp.md#the-receiver)). Closing the publication ends
   the stream: the sender's heartbeats carry end of stream, the image ends at the last
   message, and the subscription sees the image go.

## How to verify it worked

- On the publisher's host, `cavalry doctor` lists `note udp-endpoint send endpoint
  cavalry:udp?endpoint=10.0.0.2:40123: status active, 1 publication(s)` and no
  `udp-publication-not-connected`: a receiver has answered. The program's `try_claim`
  returns a claim and `publication.is_connected()` is true.
- On the subscriber's host, `cavalry doctor` lists the receive endpoint with `1
  image(s)`, no `udp-subscription-no-image` and no `udp-image-loss`; `image.created`
  is 1, and the handler is called with every message once and in order.
- `receiver.frames_received` on the subscriber's host and `sender.frames_sent` on the
  publisher's rise together: with the tutorial's 1,000 messages, 1,000 each, with
  `image.created` 1 on the subscriber's host and `image.ended` 1 there once the
  publication has closed. `cavalry doctor` prints none of these four while the stream
  is healthy: `sender.frames_sent` and `receiver.frames_received` are in none of its
  findings, and `image.created` and `image.ended` only in `udp-subscription-no-image`.
  The console that will print them is not built yet, so read them from the `counters`
  file in the driver's directory by its layout ([the counters file](../reference/counters-file.md): the record's label
  is the counter's name, the slot its value; [counters](../reference/counters.md) says
  what each means).

## When it does not work

Every line is `cavalry doctor`'s on one host or the other; [Run a driver and read
doctor](run-the-driver.md#when-it-does-not-work) explains each in full.

- **`WARN udp-publication-not-connected`** on the publisher's host: the setup frames
  go unanswered. In order: the subscriber's driver is not running, or receives on
  another port or address than the channel names, check `cavalry doctor` there for the
  receive endpoint; the stream ids differ, the subscriber's host counts the frames as
  `receiver.setups_unwanted`; the firewall drops the port or the replies, step 3; the
  two drivers run different versions, the subscriber's host counts
  `receiver.dropped_frames`.
- **`WARN udp-subscription-no-image`** on the subscriber's host: no setup frame has
  reached the port for the stream, the mirror of the line above; the same checks,
  from the publisher's side.
- **`WARN udp-interface-mtu`** on either host: the endpoint's address routes over an
  interface whose MTU is below 1436 bytes, a tunnel or a link at 1280, so the stream's
  datagrams of up to 1408 bytes are fragmented or dropped there and the receiver NAKs
  the loss for ever. Raise the interface's MTU or route over another interface; the
  finding names the interface. The check reads IPv4 endpoints with an address of their
  own on Linux; for an IPv6 endpoint, a receive endpoint bound to every address, or
  another operating system, read the interface's MTU yourself, `ip link` on Linux and
  `ifconfig` elsewhere, and hold it to the datagram with its IP and UDP headers: 1436
  for IPv4, whose header is 20 bytes, and 1456 for IPv6, whose header is 40 bytes.
- **`WARN udp-image-loss`** on the subscriber's host: a gap on the wire the receiver is
  NAKing, with `receiver.gaps_detected` rising there and `sender.retransmits_sent` on
  the publisher's host while it is mended; a rebuild position that stands still is a
  gap the sender no longer holds, and the image times out after 10 s. Look at the
  link and at **`WARN udp-socket-buffers`** on both hosts, a socket buffer the host
  capped, with the sysctl to raise.
- **`command refused (refused, error code 9, UnknownHost)`** from `add_publication` or
  `add_subscription`: the name in the channel does not resolve on that host; use the
  address, or put the name in the host's `/etc/hosts`.
- **`command refused (refused, error code 4, ChannelEndpointError)`**: the socket could
  not be opened, bound or connected: on the subscriber's host the port is in use, or
  the address is not one of the host's; on the publisher's, the interface named does
  not exist. The message names the endpoint. A message that says the previous receive
  endpoint has not closed in the 5 s the add waited, with **`WARN
  subscription.add_deferred_timed_out`** from `cavalry doctor` on the subscriber's
  host, is a receiver thread starved of CPU: give the driver's threads CPU, then add
  the subscription again.
