//! The Tauri backend (master spec s11).
//!
//! Thin by design: every command translates a UI request into one `ws-core`
//! call. No business logic lives here, so the rules that matter stay in the
//! crates that have tests.
//!
//! Streaming uses a Tauri IPC channel per turn rather than global events, so
//! two conversations streaming at once cannot interleave into one another.

mod awake;
mod blueprint;
mod bridge;
mod commands;
mod graph;
mod graph_builder;
mod run;
mod sandbox;
mod state;
mod tray;

pub use state::AppState;

// The agent's file and command tools live in `ws-tools`, shared with the CLI.
use ws_tools::{files, workspace};

// `manage` comes from the Manager trait.
use tauri::Manager as _;

/// Build and run the desktop app.
///
/// # Panics
/// Panics if the app data directory is unavailable or the database cannot be
/// opened, both of which make the app unusable rather than degraded.
#[allow(
    clippy::expect_used,
    reason = "startup failure is fatal and documented above; there is no UI yet to report it in"
)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "ws_app=info,ws_core=info,ws_providers=info".into()),
        )
        .init();

    tauri::Builder::default()
        // First, so a second launch hands over to this one and exits before
        // it opens the database.
        .plugin(tauri_plugin_single_instance::init(|app, argv, cwd| {
            // Also brings it back from the tray.
            tray::show_main(app);
            blueprint::open_from_args(app, &argv, &cwd);
        }))
        .plugin(tauri_plugin_dialog::init())
        // Blueprint windows belong to the main one, as Simulink's do to MATLAB:
        // closing it closes Workspace. It asks first about unsaved blueprints.
        .on_window_event(|window, event| {
            if window.label() == "main" && matches!(event, tauri::WindowEvent::Destroyed) {
                tray::quit_now(window.app_handle());
            }
        })
        .setup(|app| {
            let state = AppState::initialise(app)?;
            app.manage(state);
            let cwd = std::env::current_dir().unwrap_or_default();
            let args: Vec<String> = std::env::args().collect();
            blueprint::open_from_args(app.handle(), &args, &cwd.to_string_lossy());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_conversations,
            commands::create_conversation,
            commands::load_thread,
            commands::send_message,
            commands::hide_if_working,
            commands::regenerate,
            commands::cancel_turn,
            commands::switch_model,
            commands::list_models,
            commands::worker_fallbacks,
            commands::set_worker_fallbacks,
            commands::list_backends,
            commands::save_compat_provider,
            commands::list_compat_providers,
            commands::delete_compat_provider,
            commands::set_api_key,
            commands::has_api_key,
            commands::delete_api_key,
            commands::get_context_view,
            commands::siblings_of,
            commands::select_branch,
            commands::delete_conversation,
            commands::rename_conversation,
            commands::set_model_profile,
            commands::get_model_profile,
            commands::inspect_local_model,
            commands::load_local_model,
            commands::unload_local_model,
            commands::local_model_status,
            commands::resolve_approval,
            commands::mcp_status,
            commands::list_skills,
            commands::open_graph,
            commands::build_graph,
            commands::has_graph,
            commands::list_dir,
            commands::ask_model,
            commands::lsp_start,
            commands::dap_start,
            commands::bridge_send,
            commands::bridge_stop,
            commands::search_project,
            commands::project_files,
            commands::project_symbols,
            commands::git_status,
            commands::git_diff,
            commands::git_hunks,
            commands::app_data_folder,
            commands::git_stage_hunk,
            commands::git_stage,
            commands::git_unstage,
            commands::git_commit,
            commands::worker_changes,
            commands::worker_diff,
            commands::worker_merge,
            commands::worker_discard,
            commands::git_blame,
            commands::read_text_file,
            commands::write_text_file,
            commands::create_file,
            commands::create_dir,
            commands::rename_path,
            commands::delete_path,
            commands::terminal_open,
            commands::terminal_input,
            commands::terminal_resize,
            commands::terminal_close,
            commands::pip_install,
            commands::set_workspace,
            commands::workspace_folder,
            commands::run_file,
            commands::run_input,
            commands::run_resize,
            commands::stop_run,
            commands::undo_edit,
            blueprint::read_parts_library,
            blueprint::write_parts_library,
            blueprint::open_blueprint_window,
            blueprint::blueprint_saved_as,
            blueprint::blueprint_dirty,
            blueprint::unsaved_blueprints,
            sandbox::sandbox_run,
            sandbox::sandbox_stop,
        ])
        .run(tauri::generate_context!())
        .expect("the Tauri application failed to start");
}
