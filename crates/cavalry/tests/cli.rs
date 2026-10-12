//! The binary, as a user runs it: no arguments and unknown commands exit 2 with the
//! usage; `run` starts a driver that `doctor` reports running, stops on SIGTERM and
//! leaves a directory that `doctor` reports closed (CLAUDE.md, invariant 5; ADR 0002).
//! `run` and `doctor` take no arguments, so these use the default directory; a driver
//! already running there makes `run` refuse, which is the right answer and this test's
//! one assumption.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn cavalry() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cavalry"))
}

#[test]
fn no_command_prints_the_usage_and_exits_2() {
    let output = cavalry().output().expect("runs");
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("usage: cavalry <command>"));

    let output = cavalry().arg("fly").output().expect("runs");
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("`fly` is not a command"));

    let output = cavalry().args(["doctor", "extra"]).output().expect("runs");
    assert_eq!(output.status.code(), Some(2));

    let output = cavalry().arg("--version").output().expect("runs");
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).starts_with("cavalry 0."));
}

#[test]
fn run_starts_a_driver_doctor_sees_and_sigterm_closes_it() {
    let mut run = cavalry()
        .arg("run")
        .stderr(Stdio::piped())
        .spawn()
        .expect("run starts");
    let deadline = Instant::now() + Duration::from_secs(10);
    let running = loop {
        let output = cavalry().arg("doctor").output().expect("doctor runs");
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        if output.status.success() && text.contains("is running") {
            break text;
        }
        if let Some(status) = run.try_wait().expect("try_wait") {
            let mut stderr = String::new();
            run.stderr
                .take()
                .expect("piped")
                .read_to_string(&mut stderr)
                .ok();
            panic!("run ended early with {status}: {stderr}");
        }
        assert!(
            Instant::now() < deadline,
            "doctor never saw the driver running: {text}"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(running.contains("checks run:"), "{running}");

    let status = Command::new("kill")
        .args(["-TERM", &run.id().to_string()])
        .status()
        .expect("kill runs");
    assert!(status.success());
    let exit = run.wait().expect("run exits");
    let mut stderr = String::new();
    run.stderr
        .take()
        .expect("piped")
        .read_to_string(&mut stderr)
        .ok();
    assert!(exit.success(), "run exited with {exit}: {stderr}");
    assert!(stderr.contains("driver started on"), "{stderr}");
    assert!(stderr.contains("closed;"), "{stderr}");

    let output = cavalry().arg("doctor").output().expect("doctor runs");
    assert_eq!(output.status.code(), Some(1));
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains("FAIL driver-heartbeat") && text.contains("closed"),
        "{text}"
    );
}
