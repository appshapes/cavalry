# cavalry-client

The client API implementation, re-exported by the `cavalry` crate. Data plane.

## What this does

- Attaches a process to the driver through shared memory and creates publications (a
  handle for writing one stream) and subscriptions (a handle for reading one stream,
  possibly from several publishers) on channels such as `cavalry:udp?endpoint=host:port`
  and `cavalry:ipc`. IPC and UDP are the same API.
- Publishes by claim and commit: claim a slice of the log buffer, write into it in
  place, commit it. Nothing is allocated per message, and a codec encodes straight into
  the claimed slice.
- Delivers to the subscriber's fragment handler as a view into mapped memory, one image
  per connected publisher.
- Returns back-pressure as a result code when the receiver window or the log buffer is
  full. The caller decides what to do; the driver never buffers more.

In Aeron, this is the client, with `tryClaim` and `commit`
([glossary](../../docs/glossary.md)).

## What it must never do

- Lie about delivery (invariant 1). A stream is ordered; a message is delivered once, or
  its absence is reported to the subscriber.
- Allocate, lock, make a syscall, await or panic per message (invariant 2). The path
  from claim to the fragment handler is judged at p99.99 and max, never at the mean.
- Absorb back-pressure. It is explicit and returned to the caller, never hidden behind a
  growing buffer (invariant 3).
- Rename a concept. Publication, subscription, image, session, position and back-
  pressure keep the names in the glossary.

## Example

One process publishes and subscribes through the driver on this host: this is the
ten-minute path's program, and the crate's doctest. `cavalry run` in another terminal is
the usual driver; the doctest embeds one.

```rust
use cavalry_client::{Cavalry, Offer};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // A driver of our own for the doctest; `cavalry run` is the usual shape.
    let dir = std::env::temp_dir().join(format!("cavalry-client-readme-{}", std::process::id()));
    let driver = cavalry_driver::Driver::start(&dir)?;

    // `Cavalry::connect()` reads CAVALRY_DIR or the default directory; this names one.
    let mut client = Cavalry::connect_to(&dir)?;
    let mut subscription = client.add_subscription("cavalry:ipc", 1001)?;
    let mut publication = client.add_publication("cavalry:ipc", 1001)?;

    // Publish by claim and commit: write in place, nothing copied, nothing allocated.
    // Until the subscription's image joins, the driver answers `not-connected`.
    loop {
        match publication.try_claim(5) {
            Ok(mut claim) => {
                claim.body().copy_from_slice(b"hello");
                claim.commit();
                break;
            }
            Err(Offer::NotConnected | Offer::BackPressured | Offer::AdminAction) => {
                subscription.poll(|_, _| {}, 1)?;
            }
            Err(other) => return Err(other.into()),
        }
    }

    // Subscribe by polling: each fragment is a view into the log, with its header.
    let mut received = None;
    while received.is_none() {
        subscription.poll(|body, header| received = Some((body.to_vec(), header.session_id())), 10)?;
    }
    let (body, session) = received.unwrap();
    println!("received {:?} from session {session}", std::str::from_utf8(&body)?);

    publication.close()?;
    subscription.close()?;
    client.close()?;
    driver.close()?;
    std::fs::remove_dir_all(&dir).ok();
    Ok(())
}
```

Running it prints one line, such as `received "hello" from session -1716223105`. A
directory with no driver is refused with what to do:

```text
client refused (not-found): no driver at /tmp/nowhere; start one with `cavalry run`, or set CAVALRY_DIR to the directory of the driver to use; see docs/reference/client-protocol.md#the-driver-directory
```

## What to read next

- [`spec/client-protocol.md`](../../spec/client-protocol.md): how a client attaches, the
  commands it sends, the events it reads, and the timeouts its conductor keeps; and
  [ADR 0021](../../docs/decisions/0021-shape-the-client-api-after-aerons.md), the shape
  of this API and why its two threads share no lock.
- [`spec/log-buffer.md`](../../spec/log-buffer.md), "Claim, commit and abort" and
  "Reading": what a publication and a subscription do with a log.
- [Configuration](../../docs/reference/config.md): `CAVALRY_DIR`, the one key a client
  reads; and [`cavalry-driver`](../cavalry-driver/README.md), the driver it attaches to.
