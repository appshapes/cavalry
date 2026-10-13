//! The binary, as a user runs it: no arguments and unknown commands exit 2 with the
//! usage; `run` starts a driver that `doctor` reports running, stops on SIGTERM and
//! leaves a directory that `doctor` reports closed (CLAUDE.md, invariant 5; ADR 0002);
//! the directory comes from `--dir` or `CAVALRY_DIR`, so that these tests never touch
//! the default directory, and a value a key does not take is refused before anything
//! starts (ADR 0020).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn cavalry() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_cavalry"));
    // The tests set what they test; the developer's shell does not leak in.
    command.env_remove("CAVALRY_DIR");
    command.env_remove("CAVALRY_CONDUCTOR_IDLE_STRATEGY");
    command
}

/// A directory of its own per test, removed when the guard drops.
struct TestDir(PathBuf);

impl TestDir {
    fn new(test: &str) -> Self {
        let path = std::env::temp_dir().join(format!("cavalry-cli-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        TestDir(path)
    }

    fn arg(&self) -> String {
        self.0.to_string_lossy().into_owned()
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn stderr_of(child: &mut Child) -> String {
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        pipe.read_to_string(&mut stderr).ok();
    }
    stderr
}

/// Polls `doctor` on `dir` until it reports the driver running, or `run` ends early.
fn wait_until_running(run: &mut Child, doctor: impl Fn() -> Command) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let output = doctor().output().expect("doctor runs");
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        if output.status.success() && text.contains("is running") {
            return text;
        }
        if let Some(status) = run.try_wait().expect("try_wait") {
            panic!("run ended early with {status}: {}", stderr_of(run));
        }
        assert!(
            Instant::now() < deadline,
            "doctor never saw the driver running: {text}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn terminate(mut run: Child) -> String {
    let status = Command::new("kill")
        .args(["-TERM", &run.id().to_string()])
        .status()
        .expect("kill runs");
    assert!(status.success());
    let exit = run.wait().expect("run exits");
    let stderr = stderr_of(&mut run);
    assert!(exit.success(), "run exited with {exit}: {stderr}");
    stderr
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
    assert!(String::from_utf8_lossy(&output.stderr).contains("--dir <path>"));

    let output = cavalry().args(["doctor", "--dir"]).output().expect("runs");
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--dir needs a path"));

    let output = cavalry().arg("--version").output().expect("runs");
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).starts_with("cavalry 0."));

    let output = cavalry().arg("--help").output().expect("runs");
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("CAVALRY_DIR") && text.contains("docs/reference/config.md"));
}

#[test]
fn run_on_a_dir_flag_starts_a_driver_doctor_sees_and_sigterm_closes_it() {
    let dir = TestDir::new("flag");
    let mut run = cavalry()
        .args(["run", "--dir", &dir.arg()])
        .stderr(Stdio::piped())
        .spawn()
        .expect("run starts");
    let running = wait_until_running(&mut run, || {
        let mut c = cavalry();
        c.args(["doctor", &format!("--dir={}", dir.arg())]);
        c
    });
    assert!(running.contains("checks run:"), "{running}");
    assert!(running.contains(&dir.arg()), "{running}");
    for file in ["counters", "command-ring", "broadcast-buffer"] {
        assert!(dir.0.join(file).is_file(), "{file}");
    }

    let stderr = terminate(run);
    assert!(stderr.contains("driver started on"), "{stderr}");
    assert!(
        stderr.contains("conductor idle strategy backoff"),
        "{stderr}"
    );
    assert!(stderr.contains("closed;"), "{stderr}");

    let output = cavalry()
        .args(["doctor", "--dir", &dir.arg()])
        .output()
        .expect("doctor runs");
    assert_eq!(output.status.code(), Some(1));
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains("FAIL driver-heartbeat") && text.contains("closed"),
        "{text}"
    );
}

#[test]
fn the_environment_sets_the_directory_and_the_idle_strategy_and_the_flag_wins() {
    let from_env = TestDir::new("env");
    let from_flag = TestDir::new("env-flag");
    let mut run = cavalry()
        .arg("run")
        .env("CAVALRY_DIR", from_env.arg())
        .env("CAVALRY_CONDUCTOR_IDLE_STRATEGY", "yield")
        .stderr(Stdio::piped())
        .spawn()
        .expect("run starts");
    wait_until_running(&mut run, || {
        let mut c = cavalry();
        c.arg("doctor").env("CAVALRY_DIR", from_env.arg());
        c
    });
    let stderr = terminate(run);
    assert!(stderr.contains("conductor idle strategy yield"), "{stderr}");
    assert!(from_env.0.join("counters").is_file());

    // The flag wins over the variable.
    let mut run = cavalry()
        .args(["run", "--dir", &from_flag.arg()])
        .env("CAVALRY_DIR", from_env.arg())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run starts");
    wait_until_running(&mut run, || {
        let mut c = cavalry();
        c.args(["doctor", "--dir", &from_flag.arg()]);
        c
    });
    let stderr = terminate(run);
    assert!(
        stderr.contains(&format!("driver started on {}", from_flag.arg())),
        "{stderr}"
    );
}

#[test]
fn a_value_a_key_does_not_take_stops_the_command_before_anything_starts() {
    let dir = TestDir::new("invalid");
    for command in ["run", "doctor"] {
        let output = cavalry()
            .args([command, "--dir", &dir.arg()])
            .env("CAVALRY_CONDUCTOR_IDLE_STRATEGY", "spin")
            .output()
            .expect("runs");
        assert_eq!(output.status.code(), Some(2), "{command}");
        let text = String::from_utf8_lossy(&output.stderr);
        assert!(
            text.contains("CAVALRY_CONDUCTOR_IDLE_STRATEGY is `spin`")
                && text.contains("one of backoff, yield, busy-spin")
                && text.contains("docs/reference/config.md#conductoridlestrategy"),
            "{text}"
        );
        assert!(
            !Path::new(&dir.0).exists(),
            "{command} created the directory"
        );
    }
}
