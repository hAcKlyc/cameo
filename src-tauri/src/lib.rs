//! Cameo — Tauri 2 entry point.
//!
//! Wires up logging, the global `~/.cameo` data dir, the Cameo image protocol,
//! the Board registry (in-memory doc authority), the Codex runtime
//! registry (one app-server per Board), and all commands. Codex sessions are
//! tree-killed on app exit.

pub mod assets;
pub mod board;
pub mod codex;
pub mod commands;
pub mod config;
pub mod device;
pub mod logging;
pub mod updater;
pub mod model;
pub mod paths;
pub mod process;
pub mod prompt;
pub mod protocol;
pub mod proxy;
pub mod runtime;
pub mod session;
pub mod storage;
pub mod tray;
pub mod workspace;

use board::BoardRegistry;
use codex::CodexRegistry;
use std::sync::Arc;
use tauri::{Manager, RunEvent};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let _ = paths::ensure_data_layout();
    // Hold the appender guard for the whole process — dropping it stops the
    // file sink from flushing (see logging::init).
    let _log_guard = logging::init();
    tracing::info!(module = "lib", "Cameo starting");

    let boards: Arc<BoardRegistry> = Arc::new(BoardRegistry::default());
    let codex_reg: Arc<CodexRegistry> = Arc::new(CodexRegistry::default());
    let codex_for_exit = codex_reg.clone();

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .manage(boards)
        .manage(codex_reg)
        .setup(|app| {
            // Fire-and-forget background update check (60s delayed inside).
            // Errors are swallowed in the worker — updater never blocks boot.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                updater::check_update_on_startup(handle).await;
            });
            tray::setup(app)?;
            Ok(())
        })
        // Close → hide to tray (default) or quit, per `config.close_to_tray`.
        // Read fresh from disk each close so toggling Settings takes effect with
        // no restart. macOS Cmd+Q / dock-quit bypass this (they hit ExitRequested).
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if config::load().close_to_tray {
                    api.prevent_close();
                    let _ = window.hide();
                    tracing::info!(module = "tray", "close → hidden to tray");
                } else {
                    // Quit the whole app (macOS would otherwise keep it alive
                    // in the dock with no window).
                    api.prevent_close();
                    window.app_handle().exit(0);
                }
            }
        })
        .register_uri_scheme_protocol("cameo", protocol::handle_cameo_uri)
        .invoke_handler(tauri::generate_handler![
            ping,
            app_version,
            front_log,
            commands::cfg_load,
            commands::cfg_save,
            commands::probe_proxy,
            commands::probe_codex_network,
            device::device_id_get,
            device::device_id_reset,
            updater::check_and_download_update,
            updater::check_pending_update,
            updater::install_pending_update,
            commands::open_logs_dir,
            commands::read_clipboard_image,
            commands::detect_codex,
            commands::open_dir,
            commands::initial_board,
            commands::initial_test_prompt,
            commands::list_workspaces,
            commands::create_workspace,
            commands::rename_workspace,
            commands::remove_workspace,
            commands::open_board,
            commands::import_paths,
            commands::import_image_bytes,
            commands::read_asset_bytes,
            commands::update_placements,
            commands::delete_placements,
            commands::restore_placements,
            commands::replace_placement_image,
            commands::reveal_in_finder,
            commands::copy_image,
            commands::export_asset,
            commands::export_assets,
            commands::resolve_chat_image,
            commands::import_chat_image_to_canvas,
            commands::copy_image_from_path,
            commands::reveal_path_in_finder,
            commands::set_annotation,
            commands::rename_asset,
            commands::write_overlay,
            commands::start_session,
            commands::send_message,
            commands::interrupt_turn,
            commands::respond_permission,
            commands::codex_auth_status,
            commands::stop_session,
            commands::list_sessions,
            commands::new_session,
            commands::switch_session,
            commands::rename_session,
            commands::load_session,
            commands::append_message,
        ])
        .build(tauri::generate_context!())
        .expect("error building Cameo");

    app.run(move |_app_handle, event| {
        match event {
            // Synchronous tree-kill on exit — no block_on of async RPC
            // (deadlock-prone during shutdown). Only ExitRequested (Exit double-fires).
            RunEvent::ExitRequested { .. } => codex_for_exit.kill_all_sync(),
            // macOS: clicking the dock icon while hidden in the tray restores it.
            #[cfg(target_os = "macos")]
            RunEvent::Reopen { .. } => tray::show_main_window(_app_handle),
            _ => {}
        }
    });
}

/// Liveness probe — confirms the JS↔Rust IPC bridge is up.
#[tauri::command]
fn ping() -> String {
    "pong".to_string()
}

#[tauri::command]
fn app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// Surface frontend/webview logs into the Rust tracing stream (so canvas/runtime
/// issues are visible in logs even when devtools isn't open).
#[tauri::command]
fn front_log(level: String, msg: String) {
    match level.as_str() {
        "error" => tracing::error!(module = "front", "{msg}"),
        "warn" => tracing::warn!(module = "front", "{msg}"),
        _ => tracing::info!(module = "front", "{msg}"),
    }
}
