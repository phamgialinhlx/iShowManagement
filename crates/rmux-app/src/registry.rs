//! Command name → handler.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::ctx::Ctx;
use crate::host::{Sink, SinkResolver};

/// A handler's result, already serialised. Errors are strings because that is
/// what crosses the IPC boundary today (`anyhow::Error` is not serialisable);
/// each command converts at its edge with the same text it always produced.
pub type BoxFuture = Pin<Box<dyn Future<Output = Result<Value, String>> + Send + 'static>>;

type Erased = Arc<dyn Fn(Call, Value) -> BoxFuture + Send + Sync>;

/// Whether a command is served by rmux-web as well as the desktop.
///
/// Every command is classified, explicitly, at the point it is registered —
/// a command cannot drift into the web, or out of it, by omission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Web {
    Yes,
    No,
}

/// What a handler receives besides its arguments.
///
/// Cheap to clone (two `Arc`s), and owned, so a handler's `async move` block
/// can hold it across awaits and hand it to a task that outlives the call.
#[derive(Clone)]
pub struct Call {
    ctx: Arc<Ctx>,
    sinks: Arc<dyn SinkResolver>,
}

impl Call {
    pub fn new(ctx: Arc<Ctx>, sinks: Arc<dyn SinkResolver>) -> Self {
        Self { ctx, sinks }
    }

    pub fn ctx(&self) -> &Ctx {
        &self.ctx
    }

    pub fn ctx_arc(&self) -> Arc<Ctx> {
        self.ctx.clone()
    }

    /// The stream the UI named by `channel_id` (an argument it sent).
    ///
    /// On desktop the id is Tauri's `"__CHANNEL__:<n>"` string, resolved on the
    /// invoking webview; on the web it names a stream on the page's WebSocket.
    pub fn sink(&self, channel_id: &str) -> anyhow::Result<Arc<dyn Sink>> {
        self.sinks.resolve(channel_id)
    }
}

/// One registered command.
pub struct Command {
    name: &'static str,
    web: Web,
    handler: Erased,
}

impl Command {
    pub fn name(&self) -> &'static str {
        self.name
    }

    pub fn web(&self) -> Web {
        self.web
    }

    /// Run the handler. Arguments are deserialised inside the returned future,
    /// so a malformed payload is an `Err` from the call, never a panic.
    pub fn run(&self, call: Call, args: Value) -> BoxFuture {
        (self.handler)(call, args)
    }
}

/// Arguments of a command that takes none. Accepts `{}` (what Tauri sends for
/// `invoke("cmd")`) and `null`.
#[derive(Debug, Default, serde::Deserialize)]
pub struct NoArgs {}

/// The command table.
#[derive(Default)]
pub struct Registry {
    commands: BTreeMap<&'static str, Command>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `name`.
    ///
    /// `A` is the command's argument struct, deserialised from the JSON object
    /// the UI sent. Declare it `#[derive(Deserialize)] #[serde(rename_all =
    /// "camelCase")]` — **Tauri renamed each snake_case parameter to camelCase**,
    /// so the keys the UI sends today are camelCase and must stay exactly so.
    /// An `Option` field may be absent, as it could be for a Tauri parameter.
    ///
    /// ```ignore
    /// r.add("git_status", Web::Yes, |_call, a: GitStatusArgs| async move {
    ///     git_status(a.target, a.root).await
    /// });
    /// ```
    ///
    /// # Panics
    ///
    /// When `name` is already registered. Two handlers for one name means one of
    /// them silently never runs; `registry()` is built by the tests, so this is
    /// caught there rather than in a shipped app.
    pub fn add<A, F, Fut, R>(&mut self, name: &'static str, web: Web, handler: F)
    where
        A: DeserializeOwned + Send + 'static,
        F: Fn(Call, A) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<R, String>> + Send + 'static,
        R: Serialize,
    {
        let handler = Arc::new(handler);
        let erased: Erased = Arc::new(move |call: Call, args: Value| {
            let handler = handler.clone();
            Box::pin(async move {
                let args = parse_args::<A>(name, args)?;
                let value = handler(call, args).await?;
                serde_json::to_value(value).map_err(|e| e.to_string())
            })
        });
        let previous = self.commands.insert(name, Command { name, web, handler: erased });
        assert!(previous.is_none(), "command `{name}` registered twice");
    }

    pub fn get(&self, name: &str) -> Option<&Command> {
        self.commands.get(name)
    }

    pub fn contains(&self, name: &str) -> bool {
        self.commands.contains_key(name)
    }

    /// Every command, in name order.
    pub fn commands(&self) -> impl Iterator<Item = &Command> {
        self.commands.values()
    }

    /// Run `name`. An unknown name is answered with the same text Tauri uses,
    /// so a caller cannot tell which layer refused it.
    pub async fn invoke(&self, name: &str, call: Call, args: Value) -> Result<Value, String> {
        match self.get(name) {
            Some(command) => command.run(call, args).await,
            None => Err(format!("Command {name} not found")),
        }
    }
}

fn parse_args<A: DeserializeOwned>(name: &str, args: Value) -> Result<A, String> {
    // `invoke("cmd")` with no argument object arrives as `{}` from Tauri and
    // may arrive as `null` from other transports; both mean "no arguments".
    let args = if args.is_null() { Value::Object(Default::default()) } else { args };
    serde_json::from_value(args).map_err(|e| format!("invalid args for command `{name}`: {e}"))
}
