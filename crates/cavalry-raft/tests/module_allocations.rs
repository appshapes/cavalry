//! A member's duty cycle over a driver allocates nothing, whatever its role: not the
//! module, not its client, and not the three drivers of this process, while the leader
//! appends an entry every cycle; and neither does the service beside each member,
//! which reads the committed log in every cycle, nor the replay the driver plays it
//! (`CLAUDE.md`, invariant 2; ADR 0066). A test of its own, since the counting
//! allocator is the whole process's.

use std::alloc::System;
use std::time::{Duration, Instant};

use cavalry_client::{Cavalry, CommittedLog};
use cavalry_driver::Driver;
use cavalry_raft::{ConsensusModule, ElectionState, RaftError, Refused, Role};
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

mod ports;

#[test]
fn three_members_run_their_duty_cycles_without_an_allocation() {
    let dir = std::env::temp_dir().join(format!(
        "cavalry-raft-{}-module-allocations",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    // Fifteen ports, taken as `ports/mod.rs` says and for its reason.
    let ports = ports::free_ports(15);
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
    // Started in the order 1, 2, 0, so that no member's place is its id, before the
    // leader is put first or after: the leader is found below, never taken for the
    // first.
    let mut cluster: Vec<(Driver, ConsensusModule)> = [1, 2, 0]
        .into_iter()
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
    // The member that leads, first: every offer below goes through `cluster[0]`,
    // whichever member the cluster has for a leader (ADR 0100, change 3).
    let leader = cluster
        .iter()
        .position(|(_, module)| module.role() == Role::Leader)
        .expect("one member leads");
    cluster.swap(0, leader);
    // The first entries and their commit, so that every path has run once: the
    // segment files are there, the counters have moved, the heartbeats are under way.
    // A commit waits for one sync on a quorum, which each member asks its recorder for.
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
    // A follower that joined its leader's log late holds what lies below its live image
    // in its recording of the replay it caught up by, and a handle opened at 0 beside
    // it ends at that recording's stop. Its service opens the log again where the
    // handle ended, as any service does (ADR 0103), and does so here, before the
    // count starts, so that every handle counted is on its member's live recording.
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut warm = [0usize; 3];
    while warm.iter().any(|&read| read < 20) {
        assert!(Instant::now() < deadline, "the services read nothing");
        for (_, module) in &mut cluster {
            module.do_work().expect("the member's duty cycle");
        }
        for ((client, log), read) in services.iter_mut().zip(warm.iter_mut()) {
            *read += log.poll(|_, _, _| {}, 64).expect("the log polls");
            if log.is_ended() {
                let next = client
                    .committed_log(log.position())
                    .expect("the driver answers")
                    .expect("the member has its log");
                std::mem::replace(log, next)
                    .close(client)
                    .expect("the ended handle closes");
            }
        }
        std::thread::sleep(Duration::from_micros(100));
    }
    for (_, log) in &services {
        let (_, module) = cluster
            .iter()
            .find(|(_, module)| module.member_id() == log.member_id())
            .expect("the service's member");
        assert_eq!(
            Some(log.recording_id()),
            module.log_recording_id(),
            "member {}'s service reads its live recording",
            log.member_id()
        );
        assert!(!log.is_ended());
    }

    // Twenty thousand cycles of all three members and their services, an entry offered
    // in each, over more than two seconds, so that the members ask for their syncs,
    // the recorders sync, the append positions and the commit position move, every
    // heartbeat falls due, and each service reads what was committed.
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
