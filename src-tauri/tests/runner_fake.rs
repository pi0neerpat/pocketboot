//! Test plan 6: the real `SidecarRunner` spawning code against
//! `fake-dfu-util`, which replays a script chosen by `FAKE_DFU_SCRIPT`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use pocketboot_lib::dfu::runner::{DfuRunner, RunMsg, SidecarRunner, Stream};
use pocketboot_lib::dfu::{args, DfuEvent, DfuOutcome, Failure, Milestone};
use pocketboot_lib::types::ReasonCode;
use tokio::sync::mpsc;

const FAKE: &str = env!("CARGO_BIN_EXE_fake-dfu-util");

/// The runner clears the environment, so a wrapper script sets the script
/// variable and execs the fake with the runner's argv.
fn sidecar(dir: &Path, script: &str) -> PathBuf {
    let script_path = dir.join("script.txt");
    std::fs::write(&script_path, script).unwrap();
    let wrapper = dir.join("dfu-util");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nFAKE_DFU_SCRIPT='{}' exec '{FAKE}' \"$@\"\n",
            script_path.display()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    wrapper
}

fn fixture_script(name: &str) -> String {
    let text = std::fs::read_to_string(format!(
        "{}/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    let mut exit = 0;
    let mut out = String::new();
    for line in text.lines() {
        if let Some(code) = line.strip_prefix("# exit: ") {
            exit = code.trim().parse().unwrap();
        } else {
            out.push_str(&format!("out|{line}\n"));
        }
    }
    out.push_str(&format!("exit|{exit}\n"));
    out
}

/// A transfer-shaped invocation; the fake ignores argv, so only `op` and the
/// timeout matter here.
fn transfer(op: args::Op) -> args::Invocation {
    args::Invocation {
        op,
        argv: vec!["-a".into(), "0".into()],
        timeout: Duration::from_secs(20),
    }
}

async fn run(runner: &SidecarRunner, inv: args::Invocation) -> (DfuOutcome, Vec<RunMsg>) {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let report = runner.run(inv, tx).await;
    let mut msgs = Vec::new();
    while let Ok(m) = rx.try_recv() {
        msgs.push(m);
    }
    (report.outcome, msgs)
}

fn events(msgs: &[RunMsg]) -> Vec<&DfuEvent> {
    msgs.iter()
        .filter_map(|m| match m {
            RunMsg::Event(e) => Some(e),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn happy_upload_replays_to_ok() {
    let dir = tempfile::tempdir().unwrap();
    let runner = SidecarRunner::new(sidecar(dir.path(), &fixture_script("happy-upload.txt")));
    let (outcome, msgs) = run(&runner, transfer(args::Op::Upload)).await;
    assert_eq!(outcome, DfuOutcome::Ok);
    assert!(events(&msgs).contains(&&DfuEvent::Milestone(Milestone::UploadDone)));
    assert!(msgs
        .iter()
        .any(|m| matches!(m, RunMsg::Log(Stream::Stdout, t) if t == "Upload done.")));
}

#[tokio::test]
async fn carriage_returns_split_progress_lines() {
    let dir = tempfile::tempdir().unwrap();
    let script = "put|Upload\\t[  ]   0%    0 bytes\\rUpload\\t[==]  100%   16384 bytes\\r\nout|Upload done.\nexit|0\n";
    let runner = SidecarRunner::new(sidecar(dir.path(), script));
    let (outcome, msgs) = run(&runner, transfer(args::Op::Upload)).await;
    assert_eq!(outcome, DfuOutcome::Ok);
    let progress = events(&msgs)
        .into_iter()
        .filter(|e| matches!(e, DfuEvent::Progress { .. }))
        .count();
    assert_eq!(progress, 2);
}

#[tokio::test]
async fn idle_watchdog_kills_silent_process() {
    let dir = tempfile::tempdir().unwrap();
    let script = "out|Opening DFU capable USB device...\nsleep|5000\nout|Upload done.\nexit|0\n";
    let runner = SidecarRunner::with_limits(
        sidecar(dir.path(), script),
        Duration::from_millis(300),
        None,
    );
    let t = Instant::now();
    let (outcome, _) = run(&runner, transfer(args::Op::Upload)).await;
    assert_eq!(
        outcome,
        DfuOutcome::Failed(Failure::Reason(ReasonCode::Timeout))
    );
    assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
}

#[tokio::test]
async fn output_keeps_the_idle_watchdog_away() {
    let dir = tempfile::tempdir().unwrap();
    let script = "out|a\nsleep|200\nout|b\nsleep|200\nout|c\nsleep|200\nout|Upload done.\nexit|0\n";
    let runner = SidecarRunner::with_limits(
        sidecar(dir.path(), script),
        Duration::from_millis(400),
        None,
    );
    let (outcome, _) = run(&runner, transfer(args::Op::Upload)).await;
    assert_eq!(outcome, DfuOutcome::Ok);
}

#[tokio::test]
async fn total_timeout_kills_chatty_process() {
    let dir = tempfile::tempdir().unwrap();
    let mut script = String::new();
    for _ in 0..50 {
        script.push_str("out|still going\nsleep|100\n");
    }
    script.push_str("out|Upload done.\nexit|0\n");
    let runner = SidecarRunner::with_limits(
        sidecar(dir.path(), &script),
        Duration::from_secs(10),
        Some(Duration::from_millis(500)),
    );
    let t = Instant::now();
    let (outcome, _) = run(&runner, transfer(args::Op::Upload)).await;
    assert_eq!(
        outcome,
        DfuOutcome::Failed(Failure::Reason(ReasonCode::Timeout))
    );
    assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
}

#[tokio::test]
async fn nonzero_exit_after_leave_is_success_candidate() {
    let dir = tempfile::tempdir().unwrap();
    let runner = SidecarRunner::new(sidecar(
        dir.path(),
        &fixture_script("happy-download-leave.txt"),
    ));
    let (outcome, msgs) = run(&runner, transfer(args::Op::Download)).await;
    assert_eq!(outcome, DfuOutcome::SuccessCandidate);
    assert!(events(&msgs).contains(&&DfuEvent::Milestone(Milestone::DownloadDone)));
}

#[tokio::test]
async fn nonzero_exit_without_milestones_fails_with_first_error() {
    let dir = tempfile::tempdir().unwrap();
    let runner = SidecarRunner::new(sidecar(dir.path(), &fixture_script("busy-device.txt")));
    let (outcome, _) = run(&runner, transfer(args::Op::Upload)).await;
    assert_eq!(
        outcome,
        DfuOutcome::Failed(Failure::Reason(ReasonCode::Busy))
    );
}

#[tokio::test]
async fn stderr_lines_are_tagged() {
    let dir = tempfile::tempdir().unwrap();
    let runner = SidecarRunner::new(sidecar(dir.path(), "err|boom\nexit|1\n"));
    let (_, msgs) = run(&runner, args::list()).await;
    assert!(msgs
        .iter()
        .any(|m| matches!(m, RunMsg::Log(Stream::Stderr, t) if t == "boom")));
}

#[tokio::test]
async fn environment_is_cleared_and_argv_passed_verbatim() {
    let dir = tempfile::tempdir().unwrap();
    let runner = SidecarRunner::new(sidecar(dir.path(), "env|\nargv|\nexit|0\n"));
    std::env::set_var("POCKETBOOT_LEAK_CHECK", "1");
    let report = {
        let (tx, _rx) = mpsc::unbounded_channel();
        runner.run(args::list(), tx).await
    };
    let env: Vec<&str> = report
        .lines
        .iter()
        .filter_map(|l| l.strip_prefix("env "))
        // The wrapper's own variable and what `/bin/sh` itself sets.
        .filter(|l| !["FAKE_DFU_SCRIPT", "PWD", "SHLVL", "_"].contains(l))
        .collect();
    assert_eq!(env, ["LANG=C", "PATH=/usr/bin:/bin"]);
    let argv: Vec<&str> = report
        .lines
        .iter()
        .filter_map(|l| l.strip_prefix("arg "))
        .collect();
    assert_eq!(argv, ["-l", "-d", "0483:df11"]);
}

#[tokio::test]
async fn missing_binary_is_sidecar_missing() {
    let runner = SidecarRunner::new(PathBuf::from("/nonexistent/dfu-util"));
    let (outcome, _) = run(&runner, args::version()).await;
    assert_eq!(
        outcome,
        DfuOutcome::Failed(Failure::Reason(ReasonCode::SidecarMissing))
    );
}

#[tokio::test]
async fn one_process_at_a_time() {
    let dir = tempfile::tempdir().unwrap();
    let runner = SidecarRunner::new(sidecar(dir.path(), "sleep|300\nexit|0\n"));
    let t = Instant::now();
    let (a, b) = tokio::join!(run(&runner, args::list()), run(&runner, args::list()));
    assert_eq!((a.0, b.0), (DfuOutcome::Ok, DfuOutcome::Ok));
    assert!(
        t.elapsed() >= Duration::from_millis(600),
        "{:?}",
        t.elapsed()
    );
}
