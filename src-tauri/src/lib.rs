//! The Tauri shell.
//!
//! This layer is deliberately thin: it owns window setup and the IPC surface, and
//! delegates everything else to the `rmux-*` crates. Keeping logic out of here is
//! what lets the transport, terminal and SSH code be tested without a GUI.

use rmux_transport::{CommandSpec, Platform, Target, TargetId};
use tauri::Manager;
use serde::Serialize;

mod askpass;
mod background;
mod control;
mod notify;
mod paste;
mod uplink;
mod userpath;
mod tunnels;
pub mod agent;
mod claude;
mod claude_account;
mod claude_login;
mod auth;
mod commands;
mod face_models;
mod glass;
mod lock;
mod logs;
mod model_profile;
mod settings_window;
mod theme;
pub mod files;
pub mod metrics;
pub mod terminal;

/// A target as the UI sees it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetInfo {
    pub id: TargetId,
    pub label: String,
    pub platform: Option<Platform>,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // **Logs go to a file as well as stdout.** A double-clicked `.app` has no
    // console, and on Windows the app is launched from Explorer with no console
    // at all — so before this, the only record of a failure on someone else's
    // machine was their description of it. Settings › Diagnostics exports the
    // file; see `logs`.
    //
    // The directory is resolved here rather than from the `AppHandle`, because
    // that does not exist until the builder runs and the most interesting lines
    // are the ones written before it does. The identifier matches
    // `tauri.conf.json`; a test pins them together.
    let log_dir = dirs::data_dir()
        .map(|d| d.join(logs::IDENTIFIER))
        .unwrap_or_else(std::env::temp_dir);

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "rmux=debug,warn".into());

    match logs::writer(&log_dir) {
        Some(file) => {
            use tracing_subscriber::layer::SubscriberExt as _;
            use tracing_subscriber::util::SubscriberInitExt as _;

            tracing_subscriber::registry()
                .with(filter)
                .with(tracing_subscriber::fmt::layer())
                // No ANSI in the file: colour codes make an exported log
                // unreadable in every text editor it will be opened in.
                .with(tracing_subscriber::fmt::layer().with_ansi(false).with_writer(file))
                .init();
        }
        // A log file is a convenience; failing to start over one would be absurd.
        None => tracing_subscriber::fmt().with_env_filter(filter).init(),
    }

    // **Before anything spawns a process.** A Finder-launched .app inherits
    // launchd's minimal PATH, so a `ProxyCommand` helper like `cloudflared` —
    // which lives in /opt/homebrew/bin — cannot be found, and the host fails with
    // a bare `exit status: 255`. See `userpath`.
    match userpath::adopt_login_path() {
        Some(path) => tracing::info!(%path, "adopted the login shell's PATH"),
        None => tracing::debug!("PATH left as inherited"),
    }

    tauri::Builder::default()
        // Gives a notification rmux's own icon and name. Driven from Rust (see
        // `notify`), so it needs no ACL entry.
        .plugin(tauri_plugin_notification::init())
        // Face model weights. See `face_models::SCHEME` for why this is a
        // protocol of our own rather than Tauri's asset: handler.
        .register_uri_scheme_protocol(face_models::SCHEME, |ctx, request| {
            let name = request.uri().path().trim_start_matches('/').to_owned();
            match face_models::serve(ctx.app_handle(), &name) {
                Some(bytes) => tauri::http::Response::builder()
                    .header("Content-Type", if name.ends_with(".json") {
                        "application/json"
                    } else {
                        "application/octet-stream"
                    })
                    // `tauri://localhost` fetching `models://localhost` is a
                    // *cross-origin* request, so without this the webview
                    // rejects the response before the caller sees it — and
                    // reports only "Load failed", with no mention of CORS.
                    .header("Access-Control-Allow-Origin", "*")
                    .body(bytes)
                    .unwrap_or_else(|_| empty_response()),
                None => empty_response(),
            }
        })
        .manage(auth::AuthStore::default())
        .manage(askpass::PromptStore::default())
        .manage(terminal::TerminalStore::default())
        .manage(files::FsStore::default())
        .manage(metrics::MetricsStore::default())
        .manage(claude::ClaudeStore::default())
        .manage(agent::AgentStore::default())
        .manage(claude_login::LoginStore::default())
        .manage(tunnels::TunnelStore::default())
        .manage(control::ControlState::default())
        .manage(notify::NotifyStore::default())
        .manage(uplink::UplinkStore::default())
        .manage(theme::ThemeStore::default())
        .invoke_handler(tauri::generate_handler![
            commands::local_target,
            commands::run_on_target,
            auth::auth_config,
            auth::sign_in,
            auth::open_external,
            auth::jira_start,
            auth::jira_poll,
            auth::resume_session,
            auth::sign_out,
            auth::jira_profiles,
            auth::jira_projects,
            auth::jira_missions,
            auth::jira_mission,
            auth::jira_transitions,
            auth::jira_transition,
            auth::jira_comment,
            auth::jira_create,
            settings_window::open_settings,
            lock::lock_status,
            lock::lock_enable,
            lock::lock_disable,
            lock::lock_unlock,
            lock::lock_unlock_face,
            lock::face_enroll,
            lock::face_status,
            face_models::face_models_status,
            face_models::face_models_install,
            face_models::face_model_file,
            terminal::terminal_open,
            terminal::terminal_attach,
            terminal::terminal_write,
            terminal::terminal_resize,
            terminal::terminal_close,
            askpass::answer_prompt,
            files::fs_list,
            files::fs_read,
            files::fs_write,
            files::fs_home,
            files::fs_preview,
            files::fs_create_file,
            files::fs_create_dir,
            files::fs_rename,
            files::fs_delete,
            files::fs_upload,
            files::fs_download,
            model_profile::model_profiles,
            model_profile::model_profile_parse,
            model_profile::model_profile_save,
            model_profile::model_profile_delete,
            metrics::metrics_sample,
            metrics::metrics_processes,
            metrics::metrics_kill,
            paste::claude_paste_image,
            notify::notify,
            notify::notify_reset,
            uplink::host_uplink,
            tunnels::ports_discover,
            tunnels::port_forward,
            tunnels::port_unforward,
            tunnels::ports_forwarded,
            tunnels::port_proxy,
            control::control_sync,
            control::control_open_url,
            control::control_info,
            claude::claude_start,
            claude::claude_list_sessions,
            claude::claude_list_all_sessions,
            claude::claude_transcript,
            claude::claude_end_session,
            claude_account::claude_account_status,
            claude_account::claude_account_save,
            claude_account::claude_account_forget,
            claude_account::claude_login_command,
            claude_account::claude_usage_report,
            claude_login::claude_login_start,
            claude_login::claude_login_submit,
            claude_login::claude_login_cancel,
            claude::claude_attach,
            claude::claude_state,
            claude::claude_answer,
            claude::claude_send,
            claude::claude_interrupt,
            claude::claude_resize,
            claude::claude_write,
            claude::claude_stop,
            files::fs_join,
            files::fs_parent,
            files::fs_search,
            files::ssh_config_hosts,
            background::background_set,
            background::background_clear,
            settings_window::restart_app,
            glass::glass_status,
            glass::set_glass,
            logs::log_status,
            logs::log_export,
            theme::theme_state,
            theme::theme_save,
            theme::theme_set_active,
            theme::theme_delete,
        ])
        .setup(|app| {
            // Bring the askpass bridge up before any connection can need it.
            // A failure here only costs password/2FA hosts, so it is logged
            // rather than aborting startup.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                match askpass::start(handle.clone()).await {
                    Ok(bridge) => {
                        handle.manage(bridge);
                    }
                    Err(e) => tracing::error!(
                        error = %e,
                        "askpass bridge unavailable — password and 2FA hosts will fail fast"
                    ),
                }
            });

            // The control socket, so other apps in the ecosystem can drive rmux.
            // Also best-effort: nothing the workbench does needs it, and a
            // machine where the socket cannot be created must still run.
            let handle = app.handle().clone();
            let forwards = app.state::<tunnels::TunnelStore>().forwards();
            tauri::async_runtime::spawn(async move {
                control::start(handle, forwards).await;
            });

            // Watch theme.toml so an external hand-edit repaints the running app.
            // Best-effort: without it, edits are picked up on the next launch.
            theme::start_watcher(app.handle().clone());
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while starting rmux")
        // Clients are told rmux is going away rather than left to discover it
        // by reconnect-looping into a socket that no longer answers.
        .run(|app, event| {
            if let tauri::RunEvent::ExitRequested { .. } = event {
                let handle = app.clone();
                tauri::async_runtime::block_on(control::shutdown(&handle));
            }
        });
}

/// A 404 for the model protocol.
fn empty_response() -> tauri::http::Response<Vec<u8>> {
    tauri::http::Response::builder().status(404).body(Vec::new()).expect("static response")
}

/// Re-exported so command handlers share one spec builder.
pub use rmux_transport::Tty;

pub(crate) fn spec(program: &str, args: &[String]) -> CommandSpec {
    CommandSpec::new(program).args(args.to_vec())
}

pub(crate) fn describe(target: &dyn Target) -> TargetInfo {
    TargetInfo {
        id: target.id().clone(),
        label: target.id().label(),
        platform: target.platform(),
    }
}
