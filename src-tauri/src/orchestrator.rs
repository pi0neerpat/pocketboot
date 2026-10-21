//! The one task that owns the `AppState` (KTD1). Every input arrives on one
//! channel, goes through `machine::step`, and the returned effects run here.
//! The machine never has more than one job in flight, so at most one
//! dfu-util process runs at a time.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::backup::{BackupManager, BackupMeta, CompareResult, PartialPath};
use crate::device::layout;
use crate::dfu::runner::{DfuRunner, RunMsg, RunReport, Stream};
use crate::dfu::{args, DfuEvent, DfuOutcome, Failure, Invocation, Milestone};
use crate::diagnostics::{describe_layout, LogLine, LogRing, LogStream};
use crate::machine::{self, AppState, Effect, Input};
use crate::types::{DfuDeviceInfo, ReasonCode};

/// The side effects that reach outside the process, so tests run without
/// Tauri.
pub trait Host: Send + Sync + 'static {
    fn state_changed(&self, state: &AppState);
    fn log(&self, line: &LogLine);
    fn play_sound(&self);
    /// Posts `Radio is starting` when the window is not focused (R53).
    fn notify(&self);
    fn exit(&self);
}

/// What commands and diagnostics read while the orchestrator runs.
pub struct Shared {
    pub state: Mutex<AppState>,
    pub log: Mutex<LogRing>,
    pub devices: Mutex<Vec<DfuDeviceInfo>>,
    pub layout: Mutex<Option<String>>,
    pub dfu_util_version: Mutex<String>,
    pub sound: AtomicBool,
    /// R32: the user chose Quit while the radio was restarting.
    pub quit_requested: AtomicBool,
    /// A dfu-util download is running; quitting waits for it (R32).
    pub downloading: AtomicBool,
}

impl Shared {
    pub fn new(sound: bool) -> Self {
        Self {
            state: Mutex::new(AppState::new()),
            log: Mutex::default(),
            devices: Mutex::default(),
            layout: Mutex::default(),
            dfu_util_version: Mutex::default(),
            sound: AtomicBool::new(sound),
            quit_requested: AtomicBool::new(false),
            downloading: AtomicBool::new(false),
        }
    }

    /// Whether a requested quit can happen now.
    pub fn may_exit(&self) -> bool {
        self.quit_requested.load(Ordering::SeqCst) && !self.downloading.load(Ordering::SeqCst)
    }
}

/// A poisoned lock only means another thread panicked mid-update; the data
/// is still the best we have.
pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn log(shared: &Shared, host: &impl Host, line: LogLine) {
    host.log(&line);
    lock(&shared.log).push(line);
}

pub struct Orchestrator<R, H> {
    runner: Arc<R>,
    host: Arc<H>,
    shared: Arc<Shared>,
    manager: Arc<BackupManager>,
    tx: UnboundedSender<Input>,
    app_version: String,
    /// The upload target the machine's current `Upload` job writes to.
    partial: Option<PartialPath>,
    /// `Upload done.` was seen for `partial` (R10(a)).
    upload_done: bool,
}

impl<R: DfuRunner + Send + Sync + 'static, H: Host> Orchestrator<R, H> {
    pub fn new(
        runner: R,
        host: H,
        shared: Arc<Shared>,
        manager: Arc<BackupManager>,
        tx: UnboundedSender<Input>,
        app_version: String,
    ) -> Self {
        Self {
            runner: Arc::new(runner),
            host: Arc::new(host),
            shared,
            manager,
            tx,
            app_version,
            partial: None,
            upload_done: false,
        }
    }

    pub async fn run(mut self, mut rx: UnboundedReceiver<Input>) {
        self.check_sidecar();
        while let Some(input) = rx.recv().await {
            self.handle(input);
        }
    }

    fn check_sidecar(&self) {
        let shared = self.shared.clone();
        self.spawn_run(args::version(), move |r| {
            if r.outcome == DfuOutcome::Ok {
                *lock(&shared.dfu_util_version) = r.lines.first().cloned().unwrap_or_default();
                Input::SidecarCheck(Ok(()))
            } else {
                Input::SidecarCheck(Err(ReasonCode::SidecarMissing))
            }
        });
    }

    /// Runs `inv`, streaming its lines into the log and its events into the
    /// machine, then sends `done(report)`.
    fn spawn_run(&self, inv: Invocation, done: impl FnOnce(RunReport) -> Input + Send + 'static) {
        let argv: Vec<String> = inv
            .argv
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        log(
            &self.shared,
            &*self.host,
            LogLine::now(LogStream::App, format!("$ dfu-util {}", argv.join(" "))),
        );
        let (runner, host, shared, tx) = (
            self.runner.clone(),
            self.host.clone(),
            self.shared.clone(),
            self.tx.clone(),
        );
        tokio::spawn(async move {
            let (mtx, mut mrx) = mpsc::unbounded_channel();
            let forward = async {
                while let Some(msg) = mrx.recv().await {
                    match msg {
                        RunMsg::Log(stream, text) => {
                            let stream = match stream {
                                Stream::Stdout => LogStream::Stdout,
                                Stream::Stderr => LogStream::Stderr,
                            };
                            log(&shared, &*host, LogLine::now(stream, text));
                        }
                        RunMsg::Event(e) => {
                            let _ = tx.send(Input::DfuEvent(e));
                        }
                    }
                }
            };
            let (report, ()) = tokio::join!(runner.run(inv, mtx), forward);
            let _ = tx.send(done(report));
        });
    }

    fn send(&self, input: Input) {
        let _ = self.tx.send(input);
    }

    fn handle(&mut self, input: Input) {
        match &input {
            Input::DfuEvent(DfuEvent::Milestone(Milestone::UploadDone)) => self.upload_done = true,
            Input::DevicesChanged(snap) => *lock(&self.shared.devices) = snap.dfu_devices.clone(),
            Input::LayoutResult(Ok(l)) => *lock(&self.shared.layout) = Some(describe_layout(l)),
            Input::DfuExited(_) => self.shared.downloading.store(false, Ordering::SeqCst),
            _ => {}
        }
        let (next, effects) = {
            let mut state = lock(&self.shared.state);
            let (next, effects) = machine::step(&state, input);
            let changed = next.view() != state.view();
            *state = next.clone();
            (changed.then_some(next), effects)
        };
        if let Some(next) = next {
            self.host.state_changed(&next);
        }
        for e in effects {
            self.execute(e);
        }
        if self.shared.may_exit() {
            self.host.exit();
        }
    }

    fn execute(&mut self, effect: Effect) {
        match effect {
            Effect::RunList(serial) => {
                self.spawn_run(args::list(), move |r| {
                    Input::LayoutResult(layout::from_report(&r, serial.as_ref()))
                });
            }
            Effect::RunUpload(serial) => match self.manager.new_partial(serial.as_ref()) {
                Ok(p) => {
                    let inv = args::upload(serial.as_ref(), &p);
                    self.partial = Some(p);
                    self.upload_done = false;
                    self.spawn_run(inv, |r| Input::DfuExited(r.outcome));
                }
                Err(_) => self.send(Input::DfuExited(DfuOutcome::Failed(Failure::Internal))),
            },
            Effect::ValidateBackup { serial, earlier } => {
                let Some(p) = self.partial.take() else {
                    self.send(Input::BackupValidated(
                        Err(ReasonCode::Unknown),
                        CompareResult::Incomparable,
                    ));
                    return;
                };
                let cmp = self.manager.compare(&p, earlier.as_ref());
                let meta = BackupMeta {
                    serial,
                    app_version: self.app_version.clone(),
                    dfu_util_version: lock(&self.shared.dfu_util_version).clone(),
                    layout: lock(&self.shared.layout).clone().unwrap_or_default(),
                };
                let result = self.manager.validate(p, self.upload_done).and_then(|v| {
                    self.manager
                        .finalize(v, &meta)
                        .map_err(|_| ReasonCode::Unknown)
                });
                self.send(Input::BackupValidated(result, cmp));
            }
            Effect::RunDownloadLeave(serial, v) => match self.manager.recheck(&v) {
                Ok(()) => {
                    self.shared.downloading.store(true, Ordering::SeqCst);
                    self.spawn_run(args::download_leave(serial.as_ref(), &v), |r| {
                        Input::DfuExited(r.outcome)
                    });
                }
                Err(code) => self.send(Input::DfuExited(DfuOutcome::Failed(Failure::Reason(code)))),
            },
            Effect::RevalidateFirstInRun(r) => {
                self.send(Input::RestoreValidated(self.manager.revalidate(&r)));
            }
            Effect::StartWatchWindow { id, duration } => {
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(duration).await;
                    let _ = tx.send(Input::WatchWindowElapsed(id));
                });
            }
            Effect::DeletePartial => {
                if let Some(p) = self.partial.take() {
                    self.manager.discard(p);
                }
            }
            Effect::PlaySuccessSound => {
                if self.shared.sound.load(Ordering::SeqCst) {
                    self.host.play_sound();
                }
            }
            Effect::Notify => self.host.notify(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    use crate::backup::manager::SECTOR0_LEN;
    use crate::dfu::Op;
    use crate::machine::View;
    use crate::types::{DeviceSnapshot, Serial};

    #[derive(Default)]
    struct MockRunner {
        uploads: Mutex<VecDeque<Vec<u8>>>,
        calls: Mutex<Vec<Op>>,
    }

    impl DfuRunner for MockRunner {
        fn run(
            &self,
            inv: Invocation,
            tx: UnboundedSender<RunMsg>,
        ) -> impl std::future::Future<Output = RunReport> + Send {
            lock(&self.calls).push(inv.op);
            let report = |outcome, lines: &[&str]| RunReport {
                outcome,
                lines: lines.iter().map(|l| l.to_string()).collect(),
            };
            let r = match inv.op {
                Op::Version => report(DfuOutcome::Ok, &["dfu-util 0.11"]),
                Op::List => report(
                    DfuOutcome::Ok,
                    &[
                        r#"Found DFU: [0483:df11] ver=2200, devnum=5, cfg=1, intf=0, path="1-1", alt=0, name="@Internal Flash  /0x08000000/04*016Kg,01*064Kg,07*128Kg", serial="2059""#,
                    ],
                ),
                Op::Upload => {
                    let i = inv.argv.iter().position(|a| a == "-U").unwrap();
                    let dest = PathBuf::from(&inv.argv[i + 1]);
                    let bytes = lock(&self.uploads)
                        .pop_front()
                        .unwrap_or_else(|| vec![0x5a; SECTOR0_LEN]);
                    std::fs::write(dest, bytes).unwrap();
                    let _ = tx.send(RunMsg::Log(Stream::Stdout, "Upload done.".into()));
                    let _ = tx.send(RunMsg::Event(DfuEvent::Milestone(Milestone::UploadDone)));
                    report(DfuOutcome::Ok, &[])
                }
                Op::Download => report(DfuOutcome::SuccessCandidate, &[]),
            };
            async move { r }
        }
    }

    #[derive(Default)]
    struct TestHost {
        views: Mutex<Vec<View>>,
        sounds: AtomicUsize,
        notifies: AtomicUsize,
        logs: AtomicUsize,
    }

    impl Host for Arc<TestHost> {
        fn state_changed(&self, s: &AppState) {
            lock(&self.views).push(s.view().clone());
        }
        fn log(&self, _: &LogLine) {
            self.logs.fetch_add(1, Ordering::SeqCst);
        }
        fn play_sound(&self) {
            self.sounds.fetch_add(1, Ordering::SeqCst);
        }
        fn notify(&self) {
            self.notifies.fetch_add(1, Ordering::SeqCst);
        }
        fn exit(&self) {}
    }

    struct Rig {
        tx: UnboundedSender<Input>,
        host: Arc<TestHost>,
        runner: Arc<MockRunner>,
        shared: Arc<Shared>,
        manager: Arc<BackupManager>,
        _dir: tempfile::TempDir,
    }

    fn rig(uploads: Vec<Vec<u8>>) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let manager = Arc::new(BackupManager::new(dir.path().join("backups")));
        let shared = Arc::new(Shared::new(true));
        let host = Arc::new(TestHost::default());
        let (tx, rx) = mpsc::unbounded_channel();
        let runner = MockRunner {
            uploads: Mutex::new(uploads.into()),
            ..Default::default()
        };
        let o = Orchestrator::new(
            runner,
            host.clone(),
            shared.clone(),
            manager.clone(),
            tx.clone(),
            "0.1.0".into(),
        );
        // Share the runner with the test so it can read the calls.
        let runner = o.runner.clone();
        tokio::spawn(o.run(rx));
        Rig {
            tx,
            host,
            runner,
            shared,
            manager,
            _dir: dir,
        }
    }

    impl Rig {
        fn send(&self, i: Input) {
            self.tx.send(i).unwrap();
        }

        /// Two identical polls, which the machine needs before acting (R7).
        fn devices(&self, ds: &[DfuDeviceInfo]) {
            for _ in 0..2 {
                self.send(Input::DevicesChanged(DeviceSnapshot {
                    dfu_devices: ds.to_vec(),
                }));
            }
        }

        fn seen(&self, pred: impl Fn(&View) -> bool) -> bool {
            lock(&self.host.views).iter().any(&pred)
        }

        async fn until(&self, what: &str, pred: impl Fn(&View) -> bool) {
            for _ in 0..2000 {
                if self.seen(&pred) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            panic!("never reached {what}; views: {:?}", lock(&self.host.views));
        }

        fn calls(&self) -> Vec<Op> {
            lock(&self.runner.calls).clone()
        }
    }

    fn pocket() -> DfuDeviceInfo {
        DfuDeviceInfo {
            vid: 0x0483,
            pid: 0xdf11,
            serial: Some(Serial::new("2059")),
            product: Some("STM32 BOOTLOADER".into()),
            location_id: 1,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn boot_runs_waiting_backing_up_ready_booting_success() {
        let r = rig(vec![]);
        r.until("waiting", |v| matches!(v, View::Waiting)).await;
        assert_eq!(*lock(&r.shared.dfu_util_version), "dfu-util 0.11");

        r.devices(&[pocket()]);
        r.until("ready", |v| matches!(v, View::Ready { .. })).await;
        assert!(r.seen(|v| matches!(v, View::BackingUp { .. })));
        assert_eq!(
            lock(&r.shared.layout).as_deref(),
            Some("0x08000000 4*16K,1*64K,7*128K")
        );
        let (count, newest) = r.manager.stats(Some(&Serial::new("2059")));
        assert_eq!(count, 1);
        assert!(newest.unwrap().ends_with("-sector0-first.bin"));

        r.send(Input::UserBoot);
        // The radio leaves the bus after the leave request.
        r.devices(&[]);
        r.until("success", |v| matches!(v, View::Success { .. }))
            .await;
        assert!(r.seen(|v| matches!(v, View::Booting { .. })));
        assert_eq!(r.calls(), [Op::Version, Op::List, Op::Upload, Op::Download]);
        assert_eq!(r.host.sounds.load(Ordering::SeqCst), 1);
        assert_eq!(r.host.notifies.load(Ordering::SeqCst), 1);
        let log = lock(&r.shared.log).all();
        assert!(log
            .iter()
            .any(|l| l.stream == LogStream::App && l.text.starts_with("$ dfu-util ")));
        assert!(log.iter().any(|l| l.text == "Upload done."));
    }

    /// Reaches `dataChanged`: a first backup, a replug, and a second read
    /// that differs from it (R14).
    async fn data_changed(r: &Rig) {
        r.until("waiting", |v| matches!(v, View::Waiting)).await;
        r.devices(&[pocket()]);
        r.until("ready", |v| matches!(v, View::Ready { .. })).await;
        r.devices(&[]);
        r.devices(&[pocket()]);
        r.until("dataChanged", |v| matches!(v, View::DataChanged { .. }))
            .await;
    }

    fn first_file(r: &Rig) -> PathBuf {
        let dir = r.manager.serial_dir(Some(&Serial::new("2059")));
        std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| p.to_string_lossy().ends_with("-sector0-first.bin"))
            .unwrap()
    }

    #[tokio::test(start_paused = true)]
    async fn restore_revalidates_then_writes_the_first_copy() {
        let r = rig(vec![vec![0x11; SECTOR0_LEN], vec![0x22; SECTOR0_LEN]]);
        data_changed(&r).await;
        r.send(Input::UserRestore);
        r.until("restoring", |v| matches!(v, View::Restoring { .. }))
            .await;
        for _ in 0..100 {
            if r.calls().contains(&Op::Download) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(r.calls().iter().filter(|o| **o == Op::Download).count(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn tampered_first_copy_fails_without_a_download() {
        let r = rig(vec![vec![0x11; SECTOR0_LEN], vec![0x22; SECTOR0_LEN]]);
        data_changed(&r).await;
        std::fs::write(first_file(&r), vec![0x33; SECTOR0_LEN]).unwrap();
        r.send(Input::UserRestore);
        r.until("failed", |v| {
            matches!(
                v,
                View::Failed {
                    reason: ReasonCode::HashMismatch,
                    ..
                }
            )
        })
        .await;
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(!r.calls().contains(&Op::Download));
    }

    #[tokio::test(start_paused = true)]
    async fn sound_follows_the_setting() {
        let r = rig(vec![]);
        r.shared.sound.store(false, Ordering::SeqCst);
        r.until("waiting", |v| matches!(v, View::Waiting)).await;
        r.devices(&[pocket()]);
        r.until("ready", |v| matches!(v, View::Ready { .. })).await;
        r.send(Input::UserBoot);
        r.devices(&[]);
        r.until("success", |v| matches!(v, View::Success { .. }))
            .await;
        assert_eq!(r.host.sounds.load(Ordering::SeqCst), 0);
        assert_eq!(r.host.notifies.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn may_exit_waits_for_the_download() {
        let s = Shared::new(true);
        assert!(!s.may_exit());
        s.quit_requested.store(true, Ordering::SeqCst);
        s.downloading.store(true, Ordering::SeqCst);
        assert!(!s.may_exit());
        s.downloading.store(false, Ordering::SeqCst);
        assert!(s.may_exit());
    }
}
