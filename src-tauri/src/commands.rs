//! The frontend's whole API (R15). No command takes a path, address,
//! length, VID/PID, or data; they only send an `Input` or read state.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_notification::NotificationExt;
use tokio::sync::mpsc::UnboundedSender;

use crate::backup::{manager, BackupManager};
use crate::diagnostics::{self, LogLine, Report};
use crate::machine::{AppState, Input};
use crate::orchestrator::{lock, Host, Shared};
use crate::settings::{self, Settings};

pub struct AppCtx {
    pub tx: UnboundedSender<Input>,
    pub shared: Arc<Shared>,
    pub manager: Arc<BackupManager>,
    pub settings_path: PathBuf,
    pub app_version: String,
}

pub struct TauriHost {
    pub app: AppHandle,
}

impl Host for TauriHost {
    fn state_changed(&self, state: &AppState) {
        let _ = self.app.emit("state-changed", state);
    }

    fn log(&self, line: &LogLine) {
        let _ = self.app.emit("log-line", line);
    }

    fn play_sound(&self) {
        std::thread::spawn(|| {
            let _ = Command::new("/usr/bin/afplay")
                .arg("/System/Library/Sounds/Glass.aiff")
                .status();
        });
    }

    fn notify(&self) {
        let focused = self
            .app
            .get_webview_window("main")
            .is_some_and(|w| w.is_focused().unwrap_or(false));
        if !focused {
            let _ = self
                .app
                .notification()
                .builder()
                .title("Radio is starting")
                .show();
        }
    }

    fn exit(&self) {
        self.app.exit(0);
    }
}

fn send(ctx: &AppCtx, input: Input) {
    let _ = ctx.tx.send(input);
}

#[tauri::command]
pub fn get_state(ctx: State<'_, AppCtx>) -> AppState {
    lock(&ctx.shared.state).clone()
}

#[tauri::command]
pub fn boot(ctx: State<'_, AppCtx>) {
    send(&ctx, Input::UserBoot);
}

#[tauri::command]
pub fn retry(ctx: State<'_, AppCtx>) {
    send(&ctx, Input::UserRetry);
}

#[tauri::command]
pub fn restore_earlier_copy(ctx: State<'_, AppCtx>) {
    send(&ctx, Input::UserRestore);
}

#[tauri::command]
pub fn reveal_backups(ctx: State<'_, AppCtx>) -> Result<(), String> {
    let dir = ctx.manager.reveal_dir().map_err(|e| e.to_string())?;
    Command::new("/usr/bin/open")
        .arg(dir)
        .status()
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn get_diagnostics(ctx: State<'_, AppCtx>) -> String {
    let devices = lock(&ctx.shared.devices).clone();
    let serial = devices
        .iter()
        .find(|d| d.is_pocket())
        .and_then(|d| d.serial.clone());
    let (backup_count, newest) = ctx.manager.stats(serial.as_ref());
    let newest_sha256 = newest.and_then(|name| {
        let bytes = std::fs::read(ctx.manager.serial_dir(serial.as_ref()).join(name)).ok()?;
        Some(manager::hex(&manager::sha256(&bytes)))
    });
    let os_version = Command::new("/usr/bin/sw_vers")
        .arg("-productVersion")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let report = Report {
        app_version: ctx.app_version.clone(),
        os_version,
        arch: std::env::consts::ARCH.into(),
        dfu_util_version: lock(&ctx.shared.dfu_util_version).clone(),
        state: serde_json::to_string(&*lock(&ctx.shared.state)).unwrap_or_default(),
        devices,
        layout: lock(&ctx.shared.layout).clone(),
        backup_count,
        newest_sha256,
        log: lock(&ctx.shared.log).all(),
    };
    let home = std::env::var_os("HOME").map(PathBuf::from);
    diagnostics::build(&report, home.as_deref().map(Path::new))
}

#[tauri::command]
pub fn get_log(ctx: State<'_, AppCtx>) -> Vec<LogLine> {
    lock(&ctx.shared.log).all()
}

#[tauri::command]
pub fn set_success_sound(ctx: State<'_, AppCtx>, enabled: bool) -> Result<(), String> {
    ctx.shared.sound.store(enabled, Ordering::SeqCst);
    settings::save(
        &ctx.settings_path,
        &Settings {
            success_sound: enabled,
        },
    )
    .map_err(|e| e.to_string())
}
