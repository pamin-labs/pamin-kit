//! Operation registration: each operation's schemas come from its contract
//! types, and its handler is erased to one shape the server can call.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::BTreeMap;

use rmcp::handler::server::common::schema_for_type;
use rmcp::model::{ElicitResult, ElicitationAction};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

use crate::seams::{Feature, Space};

/// A JSON object, as tool schemas and arguments are.
pub type JsonObject = Map<String, Value>;

/// What an operation knows about its call besides its arguments.
#[derive(Clone)]
pub struct Ctx {
    space: Arc<Space>,
    /// Absent when nothing can cancel the call, so a plain call allocates
    /// no token.
    cancel: Option<CancellationToken>,
    progress: Option<Arc<dyn Progress>>,
}

/// One question an operation asks the user, as an MCP elicitation: a
/// message and the form of the answer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Ask {
    /// Identifies the answer when the client sends it back.
    pub key: String,
    pub message: String,
    pub form: Form,
}

/// How the user answers a question.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Form {
    /// In the client, as fields: the answer's JSON Schema, an object of
    /// primitive fields.
    Fields(JsonObject),
    /// On a web page the client opens, for what must not pass through the
    /// client: signing in elsewhere, payment, secrets.
    Url(String),
}

/// What [`Ctx::ask`] returns while the user has not answered yet: return it
/// (with `?`) from the operation, and the kit asks the client, which calls
/// the operation again with the answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NeedsInput;

impl std::fmt::Display for NeedsInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the operation needs an answer from the user")
    }
}

impl std::error::Error for NeedsInput {}

impl From<NeedsInput> for String {
    fn from(needs: NeedsInput) -> Self {
        needs.to_string()
    }
}

/// The answers a call brought and what it did besides its result, for the
/// one call running in this tokio task: kept task-local so that a call that
/// asks and changes nothing allocates nothing for it.
pub(crate) struct Input {
    answers: Option<BTreeMap<String, Value>>,
    effects: Effects,
}

/// What a call did besides returning.
#[derive(Default)]
pub(crate) struct Effects {
    /// The questions it asked the user.
    pub asked: Vec<Ask>,
    /// The resources it changed, by URI.
    pub updated: Vec<String>,
    /// Content it returns beside its result.
    pub attached: Vec<Content>,
}

tokio::task_local! {
    static INPUT: RefCell<Input>;
}

/// Runs `work` with `answers` available to [`Ctx::ask`], and returns what
/// it asked and changed besides its output.
pub(crate) async fn with_input<F: Future>(
    answers: Option<BTreeMap<String, Value>>,
    work: F,
) -> (F::Output, Effects) {
    let input = RefCell::new(Input {
        answers,
        effects: Effects::default(),
    });
    INPUT
        .scope(input, async {
            let output = work.await;
            let effects = INPUT.with(|input| std::mem::take(&mut input.borrow_mut().effects));
            (output, effects)
        })
        .await
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
        cancel: Option<CancellationToken>,
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

    /// The space shared, for the kit to keep past the call without copying.
    pub(crate) fn shared_space(&self) -> Arc<Space> {
        self.space.clone()
    }

    /// Whether the caller cancelled the call. An operation checks this
    /// between steps and stops where its state is whole; the kit never
    /// stops it half way.
    pub fn is_cancelled(&self) -> bool {
        self.cancel
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
    }

    /// Completes when the caller cancels the call, for an operation that
    /// waits on something it can abandon.
    pub async fn cancelled(&self) {
        match &self.cancel {
            Some(cancel) => cancel.cancelled().await,
            None => std::future::pending().await,
        }
    }

    /// Asks the user a question, identified by `key`, whose answer has the
    /// form of `T` (a struct of primitive fields, whose doc comments label
    /// the form). Returns the answer once the user gave it, `None` when they
    /// declined, and [`NeedsInput`] the first time: return that from the
    /// operation, which runs again, from the start, with the answer. Ask
    /// from the operation's own task, not from one it spawns.
    pub fn ask<T: DeserializeOwned + JsonSchema + 'static>(
        &self,
        key: &str,
        message: &str,
    ) -> Result<Option<T>, NeedsInput> {
        let fields = || Form::Fields(JsonObject::clone(&schema_for_type::<T>()));
        match self.answer(key, message, fields)? {
            Some(content) => match serde_json::from_value(content) {
                Ok(answer) => Ok(Some(answer)),
                // Answered in a form that does not parse: ask again.
                Err(_) => {
                    self.record(key, message, fields());
                    Err(NeedsInput)
                }
            },
            None => Ok(None),
        }
    }

    /// Asks the user to open `url` and do something there that must not
    /// pass through the client, such as signing in elsewhere or paying, as
    /// MCP's URL elicitation has it. Returns whether they agreed to go;
    /// what they did there reaches the module out of band, so the operation
    /// checks it when it runs again and asks again if it is not done.
    pub fn ask_url(&self, key: &str, message: &str, url: &str) -> Result<bool, NeedsInput> {
        let answered = self.answer(key, message, || Form::Url(url.to_owned()))?;
        Ok(answered.is_some())
    }

    /// The content of the user's answer to `key` when they accepted, `None`
    /// when they declined, or [`NeedsInput`] after recording the question
    /// when they have not answered: what both kinds of question share.
    fn answer(
        &self,
        key: &str,
        message: &str,
        form: impl FnOnce() -> Form,
    ) -> Result<Option<Value>, NeedsInput> {
        let given = INPUT
            .try_with(|input| {
                let input = input.borrow();
                let given = input.answers.as_ref()?.get(key)?;
                serde_json::from_value::<ElicitResult>(given.clone()).ok()
            })
            // Outside a kit call there is nobody to ask.
            .map_err(|_| NeedsInput)?;
        match given {
            Some(result) => Ok(match result.action {
                ElicitationAction::Accept => Some(result.content.unwrap_or(Value::Null)),
                _ => None,
            }),
            None => {
                self.record(key, message, form());
                Err(NeedsInput)
            }
        }
    }

    /// Records a question for the kit to put to the user.
    fn record(&self, key: &str, message: &str, form: Form) {
        let _ = INPUT.try_with(|input| {
            input.borrow_mut().effects.asked.push(Ask {
                key: key.to_owned(),
                message: message.to_owned(),
                form,
            });
        });
    }

    /// Returns `content` beside a tool's structured result: an image, audio,
    /// text, or a resource by link or carried along, in the order attached.
    /// Resource reads and prompts return their own content instead.
    pub fn attach(&self, content: Content) {
        // Outside a kit call there is nobody to return it to.
        let _ = INPUT.try_with(|input| input.borrow_mut().effects.attached.push(content));
    }

    /// Says the resource at `uri` changed, so clients listening for it in
    /// this space are told once the call ends.
    pub fn updated(&self, uri: impl Into<String>) {
        // Outside a kit call there is nobody to tell.
        let _ = INPUT.try_with(|input| input.borrow_mut().effects.updated.push(uri.into()));
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

/// A registered handler with its types erased: it takes its arguments as
/// JSON (an object, or null when it takes none) and returns `T` or an error
/// a model reads.
pub(crate) type Handler<M, T> = Box<
    dyn Fn(Arc<M>, Ctx, Value) -> Pin<Box<dyn Future<Output = Result<T, String>> + Send>>
        + Send
        + Sync,
>;

/// Erases `handler`'s types: parses its arguments as `A` and turns its
/// result into `T` with `finish`. Shared by tools, resources and prompts.
fn erase<M, A, R, E, F, Fut, T>(handler: F, finish: fn(R) -> Result<T, String>) -> Handler<M, T>
where
    M: Send + Sync + 'static,
    A: DeserializeOwned,
    R: 'static,
    E: std::fmt::Display,
    F: Fn(Arc<M>, Ctx, A) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<R, E>> + Send + 'static,
    T: 'static,
{
    Box::new(move |module, ctx, arguments| {
        // Parsed and started here, so the future owns only what it awaits
        // and the handler is not shared with it: no reference count to bump
        // on every call, which concurrent calls would contend on.
        let started = serde_json::from_value(arguments)
            .map(|arguments| handler(module, ctx, arguments))
            .map_err(|error| format!("invalid arguments: {error}"));
        Box::pin(async move {
            let result = started?.await.map_err(|error| error.to_string())?;
            finish(result)
        })
    })
}

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
    /// Called by an MCP App only, such as an undo button, and kept from the
    /// model.
    pub app_only: bool,
    /// What a host may assume about the tool, when the module says.
    pub hints: Option<Hints>,
    pub looks: Looks,
}

/// How a tool, prompt, resource or the server shows to people: none of it
/// changes what it does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct Looks {
    /// A name for people, where the registered name is for models.
    pub title: Option<&'static str>,
    pub icons: &'static [Icon],
    /// A resource's MIME type, shown before it is read.
    pub mime: Option<&'static str>,
    /// Who a resource is for: the user, the model, or both.
    pub audience: &'static [Role],
    /// How much a resource matters, from 0 (least) to 1 (most).
    pub priority: Option<f32>,
    /// What the server is, for people: the server only, since tools,
    /// prompts and resources take theirs from doc comments.
    pub about: Option<&'static str>,
    /// The server's home page: the server only.
    pub website: Option<&'static str>,
}

/// An icon, by URL or `data:` URI.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Icon {
    pub src: &'static str,
    pub mime: Option<&'static str>,
    /// Such as `48x48`, or `any` for scalable icons.
    pub sizes: &'static [&'static str],
}

/// What a host may assume about a tool, sent as MCP tool annotations: hints
/// for the host and the user, never guarantees. The defaults are MCP's: a
/// tool may change things, destructively, not idempotently, and reach
/// beyond the module.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Hints {
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
            read_only: false,
            destructive: true,
            idempotent: false,
            open_world: true,
        }
    }
}

/// A resource a module serves: data a host or model reads by URI. With
/// `{name}` variables in its URI it is a template, read at any URI that
/// matches.
#[derive(Clone, Debug)]
pub struct Resource {
    pub uri: &'static str,
    pub name: &'static str,
    pub feature: Feature,
    /// The variables type's doc comment, for a template.
    pub description: Option<String>,
    pub looks: Looks,
}

impl Resource {
    pub fn is_template(&self) -> bool {
        self.uri.contains('{')
    }
}

/// What reading a resource returns.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Body {
    Text {
        mime: Cow<'static, str>,
        text: String,
    },
    Bytes {
        mime: Cow<'static, str>,
        #[serde(with = "base64_bytes")]
        bytes: Vec<u8>,
    },
}

impl Body {
    pub fn text(mime: impl Into<Cow<'static, str>>, text: impl Into<String>) -> Self {
        Self::Text {
            mime: mime.into(),
            text: text.into(),
        }
    }

    pub fn bytes(mime: impl Into<Cow<'static, str>>, bytes: impl Into<Vec<u8>>) -> Self {
        Self::Bytes {
            mime: mime.into(),
            bytes: bytes.into(),
        }
    }

    /// `value` as JSON, the usual body of a module's resource.
    pub fn json<T: Serialize>(value: &T) -> Result<Self, String> {
        let text = serde_json::to_string(value).map_err(|error| error.to_string())?;
        Ok(Self::text("application/json", text))
    }
}

/// Content beside a tool's structured result, or a prompt message: what
/// MCP calls content blocks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Content {
    Text(String),
    Image {
        mime: Cow<'static, str>,
        #[serde(with = "base64_bytes")]
        data: Vec<u8>,
    },
    Audio {
        mime: Cow<'static, str>,
        #[serde(with = "base64_bytes")]
        data: Vec<u8>,
    },
    /// A resource the client may read, by link.
    Link {
        uri: String,
        name: String,
        mime: Option<Cow<'static, str>>,
    },
    /// A resource's body, carried along.
    Resource {
        uri: String,
        body: Body,
    },
}

/// Bytes as base64 in stored JSON, where a list of numbers would be four
/// times larger.
mod base64_bytes {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(deserializer)?;
        STANDARD.decode(text).map_err(serde::de::Error::custom)
    }
}

/// One registered resource.
pub(crate) struct Readable<M> {
    pub resource: Resource,
    pub read: Handler<M, Body>,
}

/// A prompt a module offers: messages a user picks to start a conversation
/// with, filled from arguments.
#[derive(Clone, Debug)]
pub struct Prompt {
    pub name: &'static str,
    pub feature: Feature,
    /// The arguments type's doc comment.
    pub description: Option<String>,
    pub arguments: Vec<PromptArgument>,
    /// The arguments' JSON Schema, for conformance: MCP prompt arguments
    /// are strings.
    pub schema: Arc<JsonObject>,
    pub looks: Looks,
}

/// One argument of a prompt, from a field of its arguments type.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PromptArgument {
    pub name: String,
    pub description: Option<String>,
    pub required: bool,
}

/// Who says a prompt message, or who a resource is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
}

/// One message of a prompt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub role: Role,
    pub content: Content,
}

impl Message {
    /// A message of text alone, the usual kind.
    pub fn text(role: Role, text: impl Into<String>) -> Self {
        Self {
            role,
            content: Content::Text(text.into()),
        }
    }
}

/// One registered prompt.
pub(crate) struct Promptable<M> {
    pub prompt: Prompt,
    pub get: Handler<M, Vec<Message>>,
}

/// Completes one argument of a prompt or variable of a resource template.
pub(crate) struct Completer<M> {
    /// A prompt's name or a resource template's URI.
    pub target: &'static str,
    pub argument: &'static str,
    pub complete: Handler<M, Vec<String>>,
}

/// The MIME type MCP Apps hosts render.
pub(crate) const APP_MIME: &str = "text/html;profile=mcp-app";

/// An HTML page a host renders as an MCP App, served as a resource.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct App {
    pub uri: &'static str,
    pub html: &'static str,
}

/// One registered operation.
pub(crate) struct Op<M> {
    pub tool: Tool,
    pub call: Handler<M, Value>,
}

/// The operations a module registers.
pub struct Ops<M> {
    pub(crate) list: Vec<Op<M>>,
    pub(crate) resources: Vec<Readable<M>>,
    pub(crate) prompts: Vec<Promptable<M>>,
    pub(crate) completers: Vec<Completer<M>>,
    pub(crate) apps: Vec<App>,
    pub(crate) instructions: Option<&'static str>,
    pub(crate) server: Looks,
}

impl<M: Send + Sync + 'static> Ops<M> {
    pub(crate) fn new() -> Self {
        Self {
            list: Vec::new(),
            resources: Vec::new(),
            prompts: Vec::new(),
            completers: Vec::new(),
            apps: Vec::new(),
            instructions: None,
            server: Looks::default(),
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

    /// Registers a resource at `uri`, readable by spaces entitled to
    /// `feature`. A URI with `{name}` variables is a template: the handler
    /// receives the variables, percent-decoded, as `A`, and `A`'s doc
    /// comment describes the template. A plain URI takes `A = ()` and is
    /// listed.
    ///
    /// # Panics
    ///
    /// When `uri` is registered twice or is not a valid template.
    pub fn resource<A, E, F, Fut>(
        &mut self,
        uri: &'static str,
        name: &'static str,
        feature: Feature,
        handler: F,
    ) -> &mut Self
    where
        A: DeserializeOwned + JsonSchema + 'static,
        E: std::fmt::Display,
        F: Fn(Arc<M>, Ctx, A) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Body, E>> + Send + 'static,
    {
        assert!(
            self.resources.iter().all(|r| r.resource.uri != uri),
            "resource {uri} is registered twice"
        );
        let description = take_text(&mut JsonObject::clone(&schema_for_type::<A>()));
        self.resources.push(Readable {
            resource: Resource {
                uri,
                name,
                feature,
                description,
                looks: Looks::default(),
            },
            read: erase(handler, Ok),
        });
        self
    }

    /// Registers a prompt `name`, available to spaces entitled to `feature`.
    /// Its arguments are the fields of `A`, which MCP passes as strings, and
    /// `A`'s doc comment describes it.
    ///
    /// # Panics
    ///
    /// When `name` is registered twice or `A` is not a struct.
    pub fn prompt<A, E, F, Fut>(
        &mut self,
        name: &'static str,
        feature: Feature,
        handler: F,
    ) -> &mut Self
    where
        A: DeserializeOwned + JsonSchema + 'static,
        E: std::fmt::Display,
        F: Fn(Arc<M>, Ctx, A) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Vec<Message>, E>> + Send + 'static,
    {
        assert!(
            self.prompts.iter().all(|p| p.prompt.name != name),
            "prompt {name} is registered twice"
        );
        let mut schema = JsonObject::clone(&schema_for_type::<A>());
        assert!(
            is_object(&schema),
            "the arguments of {name} must be a struct"
        );
        let description = take_text(&mut schema);
        let arguments = arguments(&schema);
        self.prompts.push(Promptable {
            prompt: Prompt {
                name,
                feature,
                description,
                arguments,
                schema: Arc::new(schema),
                looks: Looks::default(),
            },
            get: erase(handler, Ok),
        });
        self
    }

    /// Completes `argument` of `target`, a prompt's name or a resource
    /// template's URI: the handler gets what the user typed so far and
    /// returns suggestions. Gated by the target's feature.
    ///
    /// # Panics
    ///
    /// When `target` is not registered yet.
    pub fn complete<E, F, Fut>(
        &mut self,
        target: &'static str,
        argument: &'static str,
        handler: F,
    ) -> &mut Self
    where
        E: std::fmt::Display,
        F: Fn(Arc<M>, Ctx, String) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Vec<String>, E>> + Send + 'static,
    {
        let known = self.prompts.iter().any(|p| p.prompt.name == target)
            || self.resources.iter().any(|r| r.resource.uri == target);
        assert!(known, "register {target} before completing it");
        self.completers.push(Completer {
            target,
            argument,
            complete: erase(handler, Ok),
        });
        self
    }

    /// Keeps `tool` from the model: only an MCP App calls it, such as the
    /// undo button on a page that shows what another tool did. It is still
    /// identified, entitled and metered like any call.
    ///
    /// # Panics
    ///
    /// When `tool` is not registered yet.
    pub fn app_only(&mut self, tool: &str) -> &mut Self {
        self.registered(tool).app_only = true;
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

    /// How `target` (a tool or prompt by name, or a resource by URI) shows
    /// to people.
    ///
    /// # Panics
    ///
    /// When `target` is not registered yet.
    pub fn looks(&mut self, target: &str, looks: Looks) -> &mut Self {
        if let Some(op) = self.list.iter_mut().find(|op| op.tool.name == target) {
            op.tool.looks = looks;
        } else if let Some(p) = self.prompts.iter_mut().find(|p| p.prompt.name == target) {
            p.prompt.looks = looks;
        } else if let Some(r) = self.resources.iter_mut().find(|r| r.resource.uri == target) {
            r.resource.looks = looks;
        } else {
            panic!("register {target} before describing it");
        }
        self
    }

    /// How the server itself shows to people, in hosts that list servers.
    pub fn server(&mut self, looks: Looks) -> &mut Self {
        self.server = looks;
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
        let call = erase(handler, |result: R| {
            serde_json::to_value(result).map_err(|error| error.to_string())
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
                app_only: false,
                hints: None,
                looks: Looks::default(),
            },
            call,
        });
        self
    }
}

/// A prompt's arguments: the properties of its arguments' schema.
fn arguments(schema: &JsonObject) -> Vec<PromptArgument> {
    let required = |name: &str| {
        schema
            .get("required")
            .and_then(Value::as_array)
            .is_some_and(|required| required.iter().any(|r| r == name))
    };
    let Some(Value::Object(properties)) = schema.get("properties") else {
        return Vec::new();
    };
    properties
        .iter()
        .map(|(name, property)| PromptArgument {
            name: name.clone(),
            description: property
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_owned),
            required: required(name),
        })
        .collect()
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
