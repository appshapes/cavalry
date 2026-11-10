//! A member's duty cycle over a driver allocates nothing, whatever its role: not the
//! module, not its client, and not the three drivers of this process, while the leader
//! appends an entry every cycle; and neither does the service beside each member,
//! which reads the committed log in every cycle, nor the replay the driver plays it
//! (`CLAUDE.md`, invariant 2; ADR 0065). A test of its own, since the counting
//! allocator is the whole process's.

use std::alloc::System;
use std::net::UdpSocket;
use std::time::{Duration, Instant};

use cavalry_client::{Cavalry, CommittedLog};
use cavalry_driver::Driver;
use cavalry_raft::{ConsensusModule, ElectionState, RaftError, Refused};
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

#[test]
fn three_members_run_their_duty_cycles_without_an_allocation() {
    let dir = std::env::temp_dir().join(format!(
        "cavalry-raft-{}-module-allocations",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let sockets: Vec<UdpSocket> = (0..15)
        .map(|_| UdpSocket::bind("127.0.0.1:0").expect("a loopback socket"))
        .collect();
    let ports: Vec<u16> = sockets
        .iter()
        .map(|socket| socket.local_addr().expect("bound").port())
        .collect();
    drop(sockets);
    let members = (0..3)
        .map(|member| {
            let endpoints: Vec<String> = ports[member * 5..member * 5 + 5]
                .iter()
                .map(|port| format!("127.0.0.1:{port}"))
                .collect();
            format!("{member},{}", endpoints.join(","))
        })
        .collect::<Vec<_>>()
        .join("|");
    let mut cluster: Vec<(Driver, ConsensusModule)> = (0..3)
        .map(|id| {
            let driver_dir = dir.join(format!("member-{id}"));
            let log_dir = dir.join(format!("member-{id}-log"));
            let driver = Driver::start(&driver_dir, &log_dir).expect("the driver starts");
            let module = ConsensusModule::start(&driver_dir, &log_dir, &members, id)
                .expect("the member starts");
            (driver, module)
        })
        .collect();

    let deadline = Instant::now() + Duration::from_secs(60);
    while !cluster
        .iter()
        .all(|(_, module)| module.election_state() == ElectionState::Closed)
    {
        assert!(Instant::now() < deadline, "the elections did not close");
        for (_, module) in &mut cluster {
            module.do_work().expect("the member's duty cycle");
        }
    }
    // The first entries and their commit, so that every path has run once: the
    // segment files are there, the counters have moved, the heartbeats are under way.
    // A commit waits for the recorders' sync, once a second.
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut cycles = 0;
    while cycles < 3_000 || cluster[2].1.commit_position() < 20 * 96 {
        assert!(Instant::now() < deadline, "nothing was committed");
        let _ = cluster[0].1.offer(&[1u8; 64]);
        for (_, module) in &mut cluster {
            module.do_work().expect("the member's duty cycle");
        }
        std::thread::sleep(Duration::from_micros(100));
        cycles += 1;
    }

    // A service beside each member, its log opened and read up to the commit position
    // before the count starts, so that its image has joined and its replay has run.
    let mut services: Vec<(Cavalry, CommittedLog)> = (0..3)
        .map(|id| {
            let mut client = Cavalry::connect_to(dir.join(format!("member-{id}")))
                .expect("the service's client connects");
            let log = client
                .committed_log(0)
                .expect("the driver answers")
                .expect("the member has its log");
            (client, log)
        })
        .collect();
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut warm = [0usize; 3];
    while warm.iter().any(|&read| read < 20) {
        assert!(Instant::now() < deadline, "the services read nothing");
        for (_, module) in &mut cluster {
            module.do_work().expect("the member's duty cycle");
        }
        for ((_, log), read) in services.iter_mut().zip(warm.iter_mut()) {
            *read += log.poll(|_, _, _| {}, 64).expect("the log polls");
        }
        std::thread::sleep(Duration::from_micros(100));
    }

    // Twenty thousand cycles of all three members and their services, an entry offered
    // in each, over more than two seconds, so that the recorders sync, the append
    // positions and the commit position move, every heartbeat falls due, and each
    // service reads what was committed.
    let before = cluster[0].1.commit_position();
    let mut offered = 0u32;
    let mut read = [0usize; 3];
    let mut bytes = 0usize;
    let region = Region::new(GLOBAL);
    for _ in 0..20_000 {
        match cluster[0].1.offer(&[2u8; 64]) {
            Ok(()) => offered += 1,
            Err(RaftError::Offer(Refused::BackPressured)) => {}
            Err(_) => offered = u32::MAX,
        }
        for (_, module) in &mut cluster {
            if module.do_work().is_err() {
                offered = u32::MAX;
            }
        }
        for ((_, log), read) in services.iter_mut().zip(read.iter_mut()) {
            match log.poll(|entry, _, _| bytes += entry.len(), 64) {
                Ok(entries) => *read += entries,
                Err(_) => offered = u32::MAX,
            }
        }
        std::thread::sleep(Duration::from_micros(100));
    }
    let change = region.change();
    assert_ne!(offered, u32::MAX, "a member's duty cycle failed");
    assert!(offered > 1_000, "only {offered} entries were appended");
    assert!(
        cluster[1].1.commit_position() > before,
        "the commit position did not move in the region"
    );
    assert!(
        read.iter().all(|&entries| entries > 1_000),
        "the services read {read:?} entries"
    );
    assert!(bytes > 0);
    assert_eq!(
        (change.allocations, change.reallocations),
        (0, 0),
        "a duty cycle or a service's poll allocated: {change:?}"
    );

    for (mut client, log) in services {
        log.close(&mut client).expect("the log closes");
        client.close().expect("the client closes");
    }

    for (driver, module) in cluster {
        module.close().expect("the member closes");
        driver.close().expect("the driver closes");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
