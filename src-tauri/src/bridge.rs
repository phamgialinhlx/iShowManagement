//! The desktop side of `rmux-app`: a [`Host`] over the `AppHandle`, and an
//! invoke handler that dispatches the shared [`Registry`] before falling back
//! to the desktop-only `generate_handler!` list.
//!
//! See `docs/rmux-web.md`. The point of the shape is that a command registered
//! in `rmux-app` is live here *and* in rmux-web with no edit in either shell.
//!
//! ## What must stay identical to `generate_handler!`
//!
//! - **The ACL is unchanged.** Tauri resolves access for the command *before*
//!   any invoke handler runs (`Webview::on_message`), so routing through this
//!   function instead of the macro neither adds nor removes a grant. App
//!   commands still need none; `plugin:*` commands never reach here.
//! - **Arguments are the same JSON object**, deserialised by the command's
//!   `camelCase` argument struct — the key convention the macro applied.
//! - **Responses are the same bytes.** `Ok(value)` is resolved as JSON, an
//!   `Err(String)` rejects with that string, exactly as an
//!   `async fn -> Result<T, String>` command did.
//! - **Channels resolve on the invoking webview**, the same call
//!   (`JavaScriptChannelId::channel_on`) Tauri's own `Channel` argument makes.
//! - **Nothing blocks the main thread**: every registry command runs on the
//!   async runtime via `respond_async`, as the macro does for `async` commands.

use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;

use rmux_app::{Call, Ctx, Host, Registry, SecretStore, Sink, SinkResolver};
use tauri::ipc::{Channel, Invoke, InvokeBody, InvokeError, InvokeResponseBody, JavaScriptChannelId};
use tauri::{AppHandle, Emitter, Manager, Runtime, Webview};

use crate::keychain::KeyringSecrets;

/// The process's one [`Ctx`], held in Tauri state.
///
/// It needs the `AppHandle`, so it cannot exist before the builder runs — and
/// Tauri 2 creates the configured windows *before* the `setup` hook, so `setup`
/// is not early enough to promise it is there for the first invoke. [`ctx`]
/// therefore creates it on demand, and `setup` merely calls it eagerly.
pub struct AppCtx(pub Arc<Ctx>);

/// [`Host`] over the desktop app.
pub struct DesktopHost<R: Runtime> {
    app: AppHandle<R>,
    secrets: KeyringSecrets,
}

impl<R: Runtime> DesktopHost<R> {
    pub fn new(app: AppHandle<R>) -> Self {
        Self { app, secrets: KeyringSecrets }
    }
}

impl<R: Runtime> Host for DesktopHost<R> {
    fn resource_dir(&self) -> Option<PathBuf> {
        self.app.path().resource_dir().ok()
    }

    fn data_dir(&self) -> anyhow::Result<PathBuf> {
        Ok(self.app.path().app_data_dir()?)
    }

    fn cache_dir(&self) -> anyhow::Result<PathBuf> {
        Ok(self.app.path().app_cache_dir()?)
    }

    fn emit(&self, event: &str, payload: serde_json::Value) {
        if let Err(e) = self.app.emit(event, payload) {
            tracing::warn!(error = %e, event, "could not emit an event to the UI");
        }
    }

    fn secrets(&self) -> Option<&dyn SecretStore> {
        Some(&self.secrets)
    }
}

/// The app's one [`Ctx`], created and put in Tauri state on first use.
///
/// Racing callers are safe: `manage` refuses a second value of a type, and the
/// loser reads back the winner's, so there is only ever one `Ctx` (and so one
/// of every store in it).
pub fn ctx<R: Runtime>(app: &AppHandle<R>) -> Arc<Ctx> {
    if let Some(state) = app.try_state::<AppCtx>() {
        return state.0.clone();
    }
    let ctx = Arc::new(Ctx::new(Arc::new(DesktopHost::new(app.clone()))));
    if app.manage(AppCtx(ctx.clone())) {
        ctx
    } else {
        app.state::<AppCtx>().0.clone()
    }
}

/// One of the shared stores (`TerminalStore`, `ClaudeStore`, …) for
/// desktop-only code — `server_disconnect`, redstone, tunnels, control.
///
/// **Always through the [`Ctx`], never `tauri::State`.** Those stores are what
/// the registry's commands use; a second, Tauri-managed copy would be a cache
/// nobody else reads, and a disconnect that evicts it would evict nothing.
pub fn store<T, R>(app: &AppHandle<R>) -> Arc<T>
where
    T: Default + Send + Sync + 'static,
    R: Runtime,
{
    ctx(app).store::<T>()
}

/// Channel ids resolved on the webview that invoked the command.
struct WebviewSinks<R: Runtime>(Webview<R>);

impl<R: Runtime> SinkResolver for WebviewSinks<R> {
    fn resolve(&self, channel_id: &str) -> anyhow::Result<Arc<dyn Sink>> {
        // Tauri 2 sends a `Channel` argument as the string `"__CHANNEL__:<n>"`.
        // The error text is the one Tauri's own `Channel` argument reports.
        let id = JavaScriptChannelId::from_str(channel_id).map_err(|_| {
            anyhow::anyhow!(
                "invalid channel value `{channel_id}`, expected a string in the `__CHANNEL__:ID` format"
            )
        })?;
        Ok(Arc::new(ChannelSink(id.channel_on(self.0.clone()))))
    }
}

/// A [`Sink`] that is a Tauri `Channel`.
struct ChannelSink(Channel<InvokeResponseBody>);

impl Sink for ChannelSink {
    fn send_bytes(&self, bytes: Vec<u8>) -> anyhow::Result<()> {
        // `Raw`, never JSON: reaches JS as an `ArrayBuffer`, the same body
        // `Channel<Response>` with `InvokeResponseBody::Raw` sent before.
        Ok(self.0.send(InvokeResponseBody::Raw(bytes))?)
    }

    fn send_json(&self, value: serde_json::Value) -> anyhow::Result<()> {
        // What `Channel<T: Serialize>::send` does: serialise to a JSON string.
        Ok(self.0.send(InvokeResponseBody::Json(serde_json::to_string(&value)?))?)
    }
}

/// The app's invoke handler: the shared [`Registry`] first, then `fallback`
/// (the desktop-only `generate_handler!` list).
///
/// A name in both is a bug — the fallback copy would never run — and
/// `tests/registry_bridge.rs` fails on one.
pub fn invoke_handler<R, F>(
    registry: Arc<Registry>,
    fallback: F,
) -> impl Fn(Invoke<R>) -> bool + Send + Sync + 'static
where
    R: Runtime,
    F: Fn(Invoke<R>) -> bool + Send + Sync + 'static,
{
    move |invoke: Invoke<R>| {
        let Some(command) = registry.get(invoke.message.command()) else {
            return fallback(invoke);
        };

        let args = match invoke.message.payload() {
            InvokeBody::Json(value) => value.clone(),
            // No registry command takes a raw body; one that ever does needs
            // its own path rather than a silent reinterpretation.
            InvokeBody::Raw(_) => {
                invoke
                    .resolver
                    .reject(format!("command {} does not accept a raw body", command.name()));
                return true;
            }
        };

        let webview = invoke.message.webview();
        let ctx = ctx(webview.app_handle());
        let call = Call::new(ctx, Arc::new(WebviewSinks(webview)));
        let task = command.run(call, args);
        invoke.resolver.respond_async(async move { task.await.map_err(InvokeError::from) });
        true
    }
}
