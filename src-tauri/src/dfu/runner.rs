//! Spawns dfu-util with no shell and a cleared environment, streams its
//! output as log lines and parsed events, and enforces the total timeout and
//! the R26 no-output watchdog (KTD3, R13, R26).

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::sync::Mutex;
use tokio::time::{sleep, sleep_until, Instant};

use crate::dfu::args::Invocation;
use crate::dfu::parser::{classify, final_outcome};
use crate::dfu::{DfuEvent, DfuOutcome, Failure, Seen};
use crate::types::ReasonCode;

/// R26: kill after this much silence on both streams.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

/// What a run streams to the orchestrator while it is in flight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunMsg {
    Log(Stream, String),
    Event(DfuEvent),
}

/// A finished run: its outcome plus every line, which the layout probe
/// parses after `dfu-util -l`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunReport {
    pub outcome: DfuOutcome,
    pub lines: Vec<String>,
}

pub trait DfuRunner {
    fn run(
        &self,
        inv: Invocation,
        tx: UnboundedSender<RunMsg>,
    ) -> impl std::future::Future<Output = RunReport> + Send;
}

pub struct SidecarRunner {
    path: PathBuf,
    idle: Duration,
    /// Overrides the invocation's own timeout; tests only.
    total: Option<Duration>,
    /// Held for the whole run: at most one dfu-util process at a time.
    busy: Mutex<()>,
}

impl SidecarRunner {
    pub fn new(path: PathBuf) -> Self {
        Self::with_limits(path, IDLE_TIMEOUT, None)
    }

    /// Scaled watchdog and timeout for tests.
    pub fn with_limits(path: PathBuf, idle: Duration, total: Option<Duration>) -> Self {
        Self {
            path,
            idle,
            total,
            busy: Mutex::new(()),
        }
    }

    /// The sidecar Tauri places beside the app binary in `Contents/MacOS/`.
    /// Debug builds honour `POCKETBOOT_DFU_UTIL` (KTD3).
    pub fn bundled() -> std::io::Result<Self> {
        #[cfg(debug_assertions)]
        if let Some(p) = std::env::var_os("POCKETBOOT_DFU_UTIL") {
            return Ok(Self::new(p.into()));
        }
        let exe = std::env::current_exe()?;
        let dir = exe
            .parent()
            .ok_or_else(|| std::io::Error::other("executable has no parent directory"))?;
        Ok(Self::new(dir.join("dfu-util")))
    }
}

impl DfuRunner for SidecarRunner {
    async fn run(&self, inv: Invocation, tx: UnboundedSender<RunMsg>) -> RunReport {
        let _one = self.busy.lock().await;
        let spawned = Command::new(&self.path)
            .args(&inv.argv)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LANG", "C")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn();
        let mut child = match spawned {
            Ok(c) => c,
            Err(_) => {
                return RunReport {
                    outcome: DfuOutcome::Failed(Failure::Reason(ReasonCode::SidecarMissing)),
                    lines: Vec::new(),
                }
            }
        };

        let (chunk_tx, mut chunks) = mpsc::unbounded_channel();
        if let Some(out) = child.stdout.take() {
            tokio::spawn(pump(out, Stream::Stdout, chunk_tx.clone()));
        }
        if let Some(err) = child.stderr.take() {
            tokio::spawn(pump(err, Stream::Stderr, chunk_tx));
        }

        let total = sleep(self.total.unwrap_or(inv.timeout));
        tokio::pin!(total);
        let mut idle_at = Instant::now() + self.idle;
        let mut out = Output {
            op: inv.op,
            tx,
            seen: Seen::default(),
            lines: Vec::new(),
            partial: [Vec::new(), Vec::new()],
        };

        let mut timed_out = false;
        loop {
            tokio::select! {
                chunk = chunks.recv() => match chunk {
                    Some((stream, bytes)) => {
                        idle_at = Instant::now() + self.idle;
                        out.push(stream, &bytes);
                    }
                    None => break,
                },
                _ = sleep_until(idle_at) => { timed_out = true; break; }
                _ = &mut total => { timed_out = true; break; }
            }
        }
        // Both streams closed; the process may still linger silently.
        let code = if timed_out {
            None
        } else {
            tokio::select! {
                status = child.wait() => status.ok().and_then(|s| s.code()),
                _ = sleep_until(idle_at) => { timed_out = true; None }
                _ = &mut total => { timed_out = true; None }
            }
        };
        if timed_out {
            let _ = child.kill().await;
        }
        out.flush();

        let outcome = if timed_out {
            DfuOutcome::Failed(Failure::Reason(ReasonCode::Timeout))
        } else {
            final_outcome(inv.op, code, &out.seen)
        };
        RunReport {
            outcome,
            lines: out.lines,
        }
    }
}

async fn pump(
    mut r: impl AsyncRead + Unpin,
    stream: Stream,
    tx: UnboundedSender<(Stream, Vec<u8>)>,
) {
    let mut buf = [0u8; 4096];
    while let Ok(n) = r.read(&mut buf).await {
        if n == 0 || tx.send((stream, buf[..n].to_vec())).is_err() {
            break;
        }
    }
}

/// Splits raw bytes on `\r`/`\n` per stream and classifies each line.
struct Output {
    op: crate::dfu::Op,
    tx: UnboundedSender<RunMsg>,
    seen: Seen,
    lines: Vec<String>,
    partial: [Vec<u8>; 2],
}

impl Output {
    fn push(&mut self, stream: Stream, bytes: &[u8]) {
        let i = stream as usize;
        for &b in bytes {
            if b == b'\r' || b == b'\n' {
                let line = std::mem::take(&mut self.partial[i]);
                self.emit(stream, &line);
            } else {
                self.partial[i].push(b);
            }
        }
    }

    fn flush(&mut self) {
        for stream in [Stream::Stdout, Stream::Stderr] {
            let line = std::mem::take(&mut self.partial[stream as usize]);
            self.emit(stream, &line);
        }
    }

    fn emit(&mut self, stream: Stream, raw: &[u8]) {
        let text = String::from_utf8_lossy(raw).trim_end().to_string();
        if text.trim().is_empty() {
            return;
        }
        let ev = classify(&text, self.seen.ctx(self.op));
        self.seen.observe(&ev);
        // A closed receiver only means nobody is watching; the run goes on.
        let _ = self.tx.send(RunMsg::Log(stream, text.clone()));
        if ev != DfuEvent::Other {
            let _ = self.tx.send(RunMsg::Event(ev));
        }
        self.lines.push(text);
    }
}
