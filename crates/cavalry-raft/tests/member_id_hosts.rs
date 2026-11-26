//! A member id started a second time (`spec/cluster.md`, "Members and streams"; work
//! item 470). A member binds its own endpoints of the members string, each at the
//! address its host resolves the endpoint's host to, with no option that lets a second
//! socket share it ([`cavalry_driver`]'s receive endpoint), and a refused bind ends its
//! start with error code 4. So on one host a member id runs once: a second driver
//! finds the endpoint in use, and a second module on the first's driver finds the log
//! directory held (`tests/log_directory.rs`) or, on a directory of its own, a directory
//! that is not its driver's. A host that is not the endpoint's, by an address literal
//! or by a name that resolves to another host's address, cannot bind it at all. Only
//! hosts that each hold the address can both run the member id: a name each resolves to
//! itself, or an address that moves from one to the other, which the in-memory cluster
//! holds (`tests/member_id_twice.rs`).

use std::path::{Path, PathBuf};

use cavalry_driver::Driver;
use cavalry_raft::{ConsensusModule, RaftError};

mod ports;
use ports::free_ports;

struct TestDir(PathBuf);

impl TestDir {
    fn new(test: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "cavalry-raft-member-id-hosts-{}-{test}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        TestDir(path)
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

/// The members string of three members, member 1's endpoints at `host` and the
/// others' on the loopback, and member 1's consensus endpoint, the first it binds.
fn members_string(host: &str) -> (String, String) {
    let ports = free_ports(15);
    let members = (0..3)
        .map(|member| {
            let host = if member == 1 { host } else { "127.0.0.1" };
            let endpoints: Vec<String> = ports[member * 5..member * 5 + 5]
                .iter()
                .map(|port| format!("{host}:{port}"))
                .collect();
            format!("{member},{}", endpoints.join(","))
        })
        .collect::<Vec<_>>()
        .join("|");
    (members, format!("{host}:{}", ports[6]))
}

/// The refusal of a bind: error code 4 from the driver, naming the endpoint, with the
/// operating system's reason, which reads `reason` on Linux and on macOS.
fn assert_bind_refused(refused: Result<ConsensusModule, RaftError>, endpoint: &str, reason: &str) {
    let error = match refused {
        Ok(_) => panic!("a member started on an endpoint it cannot bind"),
        Err(error) => error,
    };
    assert_eq!(error.reason(), "client", "{error}");
    let text = error.to_string();
    assert!(text.contains("error code 4"), "{text}");
    assert!(
        text.contains(&format!("the channel's endpoint {endpoint}:")),
        "{text}"
    );
    assert!(text.contains(reason), "{text}");
    assert!(
        text.contains("check that the address is this host's and its port is free"),
        "{text}"
    );
}

/// On one host, a member id runs once. The member's driver binds its endpoints; a
/// second module of the member id on that driver is refused, on a log directory of its
/// own, since a member's log directory is its driver's; and a member started with a
/// driver of its own finds the endpoints in use.
#[test]
fn a_member_id_runs_once_on_one_host() {
    let dir = TestDir::new("one-host");
    let (members, consensus) = members_string("127.0.0.1");
    let (driver_dir, log_dir) = (dir.path().join("driver"), dir.path().join("log"));
    let _driver = Driver::start(&driver_dir, &log_dir).expect("the driver starts");
    let first = ConsensusModule::start(&driver_dir, &log_dir, &members, 1)
        .expect("the first module starts");

    // A second module on the first's driver, on another log directory.
    let other = dir.path().join("other-log");
    let refused = match ConsensusModule::start(&driver_dir, &other, &members, 1) {
        Ok(_) => panic!("a second module of the member id on one driver started"),
        Err(error) => error,
    };
    let text = refused.to_string();
    assert!(
        text.contains("Start the member with the log directory its driver was started with"),
        "{text}"
    );

    // A second driver on the host, and the member id started on it.
    let (second_dir, second_log) = (dir.path().join("driver-2"), dir.path().join("log-2"));
    let _second = Driver::start(&second_dir, &second_log).expect("a second driver starts");
    assert_bind_refused(
        ConsensusModule::start(&second_dir, &second_log, &members, 1),
        &consensus,
        "in use",
    );
    drop(first);
}

/// A member whose endpoints name an address that is not its host's is refused at its
/// start: an address literal of another host, as here an address of TEST-NET-1 (RFC
/// 5737) that no host holds, and a host name that resolves to one, since the driver
/// binds the address it resolved.
#[test]
fn a_member_whose_endpoints_are_another_hosts_is_refused_at_its_start() {
    // A host that binds an address it does not hold (Linux's `ip_nonlocal_bind`) or
    // that holds this one cannot show the refusal: the test says so and ends.
    if std::net::UdpSocket::bind(("192.0.2.1", 0)).is_ok() {
        eprintln!("this host binds 192.0.2.1, so it shows no refusal of another host's address");
        return;
    }
    let dir = TestDir::new("not-this-host");
    let (members, consensus) = members_string("192.0.2.1");
    let (driver_dir, log_dir) = (dir.path().join("driver"), dir.path().join("log"));
    let _driver = Driver::start(&driver_dir, &log_dir).expect("the driver starts");
    assert_bind_refused(
        ConsensusModule::start(&driver_dir, &log_dir, &members, 1),
        &consensus,
        "assign requested address",
    );
}
