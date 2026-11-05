//! TeamCraft Desktop – the rich client of the TeamCraft PLM.
//!
//! A Tauri application: the user interface (`ui/`, plain HTML/CSS/JS) runs in
//! the system web view; this Rust side talks to the TeamCraft server (REST API,
//! browser sign-in) and manages the local check-out workspace.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod acp;
mod api;
mod cadexport;
mod commands;
mod folderimport;
mod layout;
mod settings;
mod signin;
mod workspace;

use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64},
        Arc, Mutex,
    },
};

use tauri::{
    menu::{MenuBuilder, MenuItemBuilder, PredefinedMenuItem, SubmenuBuilder},
    Emitter, Manager,
};
use tokio::sync::Notify;

/// Shared state of the commands.
pub struct AppState {
    pub settings: Mutex<settings::Settings>,
    pub client: reqwest::Client,
    pub config_dir: PathBuf,
    /// Cancel signal of the running browser sign-in (one per attempt).
    pub sign_in_cancel: Mutex<Option<Arc<Notify>>>,
    /// Cancel switch of the running upload (one per upload).
    pub upload_cancel: Mutex<Arc<AtomicBool>>,
    /// The running ACP agent session, if one was started.
    pub agent: Mutex<Option<commands::RunningAgent>>,
    /// Number of the last agent session (events carry it).
    pub agent_seq: AtomicU64,
    /// Cancel flag of the running Get (workspace downloads).
    pub get_cancel: Arc<AtomicBool>,
}

/// Name of this computer, shown on the browser confirmation page and used for
/// the token name.
pub fn device_name() -> String {
    let name = gethostname::gethostname().to_string_lossy().to_string();
    let name = name.trim().trim_end_matches(".local").to_string();
    if name.is_empty() {
        "this computer".into()
    } else {
        name
    }
}

/// The window theme for a skin: the System skin follows the OS, TeamCraft
/// Light is light, TeamCraft Dark and every distro skin are dark.
fn window_theme(skin: Option<&str>) -> Option<tauri::Theme> {
    match skin {
        None | Some("") | Some("teamcraft") => None,
        Some("teamcraft-day") => Some(tauri::Theme::Light),
        Some(_) => Some(tauri::Theme::Dark),
    }
}

fn build_menu(app: &tauri::App) -> tauri::Result<()> {
    let item = |id: &str, text: &str, accel: Option<&str>| {
        let mut b = MenuItemBuilder::with_id(id, text);
        if let Some(a) = accel {
            b = b.accelerator(a);
        }
        b.build(app)
    };
    // macOS shows the first submenu as the application menu.
    #[cfg(target_os = "macos")]
    let app_menu = SubmenuBuilder::new(app, "TeamCraft")
        .item(&item("about-app", "About TeamCraft", None)?)
        .separator()
        .item(&item("settings-app", "Settings…", Some("CmdOrCtrl+,"))?)
        .separator()
        .item(&PredefinedMenuItem::hide(app, None)?)
        .item(&PredefinedMenuItem::hide_others(app, None)?)
        .separator()
        .item(&PredefinedMenuItem::quit(app, Some("Quit TeamCraft"))?)
        .build()?;
    let file = SubmenuBuilder::new(app, "File")
        .item(&item("new-item", "New Item…", Some("CmdOrCtrl+N"))?)
        .separator()
        .item(&item("sign-out", "Sign Out", None)?);
    #[cfg(not(target_os = "macos"))]
    let file = file
        .item(&item("settings", "Settings…", Some("CmdOrCtrl+,"))?)
        .separator()
        .item(&PredefinedMenuItem::quit(app, Some("Quit TeamCraft"))?);
    let file = file.build()?;
    let edit = SubmenuBuilder::new(app, "Edit")
        .item(&PredefinedMenuItem::undo(app, None)?)
        .item(&PredefinedMenuItem::redo(app, None)?)
        .separator()
        .item(&PredefinedMenuItem::cut(app, None)?)
        .item(&PredefinedMenuItem::copy(app, None)?)
        .item(&PredefinedMenuItem::paste(app, None)?)
        .item(&PredefinedMenuItem::select_all(app, None)?)
        .build()?;
    let view = SubmenuBuilder::new(app, "View")
        .item(&item("refresh", "Refresh", Some("F5"))?)
        .item(&item("search", "Search", Some("CmdOrCtrl+F"))?)
        .separator()
        .item(&item("go-home", "Home", Some("CmdOrCtrl+1"))?)
        .item(&item("go-worklist", "My Worklist", Some("CmdOrCtrl+2"))?)
        .item(&item("go-changes", "Changes", Some("CmdOrCtrl+3"))?)
        .item(&item("go-agent", "Agent", Some("CmdOrCtrl+4"))?)
        .separator()
        .item(&item("zoom-in", "Zoom In (Ctrl + wheel)", None)?)
        .item(&item("zoom-out", "Zoom Out", None)?)
        .item(&item("zoom-reset", "Actual Size", None)?)
        .build()?;
    let tools = SubmenuBuilder::new(app, "Tools")
        .item(&item("open-workspace", "Open Workspace Folder", None)?)
        .item(&item("get-projects", "Get Project Groups…", None)?)
        .item(&item("import-folder", "Import CAD Folder…", None)?)
        .item(&item("open-web", "Open in Browser", None)?)
        .build()?;
    let help = SubmenuBuilder::new(app, "Help")
        .item(&item("import-help", "How to Import a CAD Folder", None)?)
        .item(&item("about", "About TeamCraft", None)?)
        .build()?;
    let menu = MenuBuilder::new(app);
    #[cfg(target_os = "macos")]
    let menu = menu.item(&app_menu);
    let menu = menu.items(&[&file, &edit, &view, &tools, &help]).build()?;
    app.set_menu(menu)?;
    Ok(())
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let config_dir = app.path().app_config_dir()?;
            let settings = settings::Settings::load(&config_dir);
            let zoom = settings.zoom.map(commands::clamp_zoom);
            let theme = window_theme(settings.skin.as_deref());
            app.manage(AppState {
                settings: Mutex::new(settings),
                client: api::client(),
                config_dir,
                sign_in_cancel: Mutex::new(None),
                upload_cancel: Mutex::new(Arc::new(AtomicBool::new(false))),
                agent: Mutex::new(None),
                agent_seq: AtomicU64::new(0),
                get_cancel: Arc::new(AtomicBool::new(false)),
            });
            build_menu(app)?;
            if let Some(window) = app.get_webview_window("main") {
                if let Some(zoom) = zoom {
                    let _ = window.set_zoom(zoom);
                }
                // the native chrome (the GTK menu bar) in the skin's scheme
                // from the first frame; app.js keeps it in step on changes
                let _ = window.set_theme(theme);
            }
            Ok(())
        })
        .on_menu_event(|app, event| {
            // the user interface handles the menu commands
            let _ = app.emit("menu", event.id().0.clone());
        })
        .invoke_handler(tauri::generate_handler![
            commands::session,
            commands::set_server,
            commands::sign_in_browser,
            commands::cancel_sign_in,
            commands::sign_in_token,
            commands::sign_out,
            commands::set_skin,
            commands::set_workspace_dir,
            commands::api,
            commands::file_bytes,
            commands::workspace_checkout,
            commands::workspace_download,
            commands::workspace_get,
            commands::workspace_cancel,
            commands::workspace_migration_plan,
            commands::workspace_migrate,
            commands::workspace_status,
            commands::workspace_checkin,
            commands::workspace_open,
            commands::open_web,
            commands::set_zoom,
            commands::save_package,
            commands::save_stl,
            commands::upload_files,
            commands::cancel_upload,
            commands::upload_request_attachment,
            commands::cad_programs,
            commands::cad_export_step,
            commands::scan_folder,
            commands::agent_profiles,
            commands::set_agent_profiles,
            commands::agent_status,
            commands::agent_start,
            commands::agent_prompt,
            commands::agent_permission,
            commands::agent_cancel,
            commands::agent_stop,
        ])
        .run(tauri::generate_context!())
        .expect("error while running TeamCraft Desktop");
}
