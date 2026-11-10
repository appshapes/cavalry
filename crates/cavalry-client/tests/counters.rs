//! A client's counter through the public API (`spec/client-protocol.md`, version 7,
//! "Commands"; ADR 0062): the client adds a counter of a type `counters.toml` gives a
//! client, owns it and writes it, another client reads it, and it goes when its handle
//! is closed or dropped and when its client closes.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use cavalry_client::{Cavalry, ClientError, Closed};
use cavalry_driver::Driver;
use cavalry_proto::client_protocol::ErrorCode;
use cavalry_shm::counters::generated::{CLUSTER_COMMIT_POSITION, DRIVER_REGISTRY_FULL};

struct TestDir(PathBuf);

impl TestDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "cavalry-client-counters-{name}-{}",
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

/// Whether record `id` holds a counter, as a reader of the counters file sees it.
fn is_a_counter(reader: &Cavalry, id: u32) -> bool {
    reader
        .counters()
        .record(id)
        .expect("the record reads")
        .is_some()
}

#[test]
fn a_counter_is_owned_by_its_client_read_by_another_and_freed_with_its_handle() {
    let dir = TestDir::new("owned");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("starts");
    let mut owner = Cavalry::connect_to(dir.path()).expect("connects");
    let reader = Cavalry::connect_to(dir.path()).expect("connects");

    let member_id: i32 = 2;
    let counter = owner
        .add_counter(
            CLUSTER_COMMIT_POSITION.0,
            &member_id.to_le_bytes(),
            "cluster.commit_position 2",
        )
        .expect("added");
    assert_eq!(counter.type_id(), CLUSTER_COMMIT_POSITION.0);
    assert!(!counter.is_closed());
    // The record another client reads is the one the owner asked for.
    let record = reader
        .counters()
        .record(counter.id())
        .expect("the record reads")
        .expect("the counter is allocated");
    assert_eq!(record.type_id(), CLUSTER_COMMIT_POSITION.0);
    assert_eq!(record.key()[..4], member_id.to_le_bytes());
    assert_eq!(record.label(), "cluster.commit_position 2");
    // The owner alone writes the value, and a reader of the file sees each store.
    let value = reader.counters().value(counter.id()).expect("a reader");
    assert_eq!(value.load_acquire(), 0);
    counter.store_release(4096).expect("stored");
    assert_eq!(value.load_acquire(), 4096);
    assert_eq!(counter.get(), 4096);
    counter.set(8192).expect("set");
    counter.increment().expect("incremented");
    counter.add(31).expect("added");
    assert_eq!(value.get(), 8224);

    // A second counter of the same type and key is a counter of its own.
    let second = owner
        .add_counter(
            CLUSTER_COMMIT_POSITION.0,
            &member_id.to_le_bytes(),
            "cluster.commit_position 2",
        )
        .expect("added");
    assert_ne!(second.id(), counter.id());
    assert_ne!(second.registration_id(), counter.registration_id());

    // Closing the handle removes the counter; dropping one does the same.
    let (first_id, second_id) = (counter.id(), second.id());
    counter.close();
    assert!(
        wait_until(Duration::from_secs(5), || !is_a_counter(&reader, first_id)),
        "the closed counter's record was not freed"
    );
    assert!(is_a_counter(&reader, second_id));
    drop(second);
    assert!(
        wait_until(Duration::from_secs(5), || !is_a_counter(&reader, second_id)),
        "the dropped counter's record was not freed"
    );
    owner.close().expect("closes");
    reader.close().expect("closes");
    driver.close().expect("closes");
}

#[test]
fn a_counter_goes_with_its_client_and_stores_nothing_after() {
    let dir = TestDir::new("closed");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("starts");
    let mut owner = Cavalry::connect_to(dir.path()).expect("connects");
    let reader = Cavalry::connect_to(dir.path()).expect("connects");
    let counter = owner
        .add_counter(
            CLUSTER_COMMIT_POSITION.0,
            &7i32.to_le_bytes(),
            "cluster.commit_position 7",
        )
        .expect("added");
    counter.store_release(64).expect("stored");
    let id = counter.id();
    let value = reader.counters().value(id).expect("a reader");
    // The client closes with the handle still held: the driver frees the counter with
    // the client, and the handle stores nothing from then on, since the record may be
    // another counter's by the time it would.
    owner.close().expect("closes");
    assert!(counter.is_closed());
    assert!(matches!(
        counter.store_release(128),
        Err(ClientError::Closed(Closed::ByUser))
    ));
    assert!(matches!(
        counter.set(128),
        Err(ClientError::Closed(Closed::ByUser))
    ));
    assert!(matches!(
        counter.increment(),
        Err(ClientError::Closed(Closed::ByUser))
    ));
    assert!(matches!(
        counter.add(1),
        Err(ClientError::Closed(Closed::ByUser))
    ));
    assert_eq!(
        value.load_acquire(),
        64,
        "nothing was stored after the close"
    );
    assert!(
        wait_until(Duration::from_secs(5), || !is_a_counter(&reader, id)),
        "the closed client's counter was not freed"
    );
    drop(counter);
    reader.close().expect("closes");
    driver.close().expect("closes");
}

#[test]
fn the_refusals_of_add_counter_carry_their_codes_and_register_nothing() {
    let dir = TestDir::new("refused");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("starts");
    let mut client = Cavalry::connect_to(dir.path()).expect("connects");
    // A key or a label past its bound is refused on this side, before a byte is sent.
    for (key, label) in [
        (&[0u8; 113][..], "cluster.commit_position 2"),
        (&[2u8, 0, 0, 0][..], &"l".repeat(125)[..]),
    ] {
        match client.add_counter(CLUSTER_COMMIT_POSITION.0, key, label) {
            Err(ClientError::Protocol(error)) => assert_eq!(error.reason(), "counter"),
            other => panic!("expected the counter check, got {other:?}"),
        }
    }
    // A type that is not a client's, here one of the driver's own, is code 11 with the
    // page that lists the ones a client adds.
    match client.add_counter(DRIVER_REGISTRY_FULL.0, &[], "driver.registry_full") {
        Err(ClientError::Refused { code, message, .. }) => {
            assert_eq!(code, Some(ErrorCode::GenericError), "{message}");
            assert!(message.contains("docs/reference/counters.md"), "{message}");
        }
        other => panic!("expected code 11, got {other:?}"),
    }
    // None of the three registered a counter: the driver's bound is still whole.
    let held: Vec<_> = (0..256i32)
        .map(|member_id| {
            client
                .add_counter(
                    CLUSTER_COMMIT_POSITION.0,
                    &member_id.to_le_bytes(),
                    &format!("cluster.commit_position {member_id}"),
                )
                .expect("the registry holds 256")
        })
        .collect();
    // The 257th is code 10 and names the registry; one closed makes room again.
    match client.add_counter(CLUSTER_COMMIT_POSITION.0, &[], "cluster.commit_position") {
        Err(ClientError::Refused { code, message, .. }) => {
            assert_eq!(
                code,
                Some(ErrorCode::ResourceTemporarilyUnavailable),
                "{message}"
            );
            assert!(message.contains("client counters registry"), "{message}");
        }
        other => panic!("expected code 10, got {other:?}"),
    }
    let mut held = held;
    held.pop().expect("a counter").close();
    let again = wait_until(Duration::from_secs(5), || {
        client
            .add_counter(CLUSTER_COMMIT_POSITION.0, &[], "cluster.commit_position")
            .is_ok()
    });
    assert!(again, "a closed counter did not make room for another");
    drop(held);
    client.close().expect("closes");
    driver.close().expect("closes");
}
