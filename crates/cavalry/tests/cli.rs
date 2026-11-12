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

use cavalry::{Cavalry, Driver, Offer, Publication, Recording};

fn cavalry() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_cavalry"));
    // The tests set what they test; the developer's shell does not leak in. A test that
    // runs a driver sets XDG_DATA_HOME to its own directory, so the default log directory
    // (`Driver::default_log_dir`) lands there and never under the developer's home.
    command.env_remove("CAVALRY_DIR");
    command.env_remove("CAVALRY_CONDUCTOR_IDLE_STRATEGY");
    command.env_remove("CAVALRY_CLUSTER_MEMBERS");
    command.env_remove("CAVALRY_CLUSTER_MEMBER_ID");
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
/// recording 0, live, of `messages` messages of 32 letters each on stream `stream_id`,
/// the letters `a`, `b` and on, with a padding frame of 64 bytes after the first
/// message when `padding` is set, which an aborted claim leaves (`spec/log-buffer.md`,
/// "Claim, commit and abort"); the recording is synced to its end, which is returned
/// with the publication and the recording's handle, for a test that follows it live.
fn recording(
    dir: &TestDir,
    stream_id: i32,
    messages: usize,
    padding: bool,
) -> (Driver, Cavalry, Publication, Recording, i64) {
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
    (driver, client, publication, recording, after)
}

/// [`recording`], stopped at its end, which is returned.
fn recorded(
    dir: &TestDir,
    stream_id: i32,
    messages: usize,
    padding: bool,
) -> (Driver, Cavalry, i64) {
    let (driver, mut client, publication, _recording, after) =
        recording(dir, stream_id, messages, padding);
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

/// `cavalry replay --channel <a cavalry:udp channel> --stream <id>` plays the recording
/// to the channel's endpoint and prints the summary alone (ADR 0043): a subscription on
/// that endpoint in this process, on that stream, reads every message in order, and the
/// summary says the bytes, the positions and the channel.
#[test]
fn replay_on_a_udp_channel_plays_to_the_endpoints_subscriber_and_prints_the_summary() {
    let dir = TestDir::new("replay-udp");
    let (driver, mut client, end) = recorded(&dir, 9, 20, false);
    let port = loopback_ports(1)[0];
    let channel = format!("cavalry:udp?endpoint=127.0.0.1:{port}");
    let mut subscription = client
        .add_subscription(&channel, 9)
        .expect("a subscription on the endpoint");
    let output = replay(&dir)
        .args(["--channel", &channel, "--stream", "9"])
        .output()
        .expect("replay runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "stdout {stdout}; stderr {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 1, "the summary alone: {stdout}");
    assert!(lines[0].starts_with("replay "), "{stdout}");
    assert!(
        lines[0].ends_with(&format!(
            " of recording 0 ended at position {end}: {end} bytes from position 0 on {channel}"
        )),
        "{stdout}"
    );
    // The subscriber reads every message in order; the messages went to the wire, not
    // to the command's stdout.
    let mut bodies: Vec<Vec<u8>> = Vec::new();
    wait("the subscriber reads the recording", || {
        subscription
            .poll(|body, _| bodies.push(body.to_vec()), 64)
            .expect("the poll reads");
        bodies.len() >= 20
    });
    assert_eq!(bodies.len(), 20);
    for (n, body) in bodies.iter().enumerate() {
        assert_eq!(body, &[b'a' + (n % 26) as u8; 32], "message {n}");
    }
    subscription.close().expect("closes");
    client.close().expect("closes");
    driver.close().expect("the driver closes");
}

/// A replay over a `cavalry:udp` channel that no subscriber answers: the port is a
/// bound socket nobody reads, so the setup frames land and nothing answers them, the
/// replay ends after the replay connect timeout, 5 s, `for want of a subscriber`, and
/// the command prints the summary with `0 bytes` and exits 1 naming the endpoint, the
/// stream and what to do, never success for a replay that delivered nothing
/// (invariant 1).
#[test]
fn replay_on_a_udp_channel_exits_1_when_no_subscriber_answers() {
    let dir = TestDir::new("replay-udp-nobody");
    let (driver, client, _end) = recorded(&dir, 11, 4, false);
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").expect("a loopback socket");
    let port = socket.local_addr().expect("bound").port();
    let channel = format!("cavalry:udp?endpoint=127.0.0.1:{port}");
    let started = Instant::now();
    let output = replay(&dir)
        .args(["--channel", &channel, "--stream", "11"])
        .output()
        .expect("replay runs");
    let waited = started.elapsed();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(1),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        waited >= Duration::from_secs(4),
        "the replay connect timeout passed first: {waited:?}"
    );
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 1, "the summary alone: {stdout}");
    assert!(
        lines[0].starts_with("replay ")
            && lines[0].ends_with(&format!(
                " of recording 0 ended for want of a subscriber at position 0: 0 bytes from \
                 position 0 on {channel}"
            )),
        "{stdout}"
    );
    assert!(
        stderr.starts_with("cavalry replay: no subscriber on ")
            && stderr.contains(&format!("{channel}, stream 11,"))
            && stderr.contains("nothing was played")
            && stderr.contains("replay.not_connected")
            && stderr.contains("udp-subscription-no-image")
            && stderr.contains("docs/how-to/replay-a-recording.md#when-it-does-not-work"),
        "{stderr}"
    );
    drop(socket);
    client.close().expect("closes");
    driver.close().expect("the driver closes");
}

/// A `--follow` replay over a `cavalry:udp` channel whose driver goes before the
/// recording stops: no `replay stopped` comes, the command's client closes with the
/// driver, and the command exits 1 naming the reason and what to do, with no summary
/// line, since where the replay ended is unknown to it (invariant 1); it never waits
/// for an end that cannot come.
#[test]
fn replay_on_a_udp_channel_exits_1_when_the_driver_goes_before_the_replay_ends() {
    let dir = TestDir::new("replay-udp-driver-gone");
    let (driver, mut client, publication, recording, end) = recording(&dir, 10, 4, false);
    assert_eq!(end, 4 * 64);
    let port = loopback_ports(1)[0];
    let channel = format!("cavalry:udp?endpoint=127.0.0.1:{port}");
    let mut subscription = client
        .add_subscription(&channel, 10)
        .expect("a subscription on the endpoint");
    let mut child = replay(&dir)
        .args(["--follow", "--channel", &channel, "--stream", "10"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("replay starts");
    // The replay is live and connected once the subscriber has read what was recorded,
    // and the recording is live, so the replay follows it and does not end.
    let mut read = 0;
    wait("the subscriber reads the recording", || {
        subscription
            .poll(|_, _| read += 1, 64)
            .expect("the poll reads");
        read >= 4
    });
    assert!(
        child.try_wait().expect("try_wait").is_none(),
        "the command follows the live recording"
    );
    driver.close().expect("the driver closes");
    let deadline = Instant::now() + Duration::from_secs(10);
    while child.try_wait().expect("try_wait").is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the command did not exit within 10 s of the driver closing");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().expect("the pipes drain");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(1),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.is_empty(),
        "no summary names an end the command does not know: {stdout}"
    );
    assert!(
        stderr.starts_with("cavalry replay: client closed (driver-closed)")
            && stderr.contains("of recording 0 from position 0 on ")
            && stderr.contains("had not ended when the client closed")
            && stderr.contains("`cavalry doctor`")
            && stderr.contains("docs/how-to/replay-a-recording.md#when-it-does-not-work"),
        "{stderr}"
    );
    // The test's own client closed with the driver too; a driver that is gone cannot be
    // told, and that is not an error.
    drop(subscription);
    drop(publication);
    drop(recording);
    client.close().expect("closes");
}

/// `cavalry record --truncate`, `--purge` and `--delete` against a stopped recording in
/// this process's driver: each prints its line and exit 0; an instant for `--before`
/// resolves through the time index and rounds down to a segment base, here the start,
/// which is nothing to purge, so the driver refuses it with code 23 and the message
/// names the retained position (ADR 0031).
#[test]
fn record_truncates_purges_and_deletes_a_stopped_recording() {
    let dir = TestDir::new("retention");
    let (driver, client, after) = recorded(&dir, 1001, 100, false);
    assert_eq!(after, 100 * 64);
    let record = |args: &[&str]| {
        let mut command = cavalry();
        command.arg("record").args(args).args([
            "--dir",
            &dir.0.join("driver").to_string_lossy(),
            "--log-dir",
            &dir.0.join("log").to_string_lossy(),
        ]);
        command.output().expect("runs")
    };

    let out = record(&["--truncate", "0", "--to", "3200"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "recording 0 truncated to 3200\n"
    );

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after the epoch")
        .as_nanos();
    let out = record(&["--purge", "0", "--before", &format!("@{now}")]);
    assert!(!out.status.success(), "a purge of nothing is refused");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("purging before the segment base 0"),
        "{stdout}"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.starts_with("cavalry record: ") && stderr.contains("retained"),
        "{stderr}"
    );

    let out = record(&["--delete", "0"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "recording 0 deleted\n"
    );
    assert!(!dir.0.join("log").join("0-0.rec").exists(), "the file went");

    client.close().expect("closes");
    driver.close().expect("closes");
}

/// Loopback ports for this file's tests, counted out from 10000 to 19999 and never
/// tried: no socket is opened here to choose one.
///
/// This file's tests run at once, and most of them start child processes. On macOS a
/// socket is marked close-on-exec a moment after it is created, not as it is created,
/// so a child that another test starts in that moment keeps the socket, and with it
/// the port, for as long as the child lives. A test that bound a socket to find a
/// free port and dropped it could then find the port taken when its driver, or a
/// member of its cluster, came to bind it: `Address already in use`, code 4. A member
/// that cannot bind an endpoint stops, or never joins the log, and its election never
/// closes.
///
/// The range is below the ports an operating system gives a socket bound to port 0,
/// from 32768 on Linux and from 49152 on macOS, so no send socket of a driver is
/// given one of them, and apart from the 20000 to 31999 of `cavalry-raft`'s cluster
/// tests. Each port is given once in this process. The process id picks where the
/// count starts, through a hash that puts neighbouring ids far apart, so that two
/// test processes on one host seldom meet. A port another program holds is not seen
/// here: it shows as the bind's refusal, in the error of the test's own driver or in
/// the standard error of the member, which `Runs` prints.
fn loopback_ports(count: usize) -> Vec<u16> {
    use std::sync::atomic::{AtomicU32, Ordering};
    const FIRST: u32 = 10_000;
    const SPAN: u32 = 10_000;
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let count = u32::try_from(count).expect("a few ports");
    // In 64 bits, so that the product does not wrap.
    let start = u64::from(std::process::id()) * 2_654_435_761 % u64::from(SPAN);
    let start = u32::try_from(start).expect("below the span");
    let first = NEXT.fetch_add(count, Ordering::Relaxed);
    (0..count)
        .map(|port| u16::try_from(FIRST + (start + first + port) % SPAN).expect("below 20000"))
        .collect()
}

/// The value of a member's cluster counter named `name` in the driver directory `dir`,
/// read as `doctor` reads it, or `None` while the member has not added it.
fn cluster_counter(dir: &Path, name: &str) -> Option<i64> {
    let client = Cavalry::connect_to(dir).ok()?;
    let counters = client.counters();
    let value = (0..counters.capacity()).find_map(|id| {
        let record = counters.record(id).ok()??;
        // The label is the name, a space and the member id.
        let named = record
            .label()
            .strip_prefix(name)
            .is_some_and(|rest| rest.starts_with(' '));
        named.then(|| counters.read(id).ok())?
    });
    client.close().ok()?;
    value
}

/// The members a test started, each killed when the test ends before it stopped them:
/// a failed assertion would otherwise leave three drivers running after the test. When
/// the test failed, each member's standard error is printed with the failure, so that
/// the reason a member gave, a duty cycle's error among them, is in the test's output.
struct Runs(Vec<Child>);

impl Drop for Runs {
    fn drop(&mut self) {
        for (member, run) in self.0.iter_mut().enumerate() {
            let _ = run.kill();
            let _ = run.wait();
            if std::thread::panicking() {
                eprintln!("member {member}'s standard error:\n{}", stderr_of(run));
            }
        }
    }
}

/// `cluster.role` and `cluster.election_state` in `counters.toml`, by name: the
/// consensus module labels each of its counters with the counter's name and its
/// member id, and a name does not move when an id in that file does.
const CLUSTER_ROLE: &str = "cluster.role";
const CLUSTER_ELECTION_STATE: &str = "cluster.election_state";
const CLUSTER_LEADERSHIP_TERM_ID: &str = "cluster.leadership_term_id";

#[test]
fn run_with_the_cluster_keys_starts_a_member_and_three_of_them_elect_a_leader() {
    let ports = loopback_ports(15);
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
    let dirs: Vec<TestDir> = (0..3)
        .map(|member| TestDir::new(&format!("cluster-{member}")))
        .collect();
    let start = |member: usize| {
        cavalry()
            .args(["run", "--dir", &dirs[member].arg()])
            .args(["--log-dir", &format!("{}-log", dirs[member].arg())])
            .env("CAVALRY_CLUSTER_MEMBERS", &members)
            .env("CAVALRY_CLUSTER_MEMBER_ID", member.to_string())
            .stderr(Stdio::piped())
            .spawn()
            .expect("run starts")
    };
    let mut runs = Runs((0..3).map(start).collect());
    for (member, run) in runs.0.iter_mut().enumerate() {
        wait_until_running(run, || {
            let mut c = cavalry();
            c.args(["doctor", "--dir", &dirs[member].arg()]);
            c
        });
    }
    // Every member's election closes, state 17, one member leading, role 2, whichever
    // the members elected, and the others following, role 0: read from each driver's
    // counters file.
    // A wait that fails says where each member's election stood, from the same
    // counters, and the members string, whose ports a member's error names; `Runs`
    // then prints what each member's `run` wrote. Together they tell a member that
    // could not bind an endpoint from one whose leader closed without it.
    let deadline = Instant::now() + Duration::from_secs(10);
    while !dirs
        .iter()
        .all(|dir| cluster_counter(&dir.0, CLUSTER_ELECTION_STATE) == Some(17))
    {
        if Instant::now() >= deadline {
            let mut report = vec![format!("the members string: {members}")];
            for (member, dir) in dirs.iter().enumerate() {
                report.push(format!(
                    "member {member}: {CLUSTER_ELECTION_STATE} {:?}, {CLUSTER_ROLE} {:?}",
                    cluster_counter(&dir.0, CLUSTER_ELECTION_STATE),
                    cluster_counter(&dir.0, CLUSTER_ROLE)
                ));
            }
            panic!(
                "every member's election closed within 10 s\n{}",
                report.join("\n")
            );
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let roles: Vec<Option<i64>> = dirs
        .iter()
        .map(|dir| cluster_counter(&dir.0, CLUSTER_ROLE))
        .collect();
    let mut sorted = roles.clone();
    sorted.sort_unstable();
    assert_eq!(
        sorted,
        [Some(0), Some(0), Some(2)],
        "one leader and two followers: {roles:?}"
    );
    // The term they elected in: 0, or 1 when the first ballot was split.
    let follower = roles
        .iter()
        .position(|role| *role == Some(0))
        .expect("a follower");
    let term = cluster_counter(&dirs[follower].0, CLUSTER_LEADERSHIP_TERM_ID)
        .expect("the follower's term");
    assert!((0..=1).contains(&term), "term {term}");

    // Doctor with the keys set as the members have them finds nothing wrong with them,
    // and explains the member.
    let output = cavalry()
        .args(["doctor", "--dir", &dirs[follower].arg()])
        .env("CAVALRY_CLUSTER_MEMBERS", &members)
        .env("CAVALRY_CLUSTER_MEMBER_ID", follower.to_string())
        .output()
        .expect("doctor runs");
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(!text.contains("FAIL cluster-member"), "{text}");
    // And says what the member is, from its counters.
    assert!(
        text.contains("note cluster-member")
            && text.contains(&format!(
                "this driver is cluster member {follower}, a follower in term {term}, election \
                 closed"
            )),
        "{text}"
    );

    // SIGTERM stops the member and then the driver, and the exit is clean.
    while let Some(run) = runs.0.pop() {
        let member = runs.0.len();
        let stderr = terminate(run);
        assert!(
            stderr.contains(&format!("cluster member {member} started")),
            "{stderr}"
        );
        // The start line names the two counters by the labels this test read them by,
        // in the file it read them from, and no command that does not show them.
        assert!(
            stderr.contains(&format!(
                "the counters labelled `{CLUSTER_ROLE} {member}` and \
                 `{CLUSTER_ELECTION_STATE} {member}` in {}",
                dirs[member].0.join("counters").display()
            )),
            "{stderr}"
        );
        assert!(stderr.contains("docs/reference/counters.md"), "{stderr}");
        // And the command that reads them for the operator, which the test ran above.
        assert!(
            stderr.contains(
                "`cavalry doctor` explains the member from them in its `cluster-member` note"
            ),
            "{stderr}"
        );
        assert!(stderr.contains("closed;"), "{stderr}");
    }

    // Each member starts again on the log directory it had (ADR 0108): the three
    // elect a leader again, in a term above the one they held, since each kept its
    // record of terms and its vote, and SIGTERM closes each cleanly.
    let mut runs = Runs((0..3).map(start).collect());
    for (member, run) in runs.0.iter_mut().enumerate() {
        wait_until_running(run, || {
            let mut c = cavalry();
            c.args(["doctor", "--dir", &dirs[member].arg()]);
            c
        });
    }
    wait(
        "every member's election closed again in a later term",
        || {
            dirs.iter().all(|dir| {
                cluster_counter(&dir.0, CLUSTER_ELECTION_STATE) == Some(17)
                    && cluster_counter(&dir.0, CLUSTER_LEADERSHIP_TERM_ID)
                        .is_some_and(|again| again > term)
            })
        },
    );
    let mut roles: Vec<Option<i64>> = dirs
        .iter()
        .map(|dir| cluster_counter(&dir.0, CLUSTER_ROLE))
        .collect();
    roles.sort_unstable();
    assert_eq!(
        roles,
        [Some(0), Some(0), Some(2)],
        "one leader and two followers"
    );
    while let Some(run) = runs.0.pop() {
        let member = runs.0.len();
        let stderr = terminate(run);
        assert!(
            stderr.contains(&format!("cluster member {member} started")),
            "{stderr}"
        );
        assert!(stderr.contains("closed;"), "{stderr}");
    }
    for dir in &dirs {
        let _ = std::fs::remove_dir_all(format!("{}-log", dir.arg()));
    }
}

#[test]
fn one_cluster_key_without_the_other_stops_run_and_fails_doctor() {
    let dir = TestDir::new("cluster-keys");
    let output = cavalry()
        .args(["run", "--dir", &dir.arg()])
        .env("XDG_DATA_HOME", dir.arg())
        .env("CAVALRY_CLUSTER_MEMBER_ID", "1")
        .output()
        .expect("runs");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("CAVALRY_CLUSTER_MEMBER_ID is set and CAVALRY_CLUSTER_MEMBERS is not"),
        "{stderr}"
    );
    assert!(
        stderr.contains("docs/reference/config.md#clustermembers"),
        "{stderr}"
    );
    assert!(!dir.0.exists(), "nothing was started");

    // Doctor names the key that is wrong, and a members string the protocol refuses.
    let output = cavalry()
        .args(["doctor", "--dir", &dir.arg()])
        .env("CAVALRY_CLUSTER_MEMBERS", "0,127.0.0.1:9000")
        .env("CAVALRY_CLUSTER_MEMBER_ID", "0")
        .output()
        .expect("doctor runs");
    assert_eq!(output.status.code(), Some(1));
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("FAIL cluster-members"), "{text}");
    assert!(text.contains("members-grammar"), "{text}");

    // A member id that is not a whole number is refused before anything starts.
    let output = cavalry()
        .args(["run", "--dir", &dir.arg()])
        .env("CAVALRY_CLUSTER_MEMBER_ID", "two")
        .output()
        .expect("runs");
    assert_ne!(output.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("CAVALRY_CLUSTER_MEMBER_ID is `two`"),
        "{stderr}"
    );
    assert!(stderr.contains("or -1 for no member"), "{stderr}");
    assert!(!dir.0.exists(), "nothing was started");
}

#[test]
fn a_member_id_of_minus_one_written_out_is_no_member() {
    // The id's default, -1, written into a unit file, is no member: without a members
    // string `run` starts a driver in no cluster, as with the key unset.
    let dir = TestDir::new("cluster-no-member");
    let log_dir = TestDir::new("cluster-no-member-log");
    let mut run = cavalry()
        .args(["run", "--dir", &dir.arg(), "--log-dir", &log_dir.arg()])
        .env("CAVALRY_CLUSTER_MEMBER_ID", "-1")
        .stderr(Stdio::piped())
        .spawn()
        .expect("run starts");
    let text = wait_until_running(&mut run, || {
        let mut c = cavalry();
        c.args(["doctor", "--dir", &dir.arg(), "--log-dir", &log_dir.arg()])
            .env("CAVALRY_CLUSTER_MEMBER_ID", "-1");
        c
    });
    assert!(!text.contains("cluster-member"), "{text}");
    let stderr = terminate(run);
    assert!(!stderr.contains("cluster member"), "{stderr}");
    assert!(stderr.contains("closed;"), "{stderr}");

    // With a members string, -1 is the id left out, and `run` says so before anything
    // starts.
    let other = TestDir::new("cluster-no-member-other");
    let output = cavalry()
        .args(["run", "--dir", &other.arg(), "--log-dir", &log_dir.arg()])
        .env(
            "CAVALRY_CLUSTER_MEMBERS",
            "0,127.0.0.1:9000,127.0.0.1:9001,127.0.0.1:9002,127.0.0.1:9003,127.0.0.1:9004",
        )
        .env("CAVALRY_CLUSTER_MEMBER_ID", "-1")
        .output()
        .expect("runs");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(
            "CAVALRY_CLUSTER_MEMBERS is set and CAVALRY_CLUSTER_MEMBER_ID is unset or -1, no \
             member"
        ),
        "{stderr}"
    );
    // The page it names is the missing key's, as doctor's finding names it.
    assert!(
        stderr.contains("docs/reference/config.md#clustermemberid"),
        "{stderr}"
    );
    assert!(!other.0.exists(), "nothing was started");
}
