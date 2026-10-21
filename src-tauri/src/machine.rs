//! The pure state machine (KTD1, KTD2). `step` holds every branch of R7–R21
//! and does no I/O; the orchestrator executes the returned effects and feeds
//! their results back in as inputs.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::Serialize;

use crate::backup::{BackupRef, CompareResult, ValidatedBackup};
use crate::dfu::args::ADDR;
use crate::dfu::{DfuEvent, DfuOutcome, Failure, FlashLayout, ProgressOp};
use crate::messages;
use crate::types::{DeviceSnapshot, DfuDeviceInfo, ReasonCode, Serial};

/// How long the device must stay off the DFU bus after a leave (R8).
pub const WATCH_WINDOW: Duration = Duration::from_secs(8);
/// Identical consecutive polls needed before acting on a device (R7).
const STABLE_POLLS: u8 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceLabel {
    pub vid: String,
    pub pid: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub serial: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub product: Option<String>,
}

impl From<&DfuDeviceInfo> for DeviceLabel {
    fn from(d: &DfuDeviceInfo) -> Self {
        Self {
            vid: format!("{:04x}", d.vid),
            pid: format!("{:04x}", d.pid),
            serial: d.serial.as_ref().map(|s| s.to_string()),
            product: d.product.clone(),
        }
    }
}

/// The frontend `AppState` union, field for field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum View {
    Starting,
    Waiting,
    Conflict {
        devices: Vec<DeviceLabel>,
    },
    Unsupported {
        device: DeviceLabel,
    },
    BackingUp {
        device: DeviceLabel,
        percent: u8,
    },
    Ready {
        device: DeviceLabel,
        attempt: u32,
        backup_at: String,
    },
    Booting {
        device: DeviceLabel,
        attempt: u32,
        percent: u8,
    },
    Restoring {
        device: DeviceLabel,
        percent: u8,
        backup_at: String,
    },
    Success {
        device: DeviceLabel,
    },
    Failed {
        #[serde(skip_serializing_if = "Option::is_none")]
        device: Option<DeviceLabel>,
        reason: ReasonCode,
        message: &'static str,
        can_retry: bool,
    },
    BackupFailed {
        #[serde(skip_serializing_if = "Option::is_none")]
        device: Option<DeviceLabel>,
        reason: ReasonCode,
        message: &'static str,
    },
    DataChanged {
        device: DeviceLabel,
        earlier_backup_at: String,
    },
    Fatal {
        reason: ReasonCode,
        message: &'static str,
    },
    /// Reserved for Windows; never emitted on macOS.
    DriverMissing {
        device: DeviceLabel,
    },
}

/// The visible view plus the private session facts behind it (KTD2).
/// Serializes as the view alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppState {
    view: View,
    ctx: Ctx,
}

impl Serialize for AppState {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.view.serialize(s)
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

impl AppState {
    pub fn new() -> Self {
        Self {
            view: View::Starting,
            ctx: Ctx::default(),
        }
    }

    pub fn view(&self) -> &View {
        &self.view
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Ctx {
    /// The latest snapshot.
    devices: Vec<DfuDeviceInfo>,
    /// How many consecutive snapshots have equalled `devices`.
    stable: u8,
    /// The current DFU session: one continuous presence of a device (R11).
    session: Option<Session>,
    /// The first valid backup of each serial in this app run (R14).
    first_in_run: BTreeMap<Option<Serial>, BackupRef>,
    /// The effect whose result the machine is waiting for. Never more than one.
    inflight: Option<Job>,
    /// The in-flight result is stale (KTD13) and is dropped when it arrives.
    discard: bool,
    watch: Option<Watch>,
    watch_seq: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Session {
    device: DfuDeviceInfo,
    layout_ok: bool,
    /// Taken by the write, so a backup is used at most once (R11).
    backup: Option<ValidatedBackup>,
    attempt: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Job {
    List,
    Upload,
    Validate,
    Revalidate,
    Download,
}

/// The R8 window after a success candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Watch {
    id: u32,
    device: DfuDeviceInfo,
    attempt: u32,
    /// The device has been seen off the bus since the leave.
    gone: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    SidecarCheck(Result<(), ReasonCode>),
    DevicesChanged(DeviceSnapshot),
    LayoutResult(Result<FlashLayout, Failure>),
    DfuEvent(DfuEvent),
    DfuExited(DfuOutcome),
    /// The orchestrator compared, validated, and finalized the upload (KTD12).
    BackupValidated(Result<ValidatedBackup, ReasonCode>, CompareResult),
    /// `revalidate` of the first-in-run backup for Restore (KTD12).
    RestoreValidated(Result<ValidatedBackup, ReasonCode>),
    WatchWindowElapsed(u32),
    UserBoot,
    UserRetry,
    UserRestore,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    RunList(Option<Serial>),
    RunUpload(Option<Serial>),
    /// Compare the partial with `earlier`, then validate and finalize it, and
    /// answer with `BackupValidated`.
    ValidateBackup {
        serial: Option<Serial>,
        earlier: Option<BackupRef>,
    },
    /// The orchestrator rechecks the hash first and answers a mismatch with
    /// `DfuExited(Failed(Reason(HashMismatch)))`.
    RunDownloadLeave(Option<Serial>, ValidatedBackup),
    RevalidateFirstInRun(BackupRef),
    StartWatchWindow {
        id: u32,
        duration: Duration,
    },
    DeletePartial,
    PlaySuccessSound,
    Notify,
}

pub fn step(state: &AppState, input: Input) -> (AppState, Vec<Effect>) {
    let mut next = state.clone();
    let mut fx = Vec::new();
    next.apply(input, &mut fx);
    (next, fx)
}

/// Reason and copy for a failure that has no state of its own.
fn describe(f: Failure) -> (ReasonCode, &'static str) {
    match f {
        Failure::Reason(r) => (r, messages::for_reason(r)),
        Failure::Internal => (ReasonCode::Unknown, messages::INTERNAL),
        Failure::NoDevice => (ReasonCode::DeviceGone, messages::DEVICE_GONE),
        Failure::Conflict | Failure::Unsupported => (ReasonCode::Unknown, messages::UNKNOWN),
    }
}

/// R9: the Pocket's sector 0 is 16 KB at the flash base.
fn sector0_is_16k(l: &FlashLayout) -> bool {
    l.base == ADDR && l.segments.first() == Some(&(4, 16))
}

impl AppState {
    fn apply(&mut self, input: Input, fx: &mut Vec<Effect>) {
        match input {
            Input::SidecarCheck(r) => {
                if self.view != View::Starting {
                    return;
                }
                match r {
                    Ok(()) => {
                        self.view = View::Waiting;
                        self.detect(fx);
                    }
                    Err(reason) => {
                        self.view = View::Fatal {
                            reason,
                            message: messages::FATAL,
                        }
                    }
                }
            }
            Input::DevicesChanged(snap) => self.on_snapshot(snap.dfu_devices, fx),
            Input::LayoutResult(r) => {
                if self.complete(Job::List, fx) {
                    self.on_layout(r, fx);
                }
            }
            Input::DfuEvent(e) => self.on_event(e),
            Input::DfuExited(o) => {
                if self.complete(Job::Upload, fx) {
                    self.on_upload_exit(o, fx);
                } else if self.complete(Job::Download, fx) {
                    self.on_download_exit(o, fx);
                }
            }
            Input::BackupValidated(r, cmp) => {
                if self.complete(Job::Validate, fx) {
                    self.on_validated(r, cmp);
                }
            }
            Input::RestoreValidated(r) => {
                if self.complete(Job::Revalidate, fx) {
                    self.on_restore_validated(r, fx);
                }
            }
            Input::WatchWindowElapsed(id) => self.on_elapsed(id, fx),
            Input::UserBoot => self.on_boot(fx),
            Input::UserRetry => self.on_retry(fx),
            Input::UserRestore => self.on_restore(fx),
        }
    }

    /// Clears `job` if it is the one in flight. False when the result must
    /// be dropped: no such job, or it went stale (KTD13).
    fn complete(&mut self, job: Job, fx: &mut Vec<Effect>) -> bool {
        if self.ctx.inflight != Some(job) {
            return false;
        }
        self.ctx.inflight = None;
        if !self.ctx.discard {
            return true;
        }
        self.ctx.discard = false;
        if job == Job::Upload {
            fx.push(Effect::DeletePartial);
        }
        self.detect(fx);
        false
    }

    fn present(&self, d: &DfuDeviceInfo) -> bool {
        self.ctx.devices.iter().any(|x| x.same_device(d))
    }

    /// The device is the only one on the bus, which every write requires (R10).
    fn alone(&self, d: &DfuDeviceInfo) -> bool {
        matches!(&self.ctx.devices[..], [x] if x.same_device(d))
    }

    fn session_label(&self) -> Option<DeviceLabel> {
        self.ctx.session.as_ref().map(|s| (&s.device).into())
    }

    fn on_snapshot(&mut self, devices: Vec<DfuDeviceInfo>, fx: &mut Vec<Effect>) {
        if devices == self.ctx.devices {
            self.ctx.stable = self.ctx.stable.saturating_add(1);
        } else {
            self.ctx.devices = devices;
            self.ctx.stable = 1;
        }
        // A write is never interrupted by what the bus looks like (KTD13).
        if matches!(self.view, View::Starting | View::Fatal { .. })
            || self.ctx.inflight == Some(Job::Download)
        {
            return;
        }
        self.react_to_bus(fx);
    }

    /// Ends or keeps the session for the current bus, then looks for a new one.
    fn react_to_bus(&mut self, fx: &mut Vec<Effect>) {
        if let Some(s) = &self.ctx.session {
            if !self.present(&s.device) {
                self.ctx.session = None;
            }
        }
        if let Some(w) = self.ctx.watch.clone() {
            self.on_watch_snapshot(w, fx);
            return;
        }
        if self.ctx.devices.len() > 1 {
            self.ctx.session = None;
            self.ctx.discard |= self.ctx.inflight.is_some();
            self.view = self.conflict_view();
            return;
        }
        let ends_with_session = matches!(
            self.view,
            View::Conflict { .. }
                | View::Unsupported { .. }
                | View::BackingUp { .. }
                | View::Ready { .. }
                | View::DataChanged { .. }
                | View::BackupFailed { .. }
        );
        if self.ctx.session.is_none() && ends_with_session {
            self.ctx.discard |= self.ctx.inflight.is_some();
            self.view = View::Waiting;
        }
        self.detect(fx);
    }

    /// R8: re-enumeration of the same device inside the window means the
    /// radio jumped back into DFU, so the boot missed its catch.
    fn on_watch_snapshot(&mut self, mut w: Watch, fx: &mut Vec<Effect>) {
        match &self.ctx.devices[..] {
            [] => {
                w.gone = true;
                self.ctx.watch = Some(w);
            }
            [d] if d.same_device(&w.device) => {
                if w.gone && self.ctx.stable >= STABLE_POLLS {
                    self.ctx.watch = None;
                    self.missed_catch(w, fx);
                }
            }
            [_] => {
                self.ctx.watch = None;
                self.view = View::Waiting;
                self.detect(fx);
            }
            _ => {
                self.ctx.watch = None;
                self.ctx.session = None;
                self.view = self.conflict_view();
            }
        }
    }

    fn conflict_view(&self) -> View {
        View::Conflict {
            devices: self.ctx.devices.iter().map(Into::into).collect(),
        }
    }

    /// Starts a session once exactly one Pocket has held still (R7).
    fn detect(&mut self, fx: &mut Vec<Effect>) {
        if self.ctx.inflight.is_some()
            || self.ctx.session.is_some()
            || self.ctx.watch.is_some()
            || self.ctx.stable < STABLE_POLLS
            || matches!(self.view, View::Starting | View::Fatal { .. })
        {
            return;
        }
        let [d] = &self.ctx.devices[..] else { return };
        if !d.is_pocket() {
            return;
        }
        let device = d.clone();
        self.view = View::BackingUp {
            device: (&device).into(),
            percent: 0,
        };
        fx.push(Effect::RunList(device.serial.clone()));
        self.ctx.inflight = Some(Job::List);
        self.ctx.session = Some(Session {
            device,
            layout_ok: false,
            backup: None,
            attempt: 1,
        });
    }

    fn begin_backup(&mut self, fx: &mut Vec<Effect>) {
        let Some(s) = self.ctx.session.as_mut() else {
            return;
        };
        s.backup = None;
        self.view = View::BackingUp {
            device: (&s.device).into(),
            percent: 0,
        };
        fx.push(Effect::RunUpload(s.device.serial.clone()));
        self.ctx.inflight = Some(Job::Upload);
    }

    fn missed_catch(&mut self, w: Watch, fx: &mut Vec<Effect>) {
        // The layout passed moments ago for this device, so skip the probe.
        self.ctx.session = Some(Session {
            device: w.device,
            layout_ok: true,
            backup: None,
            attempt: w.attempt + 1,
        });
        self.begin_backup(fx);
    }

    /// R21: losing the device before a write is silent; wait for it again.
    fn lose_device(&mut self) {
        self.ctx.session = None;
        self.ctx.stable = 0;
        self.view = View::Waiting;
    }

    fn enter_conflict(&mut self) {
        self.ctx.session = None;
        self.ctx.stable = 0;
        self.view = self.conflict_view();
    }

    fn backup_failed(&mut self, f: Failure) {
        let (reason, message) = describe(f);
        self.view = View::BackupFailed {
            device: self.session_label(),
            reason,
            message,
        };
    }

    fn failed(&mut self, reason: ReasonCode, message: &'static str) {
        self.view = View::Failed {
            device: self.session_label(),
            reason,
            message,
            can_retry: messages::can_retry(reason),
        };
    }

    fn on_layout(&mut self, r: Result<FlashLayout, Failure>, fx: &mut Vec<Effect>) {
        let Some(label) = self.session_label() else {
            self.view = View::Waiting;
            return;
        };
        match r {
            Ok(l) if sector0_is_16k(&l) => {
                if let Some(s) = self.ctx.session.as_mut() {
                    s.layout_ok = true;
                }
                self.begin_backup(fx);
            }
            Ok(_) | Err(Failure::Unsupported) => self.view = View::Unsupported { device: label },
            Err(Failure::NoDevice | Failure::Reason(ReasonCode::DeviceGone)) => self.lose_device(),
            Err(Failure::Conflict) => self.enter_conflict(),
            Err(f) => self.backup_failed(f),
        }
    }

    fn on_event(&mut self, e: DfuEvent) {
        let DfuEvent::Progress { op, percent: p } = e else {
            return;
        };
        if self.ctx.discard {
            return;
        }
        match (op, self.ctx.inflight, &mut self.view) {
            (ProgressOp::Upload, Some(Job::Upload), View::BackingUp { percent, .. })
            | (ProgressOp::Download, Some(Job::Download), View::Booting { percent, .. })
            | (ProgressOp::Download, Some(Job::Download), View::Restoring { percent, .. }) => {
                *percent = p
            }
            _ => {}
        }
    }

    fn on_upload_exit(&mut self, o: DfuOutcome, fx: &mut Vec<Effect>) {
        let Some(s) = &self.ctx.session else {
            fx.push(Effect::DeletePartial);
            self.view = View::Waiting;
            return;
        };
        let f = match o {
            DfuOutcome::Ok | DfuOutcome::SuccessCandidate => {
                let serial = s.device.serial.clone();
                let earlier = self.ctx.first_in_run.get(&serial).cloned();
                fx.push(Effect::ValidateBackup { serial, earlier });
                self.ctx.inflight = Some(Job::Validate);
                return;
            }
            DfuOutcome::Failed(f) => f,
        };
        fx.push(Effect::DeletePartial);
        match f {
            Failure::NoDevice | Failure::Reason(ReasonCode::DeviceGone) => self.lose_device(),
            Failure::Conflict => self.enter_conflict(),
            Failure::Unsupported => {
                self.view = View::Unsupported {
                    device: (&s.device).into(),
                }
            }
            f => self.backup_failed(f),
        }
    }

    fn on_validated(&mut self, r: Result<ValidatedBackup, ReasonCode>, cmp: CompareResult) {
        let Some(s) = self.ctx.session.as_mut() else {
            self.view = View::Waiting;
            return;
        };
        let device = DeviceLabel::from(&s.device);
        let earlier = self.ctx.first_in_run.get(&s.device.serial);
        // R14: a read that differs from this run's first backup is never
        // written back without the user choosing Restore.
        if let (CompareResult::Differs, Some(e)) = (cmp, earlier) {
            self.view = View::DataChanged {
                device,
                earlier_backup_at: e.created_at.clone(),
            };
            return;
        }
        match r {
            Ok(b) => {
                self.ctx
                    .first_in_run
                    .entry(s.device.serial.clone())
                    .or_insert_with(|| b.backup_ref().clone());
                self.view = View::Ready {
                    device,
                    attempt: s.attempt,
                    backup_at: b.backup_ref().created_at.clone(),
                };
                s.backup = Some(b);
            }
            Err(reason) => {
                self.view = View::BackupFailed {
                    device: Some(device),
                    reason,
                    message: messages::for_reason(reason),
                }
            }
        }
    }

    /// R10, R11, R31: one click writes this session's fresh backup, once.
    fn on_boot(&mut self, fx: &mut Vec<Effect>) {
        if !matches!(self.view, View::Ready { .. }) || self.ctx.inflight.is_some() {
            return;
        }
        let Some(s) = &self.ctx.session else { return };
        if !self.alone(&s.device) {
            return;
        }
        let Some(s) = self.ctx.session.as_mut() else {
            return;
        };
        let Some(b) = s.backup.take() else { return };
        self.view = View::Booting {
            device: (&s.device).into(),
            attempt: s.attempt,
            percent: 0,
        };
        fx.push(Effect::RunDownloadLeave(s.device.serial.clone(), b));
        self.ctx.inflight = Some(Job::Download);
    }

    fn on_download_exit(&mut self, o: DfuOutcome, fx: &mut Vec<Effect>) {
        match o {
            DfuOutcome::SuccessCandidate | DfuOutcome::Ok => {
                let Some(s) = self.ctx.session.take() else {
                    return;
                };
                self.ctx.watch_seq += 1;
                let id = self.ctx.watch_seq;
                self.ctx.watch = Some(Watch {
                    id,
                    gone: !self.present(&s.device),
                    device: s.device,
                    attempt: s.attempt,
                });
                fx.push(Effect::StartWatchWindow {
                    id,
                    duration: WATCH_WINDOW,
                });
            }
            // The orchestrator's pre-write hash check failed: nothing was
            // written, so take a fresh backup.
            // Snapshots were held back during the write, so catch up first.
            DfuOutcome::Failed(Failure::Reason(ReasonCode::HashMismatch)) => {
                match &self.ctx.session {
                    Some(s) if self.alone(&s.device) => self.begin_backup(fx),
                    _ => {
                        self.ctx.session = None;
                        self.view = View::Waiting;
                        self.react_to_bus(fx);
                    }
                }
            }
            DfuOutcome::Failed(f) => {
                let (reason, message) = describe(f);
                self.failed(reason, message);
            }
        }
    }

    fn on_elapsed(&mut self, id: u32, fx: &mut Vec<Effect>) {
        let Some(w) = self.ctx.watch.take_if(|w| w.id == id) else {
            return;
        };
        if self.present(&w.device) {
            self.missed_catch(w, fx);
            return;
        }
        self.view = View::Success {
            device: (&w.device).into(),
        };
        fx.extend([Effect::PlaySuccessSound, Effect::Notify]);
        self.detect(fx);
    }

    fn on_retry(&mut self, fx: &mut Vec<Effect>) {
        let retryable = matches!(
            self.view,
            View::Failed {
                can_retry: true,
                ..
            } | View::BackupFailed { .. }
        );
        if !retryable || self.ctx.inflight.is_some() {
            return;
        }
        let resume = match &self.ctx.session {
            Some(s) => s.layout_ok && self.alone(&s.device),
            None => false,
        };
        if resume {
            if let Some(s) = self.ctx.session.as_mut() {
                s.attempt = 1;
            }
            self.begin_backup(fx);
        } else {
            self.ctx.session = None;
            self.view = View::Waiting;
            self.detect(fx);
        }
    }

    fn on_restore(&mut self, fx: &mut Vec<Effect>) {
        if !matches!(self.view, View::DataChanged { .. }) || self.ctx.inflight.is_some() {
            return;
        }
        let Some(s) = &self.ctx.session else { return };
        if !self.alone(&s.device) {
            return;
        }
        let Some(r) = self.ctx.first_in_run.get(&s.device.serial) else {
            return;
        };
        fx.push(Effect::RevalidateFirstInRun(r.clone()));
        self.ctx.inflight = Some(Job::Revalidate);
    }

    fn on_restore_validated(
        &mut self,
        r: Result<ValidatedBackup, ReasonCode>,
        fx: &mut Vec<Effect>,
    ) {
        let b = match r {
            Ok(b) => b,
            Err(reason) => return self.failed(reason, messages::for_reason(reason)),
        };
        let Some(s) = &self.ctx.session else { return };
        let sanctioned = matches!(self.view, View::DataChanged { .. })
            && self.alone(&s.device)
            && self.ctx.first_in_run.get(&s.device.serial) == Some(b.backup_ref());
        if !sanctioned {
            return self.failed(ReasonCode::Unknown, messages::UNKNOWN);
        }
        self.view = View::Restoring {
            device: (&s.device).into(),
            percent: 0,
            backup_at: b.backup_ref().created_at.clone(),
        };
        fx.push(Effect::RunDownloadLeave(s.device.serial.clone(), b));
        self.ctx.inflight = Some(Job::Download);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn pocket(serial: &str) -> DfuDeviceInfo {
        DfuDeviceInfo {
            vid: 0x0483,
            pid: 0xdf11,
            serial: Some(Serial::new(serial)),
            product: Some("DFU in FS Mode".into()),
            location_id: 1,
        }
    }

    fn other(serial: &str) -> DfuDeviceInfo {
        DfuDeviceInfo {
            vid: 0x1209,
            pid: 0x0001,
            serial: Some(Serial::new(serial)),
            product: None,
            location_id: 2,
        }
    }

    fn snap(devs: &[DfuDeviceInfo]) -> Input {
        Input::DevicesChanged(DeviceSnapshot {
            dfu_devices: devs.to_vec(),
        })
    }

    fn good_layout() -> FlashLayout {
        FlashLayout {
            base: ADDR,
            segments: vec![(4, 16), (1, 64), (7, 128)],
        }
    }

    fn backup(name: &str) -> ValidatedBackup {
        ValidatedBackup::for_test(BackupRef {
            path: PathBuf::from(format!("/b/{name}.bin")),
            sha256: [0; 32],
            created_at: format!("2026-10-21T10:00:{name}Z"),
        })
    }

    fn sa() -> Option<Serial> {
        Some(Serial::new("A"))
    }

    struct Run {
        s: AppState,
        fx: Vec<Effect>,
    }

    impl Run {
        fn new() -> Self {
            Run {
                s: AppState::new(),
                fx: vec![],
            }
        }

        fn send(&mut self, i: Input) -> &[Effect] {
            let (s, fx) = step(&self.s, i);
            self.s = s;
            self.fx = fx;
            &self.fx
        }

        fn kind(&self) -> String {
            serde_json::to_value(&self.s).unwrap()["kind"]
                .as_str()
                .unwrap()
                .to_string()
        }

        fn waiting() -> Self {
            let mut r = Run::new();
            assert!(r.send(Input::SidecarCheck(Ok(()))).is_empty());
            assert_eq!(r.kind(), "waiting");
            r
        }

        /// Plug in A, pass the layout probe: upload in flight.
        fn backing_up() -> Self {
            let mut r = Run::waiting();
            r.plug(&[pocket("A")]);
            assert_eq!(r.fx, [Effect::RunList(sa())]);
            assert_eq!(r.kind(), "backingUp");
            assert_eq!(
                r.send(Input::LayoutResult(Ok(good_layout()))),
                [Effect::RunUpload(sa())]
            );
            r
        }

        fn ready() -> Self {
            let mut r = Run::backing_up();
            r.finish_upload(None);
            r.send(Input::BackupValidated(
                Ok(backup("01")),
                CompareResult::NoEarlier,
            ));
            assert_eq!(r.kind(), "ready");
            r
        }

        fn booting() -> Self {
            let mut r = Run::ready();
            assert_eq!(
                r.send(Input::UserBoot),
                [Effect::RunDownloadLeave(sa(), backup("01"))]
            );
            assert_eq!(r.kind(), "booting");
            r
        }

        /// Two identical polls (R7).
        fn plug(&mut self, devs: &[DfuDeviceInfo]) {
            self.send(snap(devs));
            self.send(snap(devs));
        }

        fn finish_upload(&mut self, earlier: Option<BackupRef>) {
            assert_eq!(
                self.send(Input::DfuExited(DfuOutcome::Ok)),
                [Effect::ValidateBackup {
                    serial: sa(),
                    earlier
                }]
            );
        }

        fn watch_id(&self) -> u32 {
            match self.fx.as_slice() {
                [Effect::StartWatchWindow { id, duration }] => {
                    assert_eq!(*duration, WATCH_WINDOW);
                    *id
                }
                fx => panic!("expected one StartWatchWindow, got {fx:?}"),
            }
        }
    }

    fn json(r: &Run) -> serde_json::Value {
        serde_json::to_value(&r.s).unwrap()
    }

    // ---- starting ----

    #[test]
    fn starting_goes_to_waiting_or_fatal() {
        Run::waiting();
        let mut r = Run::new();
        assert_eq!(r.kind(), "starting");
        r.send(Input::SidecarCheck(Err(ReasonCode::SidecarMissing)));
        assert_eq!(
            json(&r),
            serde_json::json!({"kind": "fatal", "reason": "sidecarMissing", "message": messages::FATAL})
        );
        r.plug(&[pocket("A")]);
        assert!(r.fx.is_empty());
        assert_eq!(r.kind(), "fatal");
    }

    #[test]
    fn devices_seen_while_starting_are_used_after_the_check() {
        let mut r = Run::new();
        r.plug(&[pocket("A")]);
        assert!(r.fx.is_empty());
        assert_eq!(r.send(Input::SidecarCheck(Ok(()))), [Effect::RunList(sa())]);
    }

    // ---- waiting / debounce ----

    #[test]
    fn detection_needs_two_identical_polls() {
        let mut r = Run::waiting();
        assert!(r.send(snap(&[pocket("A")])).is_empty());
        assert_eq!(r.kind(), "waiting");
        assert!(r.send(snap(&[pocket("C")])).is_empty());
        assert_eq!(
            r.send(snap(&[pocket("C")])),
            [Effect::RunList(Some(Serial::new("C")))]
        );
        assert_eq!(r.kind(), "backingUp");
    }

    #[test]
    fn a_non_pocket_dfu_device_is_never_touched() {
        let mut r = Run::waiting();
        r.plug(&[other("X")]);
        r.send(snap(&[other("X")]));
        assert!(r.fx.is_empty());
        assert_eq!(r.kind(), "waiting");
    }

    // ---- conflict ----

    #[test]
    fn conflict_entry_and_exit() {
        let mut r = Run::waiting();
        assert!(r.send(snap(&[pocket("A"), other("X")])).is_empty());
        assert_eq!(json(&r)["kind"], "conflict");
        assert_eq!(json(&r)["devices"].as_array().unwrap().len(), 2);
        assert_eq!(json(&r)["devices"][1]["vid"], "1209");
        assert!(r.send(snap(&[])).is_empty());
        assert_eq!(r.kind(), "waiting");
        r.send(snap(&[pocket("A"), pocket("C")]));
        assert_eq!(r.kind(), "conflict");
        assert!(r.send(snap(&[pocket("A")])).is_empty());
        assert_eq!(r.send(snap(&[pocket("A")])), [Effect::RunList(sa())]);
        assert_eq!(r.kind(), "backingUp");
    }

    #[test]
    fn second_device_during_backup_discards_the_late_upload() {
        let mut r = Run::backing_up();
        assert!(r.send(snap(&[pocket("A"), other("X")])).is_empty());
        assert_eq!(r.kind(), "conflict");
        assert_eq!(
            r.send(Input::DfuExited(DfuOutcome::Ok)),
            [Effect::DeletePartial]
        );
        assert_eq!(r.kind(), "conflict");
        r.plug(&[pocket("A")]);
        assert_eq!(r.fx, [Effect::RunList(sa())]);
    }

    #[test]
    fn second_device_during_list_discards_the_layout() {
        let mut r = Run::waiting();
        r.plug(&[pocket("A")]);
        r.send(snap(&[pocket("A"), pocket("C")]));
        assert!(r.send(Input::LayoutResult(Ok(good_layout()))).is_empty());
        assert_eq!(r.kind(), "conflict");
    }

    #[test]
    fn second_device_during_validation_discards_the_backup() {
        let mut r = Run::backing_up();
        r.finish_upload(None);
        r.send(snap(&[pocket("A"), pocket("C")]));
        let fx = r.send(Input::BackupValidated(
            Ok(backup("01")),
            CompareResult::NoEarlier,
        ));
        assert!(fx.is_empty());
        assert_eq!(r.kind(), "conflict");
        // The discarded backup did not become this run's first (R14).
        r.plug(&[pocket("A")]);
        r.send(Input::LayoutResult(Ok(good_layout())));
        r.finish_upload(None);
    }

    #[test]
    fn second_device_in_ready_blocks_boot() {
        let mut r = Run::ready();
        r.send(snap(&[pocket("A"), other("X")]));
        assert_eq!(r.kind(), "conflict");
        assert!(r.send(Input::UserBoot).is_empty());
        // Back to one device: a fresh backup, never the old one (R11).
        r.plug(&[pocket("A")]);
        assert_eq!(r.fx, [Effect::RunList(sa())]);
    }

    #[test]
    fn second_device_in_data_changed_blocks_restore() {
        let mut r = data_changed();
        r.send(snap(&[pocket("A"), other("X")]));
        assert_eq!(r.kind(), "conflict");
        assert!(r.send(Input::UserRestore).is_empty());
    }

    #[test]
    fn booting_is_not_interrupted_by_a_snapshot() {
        let mut r = Run::booting();
        assert!(r.send(snap(&[pocket("A"), other("X")])).is_empty());
        assert!(r.send(snap(&[])).is_empty());
        assert_eq!(r.kind(), "booting");
        r.send(Input::DfuExited(DfuOutcome::SuccessCandidate));
        r.watch_id();
        r.send(snap(&[pocket("A"), other("X")]));
        assert_eq!(r.kind(), "conflict");
    }

    // ---- unsupported ----

    #[test]
    fn wrong_layout_is_unsupported_until_removed() {
        let mut r = Run::waiting();
        r.plug(&[pocket("A")]);
        let bad = FlashLayout {
            base: ADDR,
            segments: vec![(1, 128)],
        };
        assert!(r.send(Input::LayoutResult(Ok(bad))).is_empty());
        assert_eq!(json(&r)["kind"], "unsupported");
        assert_eq!(json(&r)["device"]["serial"], "A");
        r.send(snap(&[pocket("A")]));
        assert_eq!(r.kind(), "unsupported");
        r.send(snap(&[]));
        assert_eq!(r.kind(), "waiting");
    }

    #[test]
    fn layout_errors_route_per_table() {
        let cases = [
            (Failure::Unsupported, "unsupported"),
            (Failure::Conflict, "conflict"),
            (Failure::NoDevice, "waiting"),
            (Failure::Reason(ReasonCode::Busy), "backupFailed"),
        ];
        for (f, kind) in cases {
            let mut r = Run::waiting();
            r.plug(&[pocket("A")]);
            assert!(r.send(Input::LayoutResult(Err(f))).is_empty(), "{f:?}");
            assert_eq!(r.kind(), kind, "{f:?}");
        }
    }

    // ---- backingUp ----

    #[test]
    fn progress_updates_the_percent() {
        let mut r = Run::backing_up();
        r.send(Input::DfuEvent(DfuEvent::Progress {
            op: ProgressOp::Upload,
            percent: 40,
        }));
        assert_eq!(json(&r)["percent"], 40);
        let mut r = Run::booting();
        r.send(Input::DfuEvent(DfuEvent::Progress {
            op: ProgressOp::Erase,
            percent: 50,
        }));
        assert_eq!(json(&r)["percent"], 0);
        r.send(Input::DfuEvent(DfuEvent::Progress {
            op: ProgressOp::Download,
            percent: 70,
        }));
        assert_eq!(json(&r)["percent"], 70);
    }

    #[test]
    fn device_gone_during_backup_is_silent_r21() {
        let mut r = Run::backing_up();
        assert_eq!(
            r.send(Input::DfuExited(DfuOutcome::Failed(Failure::NoDevice))),
            [Effect::DeletePartial]
        );
        assert_eq!(r.kind(), "waiting");
        // A stale snapshot still listing the device does not re-detect at once.
        assert!(r.send(snap(&[pocket("A")])).is_empty());
        assert_eq!(r.send(snap(&[pocket("A")])), [Effect::RunList(sa())]);
    }

    #[test]
    fn device_removed_during_backup_waits_and_drops_the_result() {
        let mut r = Run::backing_up();
        assert!(r.send(snap(&[])).is_empty());
        assert_eq!(r.kind(), "waiting");
        assert_eq!(
            r.send(Input::DfuExited(DfuOutcome::Ok)),
            [Effect::DeletePartial]
        );
        assert_eq!(r.kind(), "waiting");
    }

    #[test]
    fn upload_error_is_backup_failed_and_retry_rereads() {
        let mut r = Run::backing_up();
        let fx = r.send(Input::DfuExited(DfuOutcome::Failed(Failure::Reason(
            ReasonCode::Busy,
        ))));
        assert_eq!(fx, [Effect::DeletePartial]);
        assert_eq!(json(&r)["kind"], "backupFailed");
        assert_eq!(json(&r)["reason"], "busy");
        assert_eq!(json(&r)["message"], messages::BUSY);
        assert_eq!(r.send(Input::UserRetry), [Effect::RunUpload(sa())]);
        assert_eq!(r.kind(), "backingUp");
    }

    #[test]
    fn internal_file_error_uses_internal_copy() {
        let mut r = Run::backing_up();
        r.send(Input::DfuExited(DfuOutcome::Failed(Failure::Internal)));
        assert_eq!(json(&r)["reason"], "unknown");
        assert_eq!(json(&r)["message"], messages::INTERNAL);
    }

    #[test]
    fn validation_failure_is_backup_failed_then_removal_waits() {
        let mut r = Run::backing_up();
        r.finish_upload(None);
        assert!(r
            .send(Input::BackupValidated(
                Err(ReasonCode::ValidationSize),
                CompareResult::Incomparable
            ))
            .is_empty());
        assert_eq!(json(&r)["message"], messages::VALIDATION_SIZE);
        r.send(snap(&[]));
        assert_eq!(r.kind(), "waiting");
    }

    #[test]
    fn valid_backup_is_ready_attempt_one() {
        let r = Run::ready();
        assert_eq!(
            json(&r),
            serde_json::json!({
                "kind": "ready",
                "device": {"vid": "0483", "pid": "df11", "serial": "A", "product": "DFU in FS Mode"},
                "attempt": 1,
                "backupAt": "2026-10-21T10:00:01Z",
            })
        );
    }

    // ---- ready / booting ----

    #[test]
    fn boot_only_from_ready_and_only_once_r31() {
        let mut r = Run::waiting();
        assert!(r.send(Input::UserBoot).is_empty());
        let mut r = Run::backing_up();
        assert!(r.send(Input::UserBoot).is_empty());
        let mut r = Run::booting();
        assert!(r.send(Input::UserBoot).is_empty());
        assert_eq!(json(&r)["attempt"], 1);
    }

    #[test]
    fn unplug_in_ready_waits_and_replug_takes_a_fresh_backup_r11() {
        let mut r = Run::ready();
        r.send(snap(&[]));
        assert_eq!(r.kind(), "waiting");
        assert!(r.send(Input::UserBoot).is_empty());
        r.plug(&[pocket("A")]);
        assert_eq!(r.fx, [Effect::RunList(sa())]);
        r.send(Input::LayoutResult(Ok(good_layout())));
        r.finish_upload(Some(backup("01").backup_ref().clone()));
        r.send(Input::BackupValidated(
            Ok(backup("02")),
            CompareResult::Same,
        ));
        assert_eq!(
            r.send(Input::UserBoot),
            [Effect::RunDownloadLeave(sa(), backup("02"))]
        );
    }

    #[test]
    fn no_reenumeration_in_the_window_is_success() {
        let mut r = Run::booting();
        r.send(Input::DfuExited(DfuOutcome::SuccessCandidate));
        let id = r.watch_id();
        assert_eq!(r.kind(), "booting");
        r.send(snap(&[]));
        r.send(snap(&[]));
        assert_eq!(
            r.send(Input::WatchWindowElapsed(id)),
            [Effect::PlaySuccessSound, Effect::Notify]
        );
        assert_eq!(r.kind(), "success");
        // A new DFU session starts over (state table: success row).
        r.plug(&[pocket("A")]);
        assert_eq!(r.fx, [Effect::RunList(sa())]);
    }

    #[test]
    fn reenumeration_in_the_window_is_attempt_two() {
        let mut r = Run::booting();
        r.send(Input::DfuExited(DfuOutcome::SuccessCandidate));
        let id = r.watch_id();
        r.send(snap(&[]));
        assert!(r.send(snap(&[pocket("A")])).is_empty());
        // Layout already passed for this device, so straight to the upload.
        assert_eq!(r.send(snap(&[pocket("A")])), [Effect::RunUpload(sa())]);
        assert_eq!(r.kind(), "backingUp");
        assert!(r.send(Input::WatchWindowElapsed(id)).is_empty());
        r.finish_upload(Some(backup("01").backup_ref().clone()));
        r.send(Input::BackupValidated(
            Ok(backup("02")),
            CompareResult::Same,
        ));
        assert_eq!(json(&r)["kind"], "ready");
        assert_eq!(json(&r)["attempt"], 2);
    }

    #[test]
    fn device_that_never_left_is_a_missed_catch() {
        let mut r = Run::booting();
        r.send(Input::DfuExited(DfuOutcome::SuccessCandidate));
        let id = r.watch_id();
        r.send(snap(&[pocket("A")]));
        assert_eq!(
            r.send(Input::WatchWindowElapsed(id)),
            [Effect::RunUpload(sa())]
        );
        assert_eq!(r.kind(), "backingUp");
    }

    #[test]
    fn a_stale_watch_timer_is_ignored() {
        let mut r = Run::booting();
        r.send(Input::DfuExited(DfuOutcome::SuccessCandidate));
        let id = r.watch_id();
        r.send(snap(&[]));
        assert!(r.send(Input::WatchWindowElapsed(id + 1)).is_empty());
        assert_eq!(r.kind(), "booting");
    }

    #[test]
    fn unplug_before_download_done_fails_then_rechecks_on_reconnect() {
        let mut r = Run::booting();
        let fx = r.send(Input::DfuExited(DfuOutcome::Failed(Failure::Reason(
            ReasonCode::DeviceGone,
        ))));
        assert!(fx.is_empty());
        assert_eq!(json(&r)["kind"], "failed");
        assert_eq!(json(&r)["message"], messages::DEVICE_GONE);
        assert_eq!(json(&r)["canRetry"], true);
        r.send(snap(&[]));
        assert_eq!(r.kind(), "failed");
        r.plug(&[pocket("A")]);
        assert_eq!(r.fx, [Effect::RunList(sa())]);
    }

    #[test]
    fn retry_after_failed_rereads_with_attempt_one() {
        let mut r = Run::booting();
        r.send(Input::DfuExited(DfuOutcome::Failed(Failure::Reason(
            ReasonCode::UsbIo,
        ))));
        assert_eq!(r.send(Input::UserRetry), [Effect::RunUpload(sa())]);
        r.finish_upload(Some(backup("01").backup_ref().clone()));
        r.send(Input::BackupValidated(
            Ok(backup("02")),
            CompareResult::Same,
        ));
        assert_eq!(json(&r)["attempt"], 1);
    }

    #[test]
    fn retry_without_the_device_waits() {
        let mut r = Run::booting();
        r.send(Input::DfuExited(DfuOutcome::Failed(Failure::Reason(
            ReasonCode::UsbIo,
        ))));
        r.send(snap(&[]));
        assert!(r.send(Input::UserRetry).is_empty());
        assert_eq!(r.kind(), "waiting");
    }

    #[test]
    fn timeout_and_not_writeable_cannot_retry() {
        for reason in [ReasonCode::Timeout, ReasonCode::NotWriteable] {
            let mut r = Run::booting();
            r.send(Input::DfuExited(DfuOutcome::Failed(Failure::Reason(
                reason,
            ))));
            assert_eq!(json(&r)["canRetry"], false);
            assert!(r.send(Input::UserRetry).is_empty());
            assert_eq!(r.kind(), "failed");
        }
    }

    #[test]
    fn hash_mismatch_before_write_makes_a_fresh_backup() {
        let mut r = Run::booting();
        let fx = r.send(Input::DfuExited(DfuOutcome::Failed(Failure::Reason(
            ReasonCode::HashMismatch,
        ))));
        assert_eq!(fx, [Effect::RunUpload(sa())]);
        assert_eq!(r.kind(), "backingUp");
    }

    /// Snapshots are held back during the write, so the bus can change under
    /// it. A failed pre-write check catches up before reading again (R10).
    #[test]
    fn hash_mismatch_catches_up_with_the_bus_first() {
        let mut r = Run::booting();
        r.send(snap(&[pocket("A"), other("X")]));
        let fx = r.send(Input::DfuExited(DfuOutcome::Failed(Failure::Reason(
            ReasonCode::HashMismatch,
        ))));
        assert!(fx.is_empty(), "{fx:?}");
        assert_eq!(r.kind(), "conflict");

        let mut r = Run::booting();
        r.send(snap(&[]));
        let fx = r.send(Input::DfuExited(DfuOutcome::Failed(Failure::Reason(
            ReasonCode::HashMismatch,
        ))));
        assert!(fx.is_empty(), "{fx:?}");
        assert_eq!(r.kind(), "waiting");
    }

    // ---- dataChanged / restoring ----

    /// Boot interrupted before `Download done.`, replug, and the fresh read
    /// is blank: R14 says `dataChanged`, not `backupFailed`.
    fn data_changed() -> Run {
        let mut r = Run::booting();
        r.send(Input::DfuExited(DfuOutcome::Failed(Failure::Reason(
            ReasonCode::DeviceGone,
        ))));
        r.send(snap(&[]));
        r.plug(&[pocket("A")]);
        r.send(Input::LayoutResult(Ok(good_layout())));
        r.finish_upload(Some(backup("01").backup_ref().clone()));
        r.send(Input::BackupValidated(
            Err(ReasonCode::ValidationBlank),
            CompareResult::Differs,
        ));
        r
    }

    #[test]
    fn blank_read_after_an_interrupted_boot_is_data_changed() {
        let r = data_changed();
        assert_eq!(json(&r)["kind"], "dataChanged");
        assert_eq!(json(&r)["earlierBackupAt"], "2026-10-21T10:00:01Z");
        assert!(r.fx.is_empty());
    }

    #[test]
    fn restore_revalidates_then_writes_the_first_backup() {
        let mut r = data_changed();
        assert!(r.send(Input::UserBoot).is_empty());
        assert_eq!(
            r.send(Input::UserRestore),
            [Effect::RevalidateFirstInRun(
                backup("01").backup_ref().clone()
            )]
        );
        assert!(r.send(Input::UserRestore).is_empty());
        assert_eq!(
            r.send(Input::RestoreValidated(Ok(backup("01")))),
            [Effect::RunDownloadLeave(sa(), backup("01"))]
        );
        assert_eq!(json(&r)["kind"], "restoring");
        assert_eq!(json(&r)["backupAt"], "2026-10-21T10:00:01Z");
        r.send(Input::DfuExited(DfuOutcome::SuccessCandidate));
        let id = r.watch_id();
        r.send(snap(&[]));
        r.send(Input::WatchWindowElapsed(id));
        assert_eq!(r.kind(), "success");
    }

    #[test]
    fn failed_revalidation_is_failed_with_no_write() {
        let mut r = data_changed();
        r.send(Input::UserRestore);
        let fx = r.send(Input::RestoreValidated(Err(ReasonCode::HashMismatch)));
        assert!(fx.is_empty());
        assert_eq!(json(&r)["kind"], "failed");
        assert_eq!(json(&r)["reason"], "hashMismatch");
    }

    #[test]
    fn data_changed_device_removed_waits() {
        let mut r = data_changed();
        r.send(snap(&[]));
        assert_eq!(r.kind(), "waiting");
    }

    #[test]
    fn failed_without_device_omits_the_field() {
        let v = View::Failed {
            device: None,
            reason: ReasonCode::Unknown,
            message: messages::UNKNOWN,
            can_retry: true,
        };
        assert_eq!(
            serde_json::to_value(v).unwrap(),
            serde_json::json!({"kind": "failed", "reason": "unknown", "message": messages::UNKNOWN, "canRetry": true})
        );
    }

    // ---- safety invariants (Test plan 3) ----

    #[derive(Debug, Clone)]
    enum Act {
        Snap(usize),
        /// Answer the pending effect; the number picks the outcome.
        Finish(u8),
        Boot,
        Retry,
        Restore,
        Elapse,
    }

    fn act() -> impl Strategy<Value = Act> {
        prop_oneof![
            // Mostly a lone Pocket, so runs get deep enough to write.
            4 => Just(Act::Snap(1)),
            2 => (0..SNAPS).prop_map(Act::Snap),
            6 => any::<u8>().prop_map(Act::Finish),
            2 => Just(Act::Boot),
            1 => Just(Act::Retry),
            2 => Just(Act::Restore),
            1 => Just(Act::Elapse),
        ]
    }

    const SNAPS: usize = 7;

    fn snapshot(k: usize) -> Vec<DfuDeviceInfo> {
        let n = DfuDeviceInfo {
            serial: None,
            location_id: 7,
            ..pocket("unused")
        };
        match k {
            0 => vec![],
            1 => vec![pocket("A")],
            2 => vec![pocket("C")],
            3 => vec![pocket("A"), other("X")],
            4 => vec![other("X")],
            5 => vec![n],
            _ => vec![pocket("A"), pocket("C")],
        }
    }

    #[derive(Debug)]
    enum Pending {
        List,
        Upload,
        Validate(Option<BackupRef>),
        Download,
        Revalidate(BackupRef),
    }

    /// A fresh validated read the harness handed over and the machine
    /// accepted into `ready`.
    struct Delivered {
        serial: Option<Serial>,
        tainted: bool,
        used: bool,
    }

    #[derive(Default)]
    struct Harness {
        pending: Option<Pending>,
        watch: Option<u32>,
        devices: Vec<DfuDeviceInfo>,
        delivered: HashMap<PathBuf, Delivered>,
        first: HashMap<Option<Serial>, BackupRef>,
        seq: u32,
    }

    impl Harness {
        fn input(&mut self, a: &Act) -> Option<Input> {
            Some(match a {
                Act::Snap(k) => {
                    self.devices = snapshot(*k);
                    for d in self.delivered.values_mut() {
                        if !self.devices.iter().any(|x| x.serial == d.serial) {
                            d.tainted = true;
                        }
                    }
                    snap(&self.devices)
                }
                Act::Boot => Input::UserBoot,
                Act::Retry => Input::UserRetry,
                Act::Restore => Input::UserRestore,
                Act::Elapse => Input::WatchWindowElapsed(self.watch.take()?),
                Act::Finish(v) => self.finish(*v)?,
            })
        }

        fn finish(&mut self, v: u8) -> Option<Input> {
            let fail = |f| Input::DfuExited(DfuOutcome::Failed(f));
            Some(match self.pending.take()? {
                Pending::List => match v % 4 {
                    0 | 1 => Input::LayoutResult(Ok(good_layout())),
                    2 => Input::LayoutResult(Err(Failure::NoDevice)),
                    _ => Input::LayoutResult(Err(Failure::Unsupported)),
                },
                Pending::Upload => match v % 4 {
                    0 | 1 => Input::DfuExited(DfuOutcome::Ok),
                    2 => fail(Failure::NoDevice),
                    _ => fail(Failure::Reason(ReasonCode::UsbIo)),
                },
                Pending::Validate(earlier) => {
                    self.seq += 1;
                    let ok = Ok(backup(&format!("{:04}", self.seq)));
                    let cmp = match (&earlier, v % 2) {
                        (None, _) => CompareResult::NoEarlier,
                        (Some(_), 0) => CompareResult::Same,
                        _ => CompareResult::Differs,
                    };
                    match v % 3 {
                        0 => Input::BackupValidated(Err(ReasonCode::ValidationBlank), cmp),
                        _ => Input::BackupValidated(ok, cmp),
                    }
                }
                Pending::Download => match v % 3 {
                    0 => Input::DfuExited(DfuOutcome::SuccessCandidate),
                    1 => fail(Failure::Reason(ReasonCode::DeviceGone)),
                    _ => fail(Failure::Reason(ReasonCode::HashMismatch)),
                },
                Pending::Revalidate(r) => match v % 2 {
                    0 => Input::RestoreValidated(Ok(ValidatedBackup::for_test(r))),
                    _ => Input::RestoreValidated(Err(ReasonCode::HashMismatch)),
                },
            })
        }

        /// Check every effect of one step and track what is now pending.
        fn observe(
            &mut self,
            input: &Input,
            was_idle: bool,
            after: &AppState,
            fx: &[Effect],
        ) -> Result<(), TestCaseError> {
            if let (Input::BackupValidated(Ok(b), cmp), View::Ready { .. }) = (input, after.view())
            {
                prop_assert_ne!(*cmp, CompareResult::Differs);
                let serial = match &self.devices[..] {
                    [d] => d.serial.clone(),
                    _ => return Err(TestCaseError::fail("ready without one device")),
                };
                self.first
                    .entry(serial.clone())
                    .or_insert_with(|| b.backup_ref().clone());
                self.delivered.insert(
                    b.path().to_path_buf(),
                    Delivered {
                        serial,
                        tainted: false,
                        used: false,
                    },
                );
            }
            for e in fx {
                let job = match e {
                    Effect::RunList(_) => Some(Pending::List),
                    Effect::RunUpload(_) => Some(Pending::Upload),
                    Effect::ValidateBackup { earlier, .. } => {
                        Some(Pending::Validate(earlier.clone()))
                    }
                    Effect::RevalidateFirstInRun(r) => Some(Pending::Revalidate(r.clone())),
                    Effect::RunDownloadLeave(serial, b) => {
                        self.check_write(input, serial, b)?;
                        Some(Pending::Download)
                    }
                    Effect::StartWatchWindow { id, .. } => {
                        self.watch = Some(*id);
                        None
                    }
                    _ => None,
                };
                if let Some(job) = job {
                    prop_assert!(self.pending.is_none(), "{e:?} while {:?}", self.pending);
                    if matches!(job, Pending::Download) {
                        prop_assert!(was_idle, "write while another op was in flight");
                    }
                    self.pending = Some(job);
                }
            }
            Ok(())
        }

        fn check_write(
            &mut self,
            input: &Input,
            serial: &Option<Serial>,
            b: &ValidatedBackup,
        ) -> Result<(), TestCaseError> {
            let [d] = &self.devices[..] else {
                return Err(TestCaseError::fail(format!(
                    "write with {} devices",
                    self.devices.len()
                )));
            };
            prop_assert!(d.is_pocket());
            prop_assert_eq!(&d.serial, serial);
            match input {
                Input::UserBoot => {
                    let Some(rec) = self.delivered.get_mut(b.path()) else {
                        return Err(TestCaseError::fail("boot with an undelivered backup"));
                    };
                    prop_assert_eq!(&rec.serial, serial, "backup from another serial");
                    prop_assert!(!rec.tainted, "backup from an earlier session");
                    prop_assert!(!rec.used, "backup reused");
                    rec.used = true;
                }
                Input::RestoreValidated(Ok(_)) => {
                    prop_assert_eq!(self.first.get(serial), Some(b.backup_ref()));
                }
                other => return Err(TestCaseError::fail(format!("write on {other:?}"))),
            }
            Ok(())
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        #[test]
        fn writes_only_from_a_sanctioned_backup(
            to_data_changed in any::<bool>(),
            acts in prop::collection::vec(act(), 1..80),
        ) {
            let mut h = Harness::default();
            let (mut s, _) = step(&AppState::new(), Input::SidecarCheck(Ok(())));
            // Random walks rarely reach `dataChanged`, so half the cases start
            // there: back up A, replug, and read something different.
            let prefix = if to_data_changed {
                use Act::{Finish, Snap};
                vec![
                    Snap(1), Snap(1), Finish(0), Finish(0), Finish(1),
                    Snap(0), Snap(1), Snap(1), Finish(0), Finish(0), Finish(1),
                ]
            } else {
                vec![]
            };
            for a in prefix.iter().chain(&acts) {
                let Some(input) = h.input(a) else { continue };
                let was_idle = h.pending.is_none();
                let (next, fx) = step(&s, input.clone());
                h.observe(&input, was_idle, &next, &fx)?;
                s = next;
            }
        }
    }
}
