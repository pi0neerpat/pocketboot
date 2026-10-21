pub mod backup;
pub mod device;
pub mod dfu;
pub mod machine;
pub mod messages;
pub mod types;

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .run(tauri::generate_context!())
        .expect("error while running PocketBoot");
}
