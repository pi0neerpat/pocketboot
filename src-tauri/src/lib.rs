pub mod backup;
pub mod commands;
pub mod device;
pub mod dfu;
pub mod diagnostics;
pub mod machine;
pub mod messages;
pub mod orchestrator;
pub mod settings;
pub mod types;

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use tauri::{Manager, WindowEvent};
use tauri_plugin_dialog::{
    DialogExt, MessageDialogButtons, MessageDialogKind, MessageDialogResult,
};

use backup::BackupManager;
use commands::{AppCtx, TauriHost};
use dfu::runner::SidecarRunner;
use machine::{Input, View};
use orchestrator::{lock, Orchestrator, Shared};

const QUIT_TITLE: &str = "The radio is restarting. Quit anyway?";

fn setup(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let handle = app.handle().clone();
    let manager = Arc::new(BackupManager::new(
        app.path().app_data_dir()?.join("backups"),
    ));
    let settings_path = app.path().app_config_dir()?.join("settings.json");
    let shared = Arc::new(Shared::new(settings::load(&settings_path).success_sound));
    let app_version = app.package_info().version.to_string();
    let runner = SidecarRunner::bundled().unwrap_or_else(|_| SidecarRunner::new(PathBuf::new()));

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let orch = Orchestrator::new(
        runner,
        TauriHost {
            app: handle.clone(),
        },
        shared.clone(),
        manager.clone(),
        tx.clone(),
        app_version.clone(),
    );
    tauri::async_runtime::spawn(orch.run(rx));

    let (snap_tx, mut snap_rx) = tokio::sync::mpsc::channel(4);
    tauri::async_runtime::spawn(device::watch(snap_tx));
    let fwd = tx.clone();
    tauri::async_runtime::spawn(async move {
        while let Some(snap) = snap_rx.recv().await {
            if fwd.send(Input::DevicesChanged(snap)).is_err() {
                break;
            }
        }
    });

    app.manage(AppCtx {
        tx,
        shared,
        manager,
        settings_path,
        app_version,
    });
    Ok(())
}

/// R34: closing mid-write asks first; Quit waits for the write to finish.
fn on_close_requested(window: &tauri::Window, api: &tauri::CloseRequestApi) {
    let app = window.app_handle().clone();
    let ctx = app.state::<AppCtx>();
    let writing = matches!(
        lock(&ctx.shared.state).view(),
        View::Booting { .. } | View::Restoring { .. }
    );
    if !writing {
        return;
    }
    api.prevent_close();
    let quit_app = app.clone();
    app.dialog()
        .message(QUIT_TITLE)
        .title("PocketBoot")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "Wait".into(),
            "Quit".into(),
        ))
        .show_with_result(move |r| {
            if r == MessageDialogResult::Custom("Quit".into()) {
                let shared = &quit_app.state::<AppCtx>().shared;
                shared.quit_requested.store(true, Ordering::SeqCst);
                if shared.may_exit() {
                    quit_app.exit(0);
                }
            }
        });
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(setup)
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                on_close_requested(window, api);
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_state,
            commands::boot,
            commands::retry,
            commands::restore_earlier_copy,
            commands::reveal_backups,
            commands::get_diagnostics,
            commands::get_log,
            commands::set_success_sound,
        ])
        .run(tauri::generate_context!())
        .expect("error while running PocketBoot");
}
