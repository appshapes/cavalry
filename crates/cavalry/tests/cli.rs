//! The binary, as a user runs it: no arguments and unknown commands exit 2 with the
//! usage; `run` starts a driver that `doctor` reports running, stops on SIGTERM and
//! leaves a directory that `doctor` reports closed (CLAUDE.md, invariant 5; ADR 0002);
//! the directory comes from `--dir` or `CAVALRY_DIR`, so that these tests never touch
//! the default directory, and a value a key does not take is refused before anything
//! starts (ADR 0020); `replay` prints each frame's position from its header, across a
//! padding frame, and exits 1 naming the positions it did not print when the replay's
//! log went before it had read to the end (invariant 1; ADR 0030).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use cavalry::{Cavalry, Driver, Offer};

fn cavalry() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_cavalry"));
    // The tests set what they test; the developer's shell does not leak in. A test that
    // runs a driver sets XDG_DATA_HOME to its own directory, so the default log directory
    // (`Driver::default_log_dir`) lands there and never under the developer's home.
    command.env_remove("CAVALRY_DIR");
    command.env_remove("CAVALRY_CONDUCTOR_IDLE_STRATEGY");
    command.env_remove("XDG_DATA_HOME");
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
        .env("XDG_DATA_HOME", dir.arg())
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
        .env("XDG_DATA_HOME", from_env.arg())
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
        .env("XDG_DATA_HOME", from_flag.arg())
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

/// `cavalry record` against a running driver with nothing publishing: the start and the
/// stop are refused with the driver's own messages and codes, exit 1; `--log-dir` names
/// the log directory on `run`, and `doctor --log-dir` checks it without failing.
#[test]
fn record_is_refused_with_the_drivers_message_when_nothing_publishes() {
    let dir = TestDir::new("record");
    let log_dir = format!("{}-log", dir.arg());
    let _ = std::fs::remove_dir_all(&log_dir);
    let mut run = cavalry()
        .args(["run", "--dir", &dir.arg(), "--log-dir", &log_dir])
        .stderr(Stdio::piped())
        .spawn()
        .expect("run starts");
    wait_until_running(&mut run, || {
        let mut c = cavalry();
        c.args(["doctor", "--dir", &dir.arg()]);
        c
    });
    let output = cavalry()
        .args(["record", "61", "--dir", &dir.arg()])
        .output()
        .expect("record runs");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no live publication on stream 61"),
        "{stderr}"
    );
    let output = cavalry()
        .args(["record", "--stop", "7", "--dir", &dir.arg()])
        .output()
        .expect("record runs");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not in the catalog"), "{stderr}");
    let output = cavalry()
        .args(["doctor", "--dir", &dir.arg(), "--log-dir", &log_dir])
        .output()
        .expect("doctor runs");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        Path::new(&log_dir).join("catalog").is_file(),
        "the driver opened the log directory"
    );
    let stderr = terminate(run);
    assert!(
        stderr.contains(&format!("recording into {log_dir}")),
        "{stderr}"
    );
    let _ = std::fs::remove_dir_all(&log_dir);
}

/// Waits up to ten seconds for `done`.
fn wait(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "{what}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// A driver in this process on `dir`'s `driver` and `log` directories, a client, and
/// recording 0 of `messages` messages of 32 letters each on stream `stream_id`, the
/// letters `a`, `b` and on, with a padding frame of 64 bytes after the first message
/// when `padding` is set, which an aborted claim leaves (`spec/log-buffer.md`, "Claim,
/// commit and abort"); the recording is stopped at its end, which is returned.
fn recorded(
    dir: &TestDir,
    stream_id: i32,
    messages: usize,
    padding: bool,
) -> (Driver, Cavalry, i64) {
    let driver = Driver::start(dir.0.join("driver"), dir.0.join("log")).expect("the driver starts");
    let mut client = Cavalry::connect_to(dir.0.join("driver")).expect("connects");
    let mut publication = client
        .add_exclusive_publication("cavalry:ipc", stream_id)
        .expect("a publication");
    let recording = client
        .start_recording("cavalry:ipc", stream_id)
        .expect("a recording");
    let mut after = 0;
    for n in 0..messages {
        let body = [b'a' + (n % 26) as u8; 32];
        wait("the offer goes through", || {
            publication
                .offer(&body)
                .map(|position| after = position)
                .is_ok()
        });
        if padding && n == 0 {
            wait("the claim goes through", || {
                match publication.try_claim(32) {
                    Ok(claim) => {
                        after = claim.abort();
                        true
                    }
                    Err(Offer::BackPressured | Offer::AdminAction | Offer::NotConnected) => false,
                    Err(other) => panic!("{other}"),
                }
            });
        }
    }
    wait("the recording is synced", || {
        recording.synced_position() == after
    });
    client.stop_recording(0).expect("stops");
    publication.close().expect("closes");
    (driver, client, after)
}

/// `cavalry replay` against a driver in this process, on its directories.
fn replay(dir: &TestDir) -> Command {
    let mut command = cavalry();
    command.args([
        "replay",
        "--recording",
        "0",
        "--from",
        "0",
        "--dir",
        &dir.0.join("driver").to_string_lossy(),
        "--log-dir",
        &dir.0.join("log").to_string_lossy(),
    ]);
    command
}

/// Each line's position is the frame's, from its header: a padding frame between two
/// messages prints nothing and moves the next line's position past it, where a sum of
/// the lines' lengths would be 64 short (invariant 1; `spec/log-buffer.md`, "Positions").
#[test]
fn replay_prints_each_frames_position_from_its_header_across_a_padding_frame() {
    let dir = TestDir::new("replay-padding");
    let (driver, client, end) = recorded(&dir, 81, 2, true);
    assert_eq!(end, 192, "two frames of 64 and a padding frame of 64");
    let output = replay(&dir).output().expect("replay runs");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 3, "{stdout}");
    assert_eq!(lines[0], format!("0 {}", "a".repeat(32)));
    assert_eq!(lines[1], format!("128 {}", "b".repeat(32)));
    assert!(
        lines[2].contains("ended at position 192: 2 messages from position 0")
            && !lines[2].contains("read to"),
        "{stdout}"
    );
    client.close().expect("closes");
    driver.close().expect("closes");
}

/// A command whose stdout nobody reads: it blocks once the pipe is full, the replay
/// ends, the driver keeps the log for the publication linger, 5 s, then removes it; the
/// command says which positions it did not print, names the position to replay from,
/// and exits 1, never a summary that reads as complete (invariant 1).
#[test]
fn replay_exits_1_and_names_the_positions_not_printed_when_its_log_goes_first() {
    let dir = TestDir::new("replay-unread");
    let (driver, client, end) = recorded(&dir, 82, 8000, false);
    assert_eq!(end, 8000 * 64);
    let child = replay(&dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("replay starts");
    // 8000 lines of 40 bytes and more are far over a pipe's 64 KiB: the command blocks
    // on its stdout while the replay ends and its log lingers for 5 s and goes.
    std::thread::sleep(Duration::from_secs(7));
    let output = child.wait_with_output().expect("replay exits");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(1),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let last = stdout.lines().last().expect("a summary");
    assert!(
        last.contains(&format!("ended at position {end}:")) && last.contains(", read to position "),
        "{last}"
    );
    let read_to: i64 = last
        .rsplit("read to position ")
        .next()
        .and_then(|rest| rest.trim().parse().ok())
        .expect("the position read to");
    assert!(
        read_to > 0 && read_to < end && read_to % 64 == 0,
        "{read_to}"
    );
    assert_eq!(
        stdout.lines().count() as i64 - 1,
        read_to / 64,
        "every line up to the position read to, and none after"
    );
    assert!(
        stderr.contains(&format!("positions {read_to} to {end}"))
            && stderr.contains(&format!("--from {read_to}"))
            && stderr.contains("docs/how-to/replay-a-recording.md#when-it-does-not-work"),
        "{stderr}"
    );
    client.close().expect("closes");
    driver.close().expect("closes");
}
