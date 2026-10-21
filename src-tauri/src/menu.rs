// The macOS app menu (R45). Every item calls the same helpers as the IPC
// commands, so the menu and the window never disagree.

use tauri::menu::{
    AboutMetadata, CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu,
};
use tauri::{AppHandle, Manager, Wry};
use tauri_plugin_clipboard_manager::ClipboardExt;

use crate::commands::{self, AppCtx};

const LICENSES: &str = "licenses";
const SOUND: &str = "sound";
const REVEAL: &str = "reveal-backups";
const DIAGNOSTICS: &str = "copy-diagnostics";
const QUIT: &str = "quit";

/// The sound tick, kept so the handler can read it without walking submenus.
struct SoundItem(CheckMenuItem<Wry>);

/// Built before `setup`, so the sound tick starts from the stored setting.
pub fn build(app: &AppHandle) -> tauri::Result<Menu<Wry>> {
    let sound_on = app
        .path()
        .app_config_dir()
        .map(|d| crate::settings::load(&d.join("settings.json")).success_sound)
        .unwrap_or(true);
    let about = AboutMetadata {
        version: Some(app.package_info().version.to_string()),
        ..Default::default()
    };
    let sound = CheckMenuItem::with_id(
        app,
        SOUND,
        "Play Sound on Success",
        true,
        sound_on,
        None::<&str>,
    )?;
    app.manage(SoundItem(sound.clone()));
    let app_menu = Submenu::with_items(
        app,
        "PocketBoot",
        true,
        &[
            &PredefinedMenuItem::about(app, Some("About PocketBoot"), Some(about))?,
            &MenuItem::with_id(app, LICENSES, "Licenses…", true, None::<&str>)?,
            &PredefinedMenuItem::separator(app)?,
            &sound,
            &MenuItem::with_id(app, REVEAL, "Reveal Backups in Finder", true, None::<&str>)?,
            &MenuItem::with_id(app, DIAGNOSTICS, "Copy Diagnostics", true, None::<&str>)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::hide(app, None)?,
            &PredefinedMenuItem::hide_others(app, None)?,
            &PredefinedMenuItem::show_all(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, QUIT, "Quit PocketBoot", true, Some("CmdOrCtrl+Q"))?,
        ],
    )?;
    // Keeps Cmd+C working in the log box.
    let edit = Submenu::with_items(
        app,
        "Edit",
        true,
        &[
            &PredefinedMenuItem::undo(app, None)?,
            &PredefinedMenuItem::redo(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::cut(app, None)?,
            &PredefinedMenuItem::copy(app, None)?,
            &PredefinedMenuItem::paste(app, None)?,
            &PredefinedMenuItem::select_all(app, None)?,
        ],
    )?;
    let window = Submenu::with_items(
        app,
        "Window",
        true,
        &[
            &PredefinedMenuItem::minimize(app, None)?,
            &PredefinedMenuItem::close_window(app, None)?,
        ],
    )?;
    Menu::with_items(app, &[&app_menu, &edit, &window])
}

pub fn on_event(app: &AppHandle, event: MenuEvent) {
    let Some(ctx) = app.try_state::<AppCtx>() else {
        return;
    };
    let result = match event.id().as_ref() {
        LICENSES => app
            .path()
            .resource_dir()
            .map_err(|e| e.to_string())
            .and_then(|d| commands::reveal(&d.join("licenses"))),
        // macOS has already flipped the tick; persist it, don't toggle again.
        SOUND => app
            .state::<SoundItem>()
            .0
            .is_checked()
            .map_err(|e| e.to_string())
            .and_then(|on| commands::set_sound(&ctx, on)),
        REVEAL => commands::reveal_backups_dir(&ctx),
        DIAGNOSTICS => app
            .clipboard()
            .write_text(commands::diagnostics_text(&ctx))
            .map_err(|e| e.to_string()),
        QUIT => {
            if !crate::confirm_quit(app) {
                app.exit(0);
            }
            Ok(())
        }
        _ => Ok(()),
    };
    if let Err(e) = result {
        eprintln!("menu {}: {e}", event.id().as_ref());
    }
}
