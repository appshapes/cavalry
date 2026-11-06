//! A publication on a control channel through the public API (ADR 0053, ADR 0054): the
//! client adds two destinations, each a subscription on a loopback endpoint of this
//! driver, every message reaches both once and in order, and the commands' refusals
//! carry the codes `spec/client-protocol.md`, "Commands", gives them.

use std::net::UdpSocket;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use cavalry_client::{Cavalry, ClientError, Publication, Subscription};
use cavalry_driver::Driver;

struct TestDir(PathBuf);

impl TestDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "cavalry-client-destinations-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the test directory");
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while !condition() {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    true
}

fn free_port() -> u16 {
    UdpSocket::bind("127.0.0.1:0")
        .expect("a loopback socket")
        .local_addr()
        .expect("bound")
        .port()
}

/// Offers until the window lets the message through, within five seconds.
fn offer(publication: &mut Publication, message: &[u8]) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match publication.offer(message) {
            Ok(_) => return,
            Err(error) if error.reason() == "back-pressured" || error.reason() == "admin-action" => {
                assert!(Instant::now() < deadline, "the offer did not go through: {error}");
                std::thread::sleep(Duration::from_micros(50));
            }
            Err(error) => panic!("offer: {error}"),
        }
    }
}

/// Polls until the `count` messages from id `from` have arrived, each the next in
/// order, within `timeout`; returns how many came.
fn drain_in_order(
    subscription: &mut Subscription,
    from: u64,
    count: u64,
    timeout: Duration,
) -> u64 {
    let deadline = Instant::now() + timeout;
    let mut next = from;
    while next < from + count {
        let before = next;
        subscription
            .poll(
                |buffer, _| {
                    let mut id = [0u8; 8];
                    id.copy_from_slice(&buffer[..8]);
                    assert_eq!(u64::from_le_bytes(id), next, "in order");
                    next += 1;
                },
                16,
            )
            .expect("the poll");
        if next == before {
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_micros(50));
        }
    }
    next - from
}

fn code_of(result: Result<(), ClientError>) -> i32 {
    match result {
        Err(ClientError::Refused { raw_code, .. }) => raw_code,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// Two subscriptions on two loopback endpoints, a publication on a control channel
/// under `fc=min`, both endpoints added as destinations: an image on each, every
/// message at both once and in order; a duplicate add and a remove of one not held are
/// success; after `remove_destination` the removed subscription gets nothing more.
#[test]
fn a_control_channel_reaches_every_destination_once_and_in_order() {
    let dir = TestDir::new("fan-out");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("the driver starts");
    let mut client = Cavalry::connect_to(dir.path()).expect("connects");
    let endpoints: Vec<String> = (0..2)
        .map(|_| format!("cavalry:udp?endpoint=127.0.0.1:{}", free_port()))
        .collect();
    let mut subscriptions: Vec<Subscription> = endpoints
        .iter()
        .map(|channel| client.add_subscription(channel, 7).expect("a subscription"))
        .collect();
    let control = format!(
        "cavalry:udp?control=127.0.0.1:{}|control-mode=manual|fc=min",
        free_port()
    );
    let mut publication = client.add_publication(&control, 7).expect("a publication");
    assert_eq!(publication.channel(), control);
    let id = publication.registration_id();
    for channel in &endpoints {
        client.add_destination(id, channel).expect("added");
    }
    client
        .add_destination(id, &endpoints[0])
        .expect("a duplicate add is success");
    client
        .remove_destination(id, "cavalry:udp?endpoint=127.0.0.1:1")
        .expect("a remove of one not held is success");
    assert!(
        wait_until(Duration::from_secs(5), || publication.is_connected()),
        "the destinations' status messages connected the publication"
    );
    for subscription in &mut subscriptions {
        assert!(wait_until(Duration::from_secs(5), || {
            subscription.poll(|_, _| {}, 0).expect("the poll applies");
            subscription.image_count() == 1
        }));
        assert_eq!(subscription.images()[0].session_id(), publication.session_id());
    }
    const MESSAGES: u64 = 500;
    for i in 0..MESSAGES {
        let mut message = [0u8; 32];
        message[..8].copy_from_slice(&i.to_le_bytes());
        offer(&mut publication, &message);
        // Both read as they go, so that neither window holds the slowest-paced sender.
        for subscription in &mut subscriptions {
            subscription.poll(|_, _| {}, 0).expect("the poll applies");
        }
    }
    // Each subscription read nothing above: the frames wait in the images.
    for (index, subscription) in subscriptions.iter_mut().enumerate() {
        assert_eq!(
            drain_in_order(subscription, 0, MESSAGES, Duration::from_secs(10)),
            MESSAGES,
            "destination {index}"
        );
    }
    // The second removed: the next message reaches the first alone.
    client
        .remove_destination(id, &endpoints[1])
        .expect("removed");
    let mut message = [0u8; 32];
    message[..8].copy_from_slice(&MESSAGES.to_le_bytes());
    offer(&mut publication, &message);
    assert_eq!(
        drain_in_order(&mut subscriptions[0], MESSAGES, 1, Duration::from_secs(5)),
        1
    );
    let mut arrived = 0;
    assert!(!wait_until(Duration::from_millis(300), || {
        subscriptions[1]
            .poll(|_, _| arrived += 1, 16)
            .expect("the poll");
        arrived > 0
    }));
    publication.close().expect("closes");
    for subscription in subscriptions {
        subscription.close().expect("closes");
    }
    client.close().expect("closes");
    driver.close().expect("closes");
}

/// The refusals, through the public API's codes (`spec/client-protocol.md`,
/// "Commands"): a publication with no control address is code 1, a registration this
/// client does not hold is code 3, a destination of the other family or one naming
/// more than its endpoint is code 1, and a name with no address is code 9.
#[test]
fn destination_commands_are_refused_with_the_spec_codes() {
    let dir = TestDir::new("refusals");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("the driver starts");
    let mut client = Cavalry::connect_to(dir.path()).expect("connects");
    let unicast = format!("cavalry:udp?endpoint=127.0.0.1:{}", free_port());
    let plain = client.add_publication(&unicast, 7).expect("a publication");
    assert_eq!(code_of(client.add_destination(plain.registration_id(), &unicast)), 1);
    assert_eq!(code_of(client.add_destination(999, &unicast)), 3);
    let control = format!(
        "cavalry:udp?control=127.0.0.1:{}|control-mode=manual",
        free_port()
    );
    let mdc = client.add_publication(&control, 8).expect("a publication");
    let id = mdc.registration_id();
    assert_eq!(code_of(client.add_destination(id, "cavalry:ipc")), 1);
    assert_eq!(code_of(client.add_destination(id, &control)), 1);
    assert_eq!(
        code_of(client.add_destination(id, "cavalry:udp?endpoint=[::1]:40123")),
        1
    );
    assert_eq!(
        code_of(client.add_destination(id, "cavalry:udp?endpoint=127.0.0.1:40123|fc=min")),
        1
    );
    assert_eq!(
        code_of(client.add_destination(id, "cavalry:udp?endpoint=no-such-host.invalid:40123")),
        9
    );
    client
        .add_destination(id, "cavalry:udp?endpoint=127.0.0.1:40123")
        .expect("a loopback destination beside a loopback control address");
    client
        .remove_destination(id, "cavalry:udp?endpoint=127.0.0.1:40123")
        .expect("removed");
    mdc.close().expect("closes");
    plain.close().expect("closes");
    client.close().expect("closes");
    driver.close().expect("closes");
}
