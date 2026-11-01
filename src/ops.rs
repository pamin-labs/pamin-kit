//! Operation registration: each operation's schemas come from its contract
//! types, and its handler is erased to one shape the server can call.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use rmcp::handler::server::common::schema_for_type;
use schemars::JsonSchema;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

use crate::seams::{Feature, Space};

/// A JSON object, as tool schemas and arguments are.
pub type JsonObject = Map<String, Value>;

/// What an operation knows about its call besides its arguments.
#[derive(Clone)]
pub struct Ctx {
    space: Arc<Space>,
    cancel: CancellationToken,
    progress: Option<Arc<dyn Progress>>,
}

/// Where an operation's progress goes: a progress notification to the
/// client, or a task's status. Set by whoever runs the operation.
pub(crate) trait Progress: Send + Sync {
    fn report(
        &self,
        done: f64,
        total: Option<f64>,
        message: Option<String>,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
}

impl Ctx {
    pub(crate) fn new(
        space: Arc<Space>,
        cancel: CancellationToken,
        progress: Option<Arc<dyn Progress>>,
    ) -> Self {
        Self {
            space,
            cancel,
            progress,
        }
    }

    /// The tenant, end user and project the call works on, as the identity
    /// seam resolved them.
    pub fn space(&self) -> &Space {
        &self.space
    }

    /// Whether the caller cancelled the call. An operation checks this
    /// between steps and stops where its state is whole; the kit never
    /// stops it half way.
    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// Completes when the caller cancels the call, for an operation that
    /// waits on something it can abandon.
    pub async fn cancelled(&self) {
        self.cancel.cancelled().await
    }

    /// Tells the caller how far the operation is: `done` of `total`, if the
    /// total is known. A no-op for a caller that asked for no progress.
    pub async fn progress(&self, done: f64, total: Option<f64>, message: Option<&str>) {
        if let Some(progress) = &self.progress {
            progress
                .report(done, total, message.map(str::to_owned))
                .await;
        }
    }
}

type Reply = Pin<Box<dyn Future<Output = Result<Value, String>> + Send>>;
type Handler<M> = Box<dyn Fn(Arc<M>, Ctx, JsonObject) -> Reply + Send + Sync>;

/// An operation as callers see it.
#[derive(Clone, Debug)]
pub struct Tool {
    pub name: &'static str,
    pub feature: Feature,
    /// The arguments type's doc comment.
    pub description: Option<String>,
    pub input: Arc<JsonObject>,
    /// Absent when the result is not a JSON object, which MCP's structured
    /// output cannot describe.
    pub output: Option<Arc<JsonObject>>,
    /// Runs as a task for callers that support tasks.
    pub long: bool,
    /// The MCP App that shows this tool's results: a `ui://` resource.
    pub ui: Option<&'static str>,
    /// What a host may assume about the tool, when the module says.
    pub hints: Option<Hints>,
}

/// What a host may assume about a tool, sent as MCP tool annotations: hints
/// for the host and the user, never guarantees. The defaults are MCP's: a
/// tool may change things, destructively, not idempotently, and reach
/// beyond the module.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Hints {
    /// A name for people, where the tool's name is for models.
    pub title: Option<&'static str>,
    /// Changes nothing.
    pub read_only: bool,
    /// May delete or overwrite; meaningful only when not read-only.
    pub destructive: bool,
    /// Calling it again with the same arguments changes nothing more.
    pub idempotent: bool,
    /// Reaches beyond the module's own data, such as the web.
    pub open_world: bool,
}

impl Default for Hints {
    fn default() -> Self {
        Self {
            title: None,
            read_only: false,
            destructive: true,
            idempotent: false,
            open_world: true,
        }
    }
}

/// An HTML page a host renders as an MCP App, served as a resource.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct App {
    pub uri: &'static str,
    pub html: &'static str,
}

/// One registered operation.
pub(crate) struct Op<M> {
    pub tool: Tool,
    pub call: Handler<M>,
}

/// The operations a module registers.
pub struct Ops<M> {
    pub(crate) list: Vec<Op<M>>,
    pub(crate) apps: Vec<App>,
    pub(crate) instructions: Option<&'static str>,
}

impl<M: Send + Sync + 'static> Ops<M> {
    pub(crate) fn new() -> Self {
        Self {
            list: Vec::new(),
            apps: Vec::new(),
            instructions: None,
        }
    }

    /// Registers an operation served as the MCP tool `name`, callable by
    /// spaces entitled to `feature`.
    ///
    /// # Panics
    ///
    /// When `name` is registered twice or `A` is not a struct: both are
    /// mistakes in the module, found by its first test.
    pub fn tool<A, R, E, F, Fut>(
        &mut self,
        name: &'static str,
        feature: Feature,
        handler: F,
    ) -> &mut Self
    where
        A: DeserializeOwned + JsonSchema + 'static,
        R: Serialize + JsonSchema + 'static,
        E: std::fmt::Display,
        F: Fn(Arc<M>, Ctx, A) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<R, E>> + Send + 'static,
    {
        self.add(name, feature, false, handler)
    }

    /// Registers a long operation, over about a second, like [`Self::tool`].
    /// A caller that supports tasks gets a task at once and polls it for the
    /// result; any other caller waits for the result as for a tool.
    pub fn task<A, R, E, F, Fut>(
        &mut self,
        name: &'static str,
        feature: Feature,
        handler: F,
    ) -> &mut Self
    where
        A: DeserializeOwned + JsonSchema + 'static,
        R: Serialize + JsonSchema + 'static,
        E: std::fmt::Display,
        F: Fn(Arc<M>, Ctx, A) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<R, E>> + Send + 'static,
    {
        self.add(name, feature, true, handler)
    }

    /// Shows `tool`'s results as an MCP App in hosts that render them (Claude,
    /// ChatGPT, VS Code): the HTML page `html`, served as the resource
    /// `uri`. The page receives the tool's structured result from the host.
    /// Tools may share one page.
    ///
    /// # Panics
    ///
    /// When `tool` is not registered yet, `uri` is not a `ui://` URI, or
    /// `uri` already serves a different page.
    pub fn app(&mut self, tool: &str, uri: &'static str, html: &'static str) -> &mut Self {
        assert!(
            uri.starts_with("ui://"),
            "the app {uri} must be a ui:// URI"
        );
        self.registered(tool).ui = Some(uri);
        match self.apps.iter().find(|app| app.uri == uri) {
            Some(app) => assert_eq!(app.html, html, "{uri} already serves another page"),
            None => self.apps.push(App { uri, html }),
        }
        self
    }

    /// Tells hosts what they may assume about `tool`.
    ///
    /// # Panics
    ///
    /// When `tool` is not registered yet.
    pub fn hints(&mut self, tool: &str, hints: Hints) -> &mut Self {
        self.registered(tool).hints = Some(hints);
        self
    }

    /// Guidance for a model on using the module's tools, sent when a client
    /// connects, for hosts that do not load the module's `SKILL.md`.
    pub fn instructions(&mut self, text: &'static str) -> &mut Self {
        self.instructions = Some(text);
        self
    }

    /// The registered tool `name`, to describe further.
    fn registered(&mut self, name: &str) -> &mut Tool {
        self.list
            .iter_mut()
            .map(|op| &mut op.tool)
            .find(|tool| tool.name == name)
            .unwrap_or_else(|| panic!("register {name} before describing it"))
    }

    fn add<A, R, E, F, Fut>(
        &mut self,
        name: &'static str,
        feature: Feature,
        long: bool,
        handler: F,
    ) -> &mut Self
    where
        A: DeserializeOwned + JsonSchema + 'static,
        R: Serialize + JsonSchema + 'static,
        E: std::fmt::Display,
        F: Fn(Arc<M>, Ctx, A) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<R, E>> + Send + 'static,
    {
        assert!(
            self.list.iter().all(|op| op.tool.name != name),
            "operation {name} is registered twice"
        );
        // rmcp generates JSON Schema 2020-12, as MCP requires.
        let mut input = JsonObject::clone(&schema_for_type::<A>());
        assert!(
            is_object(&input),
            "the arguments of {name} must be a struct"
        );
        let description = take_text(&mut input);
        let output = Some(JsonObject::clone(&schema_for_type::<R>()))
            .filter(is_object)
            .map(|mut output| {
                take_text(&mut output);
                Arc::new(output)
            });
        let handler = Arc::new(handler);
        let call: Handler<M> = Box::new(move |module, ctx, arguments| {
            let handler = handler.clone();
            Box::pin(async move {
                let arguments = serde_json::from_value(Value::Object(arguments))
                    .map_err(|error| format!("invalid arguments: {error}"))?;
                let result = handler(module, ctx, arguments)
                    .await
                    .map_err(|error| error.to_string())?;
                serde_json::to_value(result).map_err(|error| error.to_string())
            })
        });
        self.list.push(Op {
            tool: Tool {
                name,
                feature,
                description,
                input: Arc::new(input),
                output,
                long,
                ui: None,
                hints: None,
            },
            call,
        });
        self
    }
}

fn is_object(schema: &JsonObject) -> bool {
    schema.get("type").and_then(Value::as_str) == Some("object")
}

/// Removes the type's name and doc comment from a root schema, returning the
/// doc comment. The name is noise to a model; the doc comment becomes the
/// tool's description instead of being said twice.
fn take_text(schema: &mut JsonObject) -> Option<String> {
    schema.remove("title");
    match schema.remove("description") {
        Some(Value::String(text)) => Some(text),
        _ => None,
    }
}
