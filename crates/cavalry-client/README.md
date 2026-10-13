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

Attach to the driver running on this host, see the client id the driver knows this
process by, and close. `cavalry run` in another terminal is the usual driver; the
doctest embeds one. Publications and subscriptions arrive with the next commits of card
87, and this example grows into the ten-minute path.

```rust
use cavalry_client::Cavalry;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // A driver of our own for the doctest; `cavalry run` is the usual shape.
    let dir = std::env::temp_dir().join(format!("cavalry-client-readme-{}", std::process::id()));
    let driver = cavalry_driver::Driver::start(&dir)?;

    // `Cavalry::connect()` reads CAVALRY_DIR or the default directory; this names one.
    let client = Cavalry::connect_to(&dir)?;
    println!("client {} attached to {}", client.client_id(), client.dir().display());
    assert!(!client.is_closed());

    // While the client lives, its conductor thread keeps it alive with the driver.
    client.close()?;
    driver.close()?;
    std::fs::remove_dir_all(&dir).ok();
    Ok(())
}
```

Running it prints one line, such as `client 3 attached to /tmp/cavalry-client-readme-4242`.
A directory with no driver is refused with what to do:

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
