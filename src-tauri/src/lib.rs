//! The Tauri shell.
//!
//! This layer is deliberately thin: it owns window setup and the IPC surface, and
//! delegates everything else to the `rmux-*` crates. Keeping logic out of here is
//! what lets the transport, terminal and SSH code be tested without a GUI.

use tauri::Manager;

mod askpass;
mod clipboard;
mod background;
mod cli_install;
mod control;
mod notify;
mod redstone;
mod uplink;
mod userpath;
mod tunnels;
mod dsh;
pub mod bridge;
mod claude_login;
mod auth;
mod face_models;
mod glass;
mod identity_migration;
mod keychain;
mod lock;
pub mod logs;
mod new_window;
mod settings_window;
mod theme;
pub mod server;

/// A target as the UI sees it. Lives in `rmux-app` with the commands that
/// return it.
pub use rmux_app::TargetInfo;

/// The desktop's invoke handler: `rmux_app::registry()` first, then the
/// desktop-only commands below.
///
/// **A command that moved into `rmux-app` must leave this list in the same
/// change** — otherwise the registry answers it and this copy is dead code that
/// looks live. `tests/registry_bridge.rs` reads this source and fails on any
/// name in both.
fn invoke_handler() -> impl Fn(tauri::ipc::Invoke) -> bool + Send + Sync + 'static {
    bridge::invoke_handler(
        std::sync::Arc::new(rmux_app::registry()),
        tauri::generate_handler![
            clipboard::clipboard_write,
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
            new_window::open_window,
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
            askpass::answer_prompt,
            notify::notify,
            notify::notify_reset,
            uplink::host_uplink,
            tunnels::ports_discover,
            tunnels::port_forward,
            tunnels::port_unforward,
            tunnels::ports_forwarded,
            tunnels::port_proxy,
            dsh::dsh_view_open,
            dsh::dsh_view_bounds,
            dsh::dsh_view_close,
            dsh::dsh_open_browser,
            dsh::dsh_probe,
            control::control_sync,
            control::control_open_url,
            control::control_info,
            redstone::redstone_session,
            redstone::redstone_sign_out,
            redstone::redstone_capabilities,
            redstone::redstone_sign_in,
            redstone::redstone_sign_in_start,
            redstone::redstone_sign_in_poll,
            redstone::redstone_enrol,
            redstone::redstone_enrol_with_token,
            redstone::redstone_unenrol,
            redstone::redstone_host_status,
            redstone::redstone_start_bridge,
            claude_login::claude_login_start,
            claude_login::claude_login_submit,
            claude_login::claude_login_submit_profile,
            claude_login::claude_login_cancel,
            server::server_disconnect,
            server::server_alias,
            background::background_set,
            cli_install::servers_sync,
            cli_install::cli_status,
            cli_install::cli_install,
            cli_install::cli_uninstall,
            background::background_clear,
            settings_window::restart_app,
            glass::glass_status,
            glass::set_glass,
            logs::log_status,
            logs::log_export,
            logs::set_debug_logging,
            logs::log_event,
            theme::theme_state,
            theme::theme_save,
            theme::theme_set_active,
            theme::theme_set_auto,
            theme::theme_delete,
        ],
    )
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

    // After the subscriber, or the panic would be logged to nothing — which is
    // the exact failure this exists to fix.
    logs::route_panics_to_log();

    // **Before anything spawns a process.** A Finder-launched .app inherits
    // launchd's minimal PATH, so a `ProxyCommand` helper like `cloudflared` —
    // which lives in /opt/homebrew/bin — cannot be found, and the host fails with
    // a bare `exit status: 255`. See `userpath`.
    match userpath::adopt_login_path() {
        Some(path) => tracing::info!(%path, "adopted the login shell's PATH"),
        None => tracing::debug!("PATH left as inherited"),
    }

    // **Before the first window, because WebKit opens localStorage with it.**
    // A bundle identifier keys the webview's data store, so a rename makes the
    // app look brand new: the session list, notes and every preference live in
    // `~/Library/WebKit/<id>/`. Copying into a store that is already open gives
    // a database that is present and missing its last writes, so this runs here
    // and nowhere later.
    match identity_migration::run(logs::IDENTIFIER) {
        identity_migration::Outcome::Migrated { from } => {
            tracing::info!(%from, "carried the previous install's data forward")
        }
        outcome => tracing::debug!(?outcome, "no app-data migration needed"),
    }

    tauri::Builder::default()
        // Gives a notification rmux's own icon and name. Driven from Rust (see
        // `notify`), so it needs no ACL entry.
        .plugin(tauri_plugin_notification::init())
        // The native file picker, for Connect a server › KEY FILE: a key is
        // chosen by *path* (it is offered to ssh with `-i`), and `<input
        // type=file>` hands the webview bytes, never a path. A plugin command,
        // so it is ACL-checked — `dialog:allow-open` in capabilities/default.json,
        // guarded by `tests/key_file_dialog.rs`; without the grant the picker
        // promise rejects and the button silently does nothing.
        .plugin(tauri_plugin_dialog::init())
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
        // The stores of the commands that live in `rmux-app` (terminal, files,
        // metrics, claude, claude status, agent) are *not* managed here: they
        // are held in the one `Ctx` (`bridge::ctx`), and desktop-only code that
        // needs one reaches it through that, so each exists exactly once.
        .manage(auth::AuthStore::default())
        .manage(askpass::PromptStore::default())
        .manage(claude_login::LoginStore::default())
        .manage(tunnels::TunnelStore::default())
        .manage(control::ControlState::default())
        .manage(notify::NotifyStore::default())
        .manage(uplink::UplinkStore::default())
        .manage(theme::ThemeStore::default())
        .invoke_handler(invoke_handler())
        .setup(|app| {
            // The shared command context. Created on demand by the first
            // invoke too (windows exist before this hook runs); built here so
            // it is not created lazily mid-command in the common case.
            bridge::ctx(app.handle());

            // **Before anything opens a connection.** A previous run that died
            // without cleaning up leaves `ssh -N` masters behind; measured on a
            // live machine, six of twelve had been reparented to init and were
            // still holding a connection half an hour later, most of them with
            // no socket anyone could ever multiplex over. A master that still
            // *answers* is deliberately left alone — that is the one
            // `check_alive` piggybacks on instead of authenticating again.
            //
            // The ordering matters: a master of our own that is mid-start has
            // not created its socket yet and would look exactly like an orphan.
            // Blocking, not spawned: a spawn races the first IPC command out
            // of the webview, and a master of ours that is mid-start has no
            // socket yet — so the reaper would see it as an orphan and kill a
            // connection being opened right then. That failure is rare, random
            // and arrives as "this host would not connect at launch", which is
            // the worst shape a bug can have. It costs one `ps` before the
            // window appears.
            tauri::async_runtime::block_on(rmux_ssh::ControlMaster::reap_orphans());

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

/// Re-exported for command handlers; the shared spec builder itself moved to
/// `rmux_app::commands::spec`.
pub use rmux_transport::Tty;
