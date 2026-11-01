//! A module with its seams: the one place every call is identified, checked,
//! run and metered, whatever transport it arrived on.

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::module::{Manifest, Module};
use crate::ops::{
    APP_MIME, App, Ask, Body, Content, Ctx, JsonObject, Looks, Message, Ops, Progress, Prompt,
    Resource, Tool, with_input,
};
use crate::seams::{
    Entitlements, Feature, Identity, Local, Metering, Open, Space, Unmetered, Usage,
};
use crate::tasks::{InProcessTasks, StoredTask, TaskState, TaskStore};

/// A module ready to serve, with the seams its calls go through.
pub struct Kit<M, I = Local, E = Open, T = Unmetered, S = InProcessTasks> {
    core: Core<M>,
    identity: I,
    entitlements: E,
    metering: T,
    tasks: S,
}

/// The module and what it registered: everything but the seams, so that
/// choosing a seam moves one field however the kit grows.
struct Core<M> {
    module: Arc<M>,
    manifest: Manifest,
    ops: Ops<M>,
    /// Which resource a URI reads: an index into `ops.resources`.
    routes: matchit::Router<usize>,
    /// Resource changes, for the clients listening in this process.
    changes: broadcast::Sender<Changed>,
    /// Tasks running in this process, to stop when a caller cancels one.
    running: Mutex<HashMap<String, CancellationToken>>,
}

/// What a transport knows about a call besides its name and arguments.
#[derive(Default)]
pub(crate) struct Caller<'a> {
    /// The request's bearer token: absent over stdio.
    pub bearer: Option<&'a str>,
    /// Cancelled when the client cancels the request; absent when nothing
    /// can cancel it.
    pub cancel: Option<CancellationToken>,
    /// Where progress goes, when the client asked for it.
    pub progress: Option<Arc<dyn Progress>>,
    /// The user's answers to what an earlier round of this call asked.
    pub answers: Option<BTreeMap<String, Value>>,
    /// The caller's space when the transport already identified it, so the
    /// identity seam runs once per request.
    pub space: Option<Arc<Space>>,
}

impl<'a> Caller<'a> {
    /// A caller known only by its bearer token.
    pub(crate) fn holding(bearer: Option<&'a str>) -> Self {
        Self {
            bearer,
            ..Self::default()
        }
    }
}

impl<M: Module, I: Identity> Kit<M, I> {
    /// The module with the open-source entitlements, metering and task store.
    pub fn new(module: M, identity: I) -> Self {
        let mut ops = Ops::new();
        module.register(&mut ops);
        let mut routes = matchit::Router::new();
        for (index, readable) in ops.resources.iter().enumerate() {
            let uri = readable.resource.uri;
            if let Err(error) = routes.insert(uri, index) {
                panic!("resource {uri} cannot be routed: {error}");
            }
        }
        Self {
            core: Core {
                manifest: module.manifest(),
                module: Arc::new(module),
                ops,
                routes,
                changes: broadcast::channel(CHANGES).0,
                running: Mutex::default(),
            },
            identity,
            entitlements: Open,
            metering: Unmetered,
            tasks: InProcessTasks::default(),
        }
    }
}

impl<M: Module, I: Identity, E: Entitlements, T: Metering, S: TaskStore> Kit<M, I, E, T, S> {
    pub fn with_entitlements<E2: Entitlements>(self, entitlements: E2) -> Kit<M, I, E2, T, S> {
        Kit {
            core: self.core,
            identity: self.identity,
            entitlements,
            metering: self.metering,
            tasks: self.tasks,
        }
    }

    pub fn with_metering<T2: Metering>(self, metering: T2) -> Kit<M, I, E, T2, S> {
        Kit {
            core: self.core,
            identity: self.identity,
            entitlements: self.entitlements,
            metering,
            tasks: self.tasks,
        }
    }

    pub fn with_tasks<S2: TaskStore>(self, tasks: S2) -> Kit<M, I, E, T, S2> {
        Kit {
            core: self.core,
            identity: self.identity,
            entitlements: self.entitlements,
            metering: self.metering,
            tasks,
        }
    }

    pub fn manifest(&self) -> &Manifest {
        &self.core.manifest
    }

    /// Every registered operation, in registration order.
    pub fn tools(&self) -> impl Iterator<Item = &Tool> {
        self.core.ops.list.iter().map(|op| &op.tool)
    }

    /// The module's guidance for a model, if it gave any.
    pub fn instructions(&self) -> Option<&'static str> {
        self.core.ops.instructions
    }

    /// How the server shows to people.
    pub fn looks(&self) -> &Looks {
        &self.core.ops.server
    }

    /// Every registered MCP App page.
    pub fn apps(&self) -> &[App] {
        &self.core.ops.apps
    }

    /// Every registered resource and resource template.
    pub fn resources(&self) -> impl Iterator<Item = &Resource> {
        self.core.ops.resources.iter().map(|r| &r.resource)
    }

    /// Reads the resource at `uri` for the caller holding `bearer`.
    pub async fn read(&self, uri: &str, bearer: Option<&str>) -> Result<Body, CallError> {
        let caller = Caller::holding(bearer);
        self.read_as(uri, caller).await
    }

    /// [`Self::read`] with everything the transport knows about the read.
    pub(crate) async fn read_as(&self, uri: &str, caller: Caller<'_>) -> Result<Body, CallError> {
        // An app page is the module's code, the same for every caller; the
        // data it shows arrives through identified tool calls.
        if let Some(app) = self.apps().iter().find(|app| app.uri == uri) {
            return Ok(Body::Text {
                mime: APP_MIME.into(),
                text: app.html.to_owned(),
            });
        }
        let space = self.identified(&caller).await?;
        let found = self
            .core
            .routes
            .at(uri)
            .map_err(|_| CallError::Unknown(uri.to_owned()))?;
        let readable = &self.core.ops.resources[*found.value];
        let Resource {
            uri: template,
            feature,
            ..
        } = readable.resource;
        self.entitled(&space, feature)?;
        let variables = if found.params.is_empty() {
            Value::Null
        } else {
            found
                .params
                .iter()
                .map(|(name, value)| {
                    let value = percent_encoding::percent_decode_str(value).decode_utf8_lossy();
                    (name.to_owned(), Value::String(value.into_owned()))
                })
                .collect::<JsonObject>()
                .into()
        };
        let ctx = Ctx::new(space, caller.cancel, caller.progress);
        self.metered(template, feature, ctx, caller.answers, |ctx| {
            (readable.read)(self.core.module.clone(), ctx, variables)
        })
        .await
        .map(|(value, _)| value)
        .map_err(CallError::from)
    }

    /// Whether the module completes any argument.
    pub(crate) fn has_completions(&self) -> bool {
        !self.core.ops.completers.is_empty()
    }

    /// Every registered prompt.
    pub fn prompts(&self) -> impl Iterator<Item = &Prompt> {
        self.core.ops.prompts.iter().map(|p| &p.prompt)
    }

    /// Fills prompt `name` from `arguments` for the caller holding `bearer`.
    pub async fn prompt(
        &self,
        name: &str,
        bearer: Option<&str>,
        arguments: JsonObject,
    ) -> Result<Vec<Message>, CallError> {
        let caller = Caller::holding(bearer);
        self.prompt_as(name, caller, arguments).await
    }

    /// [`Self::prompt`] with everything the transport knows about the call.
    pub(crate) async fn prompt_as(
        &self,
        name: &str,
        caller: Caller<'_>,
        arguments: JsonObject,
    ) -> Result<Vec<Message>, CallError> {
        let space = self.identified(&caller).await?;
        let promptable = self
            .core
            .ops
            .prompts
            .iter()
            .find(|p| p.prompt.name == name)
            .ok_or_else(|| CallError::Unknown(name.to_owned()))?;
        let Prompt { name, feature, .. } = promptable.prompt;
        self.entitled(&space, feature)?;
        let ctx = Ctx::new(space, caller.cancel, caller.progress);
        self.metered(name, feature, ctx, caller.answers, |ctx| {
            (promptable.get)(self.core.module.clone(), ctx, Value::Object(arguments))
        })
        .await
        .map(|(value, _)| value)
        .map_err(CallError::from)
    }

    /// Suggestions for `argument` of `target` (a prompt or a resource
    /// template) starting from `typed`; none when the module completes it
    /// not.
    pub(crate) async fn complete_as(
        &self,
        target: &str,
        argument: &str,
        typed: String,
        caller: Caller<'_>,
    ) -> Result<Vec<String>, CallError> {
        let space = self.identified(&caller).await?;
        let Some(completer) = self
            .core
            .ops
            .completers
            .iter()
            .find(|c| c.target == target && c.argument == argument)
        else {
            return Ok(Vec::new());
        };
        let feature = self
            .prompts()
            .find(|p| p.name == target)
            .map(|p| p.feature)
            .or_else(|| {
                self.resources()
                    .find(|r| r.uri == target)
                    .map(|r| r.feature)
            })
            .expect("Ops::complete checked the target");
        self.entitled(&space, feature)?;
        let ctx = Ctx::new(space, caller.cancel, caller.progress);
        self.metered(completer.target, feature, ctx, caller.answers, |ctx| {
            (completer.complete)(self.core.module.clone(), ctx, Value::String(typed))
        })
        .await
        .map(|(value, _)| value)
        .map_err(CallError::from)
    }

    /// The registered operation `name`.
    pub fn tool(&self, name: &str) -> Option<&Tool> {
        self.tools().find(|tool| tool.name == name)
    }

    /// Calls operation `name` for the caller holding `bearer`, and waits for
    /// its result.
    pub async fn call(
        &self,
        name: &str,
        bearer: Option<&str>,
        arguments: JsonObject,
    ) -> Result<Reply, CallError> {
        let caller = Caller::holding(bearer);
        self.call_as(name, caller, arguments).await
    }

    /// [`Self::call`] with everything the transport knows about the call.
    pub(crate) async fn call_as(
        &self,
        name: &str,
        caller: Caller<'_>,
        arguments: JsonObject,
    ) -> Result<Reply, CallError> {
        let (index, space) = self.admit(name, &caller).await?;
        let ctx = Ctx::new(space, caller.cancel, caller.progress);
        self.run(index, ctx, Value::Object(arguments), caller.answers)
            .await
            .map_err(CallError::from)
    }

    /// Starts operation `name` as a task for the caller holding `bearer`, and
    /// returns it while the operation runs.
    pub async fn start(
        self: &Arc<Self>,
        name: &str,
        bearer: Option<&str>,
        arguments: JsonObject,
    ) -> Result<StoredTask, CallError> {
        self.start_as(name, &Caller::holding(bearer), arguments)
            .await
    }

    /// [`Self::start`] with everything the transport knows about the call.
    pub(crate) async fn start_as(
        self: &Arc<Self>,
        name: &str,
        caller: &Caller<'_>,
        arguments: JsonObject,
    ) -> Result<StoredTask, CallError> {
        let (index, space) = self.admit(name, caller).await?;
        let now = SystemTime::now();
        let arguments = Value::Object(arguments);
        let task = StoredTask {
            id: uuid::Uuid::new_v4().to_string(),
            space: Space::clone(&space),
            operation: self.core.ops.list[index].tool.name.to_owned(),
            arguments: arguments.clone(),
            state: TaskState::Working,
            status: None,
            created_at: now,
            updated_at: now,
            version: 0,
        };
        // A fresh id: no other writer can hold it.
        self.tasks
            .put(task.clone(), None)
            .await
            .map_err(unavailable)?;
        self.launch(index, space, task.id.clone(), arguments, None);
        Ok(task)
    }

    /// Gives a task that asked for input the user's `answers` and runs its
    /// operation again with them. A task not waiting for input is left as
    /// it is, as are answers to questions it did not ask.
    pub async fn update(
        self: &Arc<Self>,
        bearer: Option<&str>,
        id: &str,
        answers: BTreeMap<String, Value>,
    ) -> Result<StoredTask, CallError> {
        self.update_as(&Caller::holding(bearer), id, answers).await
    }

    /// [`Self::update`] with everything the transport knows about the call.
    pub(crate) async fn update_as(
        self: &Arc<Self>,
        caller: &Caller<'_>,
        id: &str,
        answers: BTreeMap<String, Value>,
    ) -> Result<StoredTask, CallError> {
        self.task_as(caller, id).await?;
        let (task, resumed) = self
            .transition(id, |task| {
                matches!(task.state, TaskState::InputRequired(_)).then(|| StoredTask {
                    state: TaskState::Working,
                    status: None,
                    ..task.clone()
                })
            })
            .await
            .map_err(unavailable)?
            .ok_or_else(|| CallError::NoTask(id.to_owned()))?;
        // Only the writer that resumed it runs it again, however many
        // updates race.
        if resumed {
            let index = self
                .core
                .ops
                .list
                .iter()
                .position(|op| op.tool.name == task.operation)
                .ok_or_else(|| CallError::Unknown(task.operation.clone()))?;
            let space = Arc::new(task.space.clone());
            self.launch(
                index,
                space,
                task.id.clone(),
                task.arguments.clone(),
                Some(answers),
            );
        }
        Ok(task)
    }

    /// Runs operation `index` for task `id` in the background and stores
    /// how it ended: a result, an error, or questions for the user.
    fn launch(
        self: &Arc<Self>,
        index: usize,
        space: Arc<Space>,
        id: String,
        arguments: Value,
        answers: Option<BTreeMap<String, Value>>,
    ) {
        // The request that started the task ends now, so the task gets a
        // token of its own, which cancelling the task triggers.
        let cancel = CancellationToken::new();
        self.running().insert(id.clone(), cancel.clone());
        let progress = Arc::new(TaskProgress {
            kit: self.clone(),
            id: id.clone(),
        });
        let ctx = Ctx::new(space, Some(cancel), Some(progress));
        let kit = self.clone();
        // The task's work stays in the span of the request that started it.
        tokio::spawn(tracing::Instrument::in_current_span(async move {
            let state = match kit.run(index, ctx, arguments, answers).await {
                Ok(result) => TaskState::Succeeded(result),
                Err(Halt::Failed(message)) => TaskState::Errored(message),
                Err(Halt::Input(asked)) => TaskState::InputRequired(asked),
            };
            kit.running().remove(&id);
            if let Err(error) = kit.finish(&id, state).await {
                tracing::error!(%error, "a finished task could not be stored");
            }
        }));
    }

    /// The caller's task `id`.
    pub async fn task(&self, bearer: Option<&str>, id: &str) -> Result<StoredTask, CallError> {
        self.task_as(&Caller::holding(bearer), id).await
    }

    /// [`Self::task`] with everything the transport knows about the call.
    pub(crate) async fn task_as(
        &self,
        caller: &Caller<'_>,
        id: &str,
    ) -> Result<StoredTask, CallError> {
        let space = self.identified(caller).await?;
        match self.tasks.get(id).await.map_err(unavailable)? {
            Some(task) if task.space == *space => Ok(task),
            // Another space's task is reported as missing, not as forbidden,
            // so its existence does not leak.
            _ => Err(CallError::NoTask(id.to_owned())),
        }
    }

    /// Marks the caller's task `id` cancelled. Cooperative: the operation
    /// sees [`Ctx::is_cancelled`] when it runs in this process, stops where
    /// its state is whole, and its result is discarded; the kit never stops
    /// it between two awaits.
    pub async fn cancel(&self, bearer: Option<&str>, id: &str) -> Result<StoredTask, CallError> {
        self.cancel_as(&Caller::holding(bearer), id).await
    }

    /// [`Self::cancel`] with everything the transport knows about the call.
    pub(crate) async fn cancel_as(
        &self,
        caller: &Caller<'_>,
        id: &str,
    ) -> Result<StoredTask, CallError> {
        self.task_as(caller, id).await?;
        if let Some(cancel) = self.running().get(id) {
            cancel.cancel();
        }
        self.finish(id, TaskState::Cancelled)
            .await
            .map_err(unavailable)?
            .ok_or_else(|| CallError::NoTask(id.to_owned()))
    }

    /// Sets a running task's status message, which pollers see.
    async fn status(&self, id: &str, status: String) -> std::io::Result<()> {
        self.transition(id, |task| {
            (!task.state.is_terminal()).then(|| StoredTask {
                status: Some(status.clone()),
                ..task.clone()
            })
        })
        .await
        .map(drop)
    }

    fn running(&self) -> std::sync::MutexGuard<'_, HashMap<String, CancellationToken>> {
        self.core
            .running
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// Whether `space` may use `feature`: the one entitlement check for
    /// tools, resources, prompts and completions.
    fn entitled(&self, space: &Space, feature: Feature) -> Result<(), CallError> {
        if self.entitlements.allows(space, feature) {
            Ok(())
        } else {
            Err(CallError::NotEntitled(feature))
        }
    }

    /// Resource changes from now on, in every space: the listener keeps
    /// its own.
    pub(crate) fn changes(&self) -> broadcast::Receiver<Changed> {
        self.core.changes.subscribe()
    }

    /// The caller's space: as the transport identified it, or from the
    /// identity seam. The one way every entry point learns who is calling.
    pub(crate) async fn identified(&self, caller: &Caller<'_>) -> Result<Arc<Space>, CallError> {
        match &caller.space {
            Some(space) => Ok(space.clone()),
            None => self.identify(caller.bearer).await,
        }
    }

    pub(crate) async fn identify(&self, bearer: Option<&str>) -> Result<Arc<Space>, CallError> {
        self.identity
            .identify(bearer)
            .await
            .ok_or(CallError::Unidentified)
    }

    /// The index of operation `name` and the caller's space, once the caller
    /// is identified and entitled to it.
    async fn admit(
        &self,
        name: &str,
        caller: &Caller<'_>,
    ) -> Result<(usize, Arc<Space>), CallError> {
        // Identity before the name, so an unidentified caller learns nothing
        // about which operations exist.
        let space = self.identified(caller).await?;
        // ponytail: linear scan; a module has a handful of operations.
        let index = self
            .core
            .ops
            .list
            .iter()
            .position(|op| op.tool.name == name)
            .ok_or_else(|| CallError::Unknown(name.to_owned()))?;
        self.entitled(&space, self.core.ops.list[index].tool.feature)?;
        Ok((index, space))
    }

    /// Runs an admitted operation and meters it.
    async fn run(
        &self,
        index: usize,
        ctx: Ctx,
        arguments: Value,
        answers: Option<BTreeMap<String, Value>>,
    ) -> Result<Reply, Halt> {
        let op = &self.core.ops.list[index];
        let (value, content) = self
            .metered(op.tool.name, op.tool.feature, ctx, answers, |ctx| {
                (op.call)(self.core.module.clone(), ctx, arguments)
            })
            .await?;
        Ok(Reply { value, content })
    }

    /// Runs `work` and records its usage as `operation`: one place for
    /// tools, resources and prompts.
    async fn metered<V, Fut>(
        &self,
        operation: &'static str,
        feature: Feature,
        ctx: Ctx,
        answers: Option<BTreeMap<String, Value>>,
        work: impl FnOnce(Ctx) -> Fut,
    ) -> Result<(V, Vec<Content>), Halt>
    where
        Fut: Future<Output = Result<V, String>>,
    {
        let space = ctx.shared_space();
        let started = Instant::now();
        let (result, effects) = with_input(answers, work(ctx)).await;
        // Changes happened whether or not the call then failed. Nobody
        // listening is not an error.
        for uri in effects.updated {
            let _ = self.core.changes.send(Changed {
                space: space.clone(),
                uri,
            });
        }
        let result = match result {
            Ok(value) => Ok((value, effects.attached)),
            // It stopped to ask the user: not a failure, a question.
            Err(_) if !effects.asked.is_empty() => Err(Halt::Input(effects.asked)),
            Err(message) => Err(Halt::Failed(message)),
        };
        self.metering.record(
            &space,
            Usage {
                operation,
                feature,
                elapsed: started.elapsed(),
                ok: result.is_ok(),
            },
        );
        result
    }

    /// Puts task `id` in `state`, unless it already has a final one: the
    /// first of finishing and cancelling wins. Returns the task as stored,
    /// or `None` when no task `id` is kept.
    async fn finish(&self, id: &str, state: TaskState) -> std::io::Result<Option<StoredTask>> {
        let changed = self
            .transition(id, |task| {
                (!task.state.is_terminal()).then(|| StoredTask {
                    state: state.clone(),
                    ..task.clone()
                })
            })
            .await?;
        Ok(changed.map(|(task, _)| task))
    }

    /// Changes task `id` with `change`, atomically against every other
    /// writer: reads it, asks `change` for the next state (`None`: leave it
    /// as it is), and writes that only if nobody wrote in between, reading
    /// again if somebody did. The one way the kit changes a task. Returns
    /// the task as stored and whether this call changed it.
    async fn transition(
        &self,
        id: &str,
        change: impl Fn(&StoredTask) -> Option<StoredTask>,
    ) -> std::io::Result<Option<(StoredTask, bool)>> {
        loop {
            let Some(current) = self.tasks.get(id).await? else {
                return Ok(None);
            };
            let Some(next) = change(&current) else {
                return Ok(Some((current, false)));
            };
            let next = StoredTask {
                version: current.version + 1,
                updated_at: SystemTime::now(),
                ..next
            };
            if self.tasks.put(next.clone(), Some(current.version)).await? {
                return Ok(Some((next, true)));
            }
        }
    }
}

/// What a tool returned: its structured result and any content beside it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Reply {
    pub value: Value,
    pub content: Vec<Content>,
}

/// How many changes a slow listener may fall behind before it skips some.
const CHANGES: usize = 1024;

/// A resource that changed, in the space whose call changed it.
#[derive(Clone, Debug)]
pub(crate) struct Changed {
    pub space: Arc<Space>,
    pub uri: String,
}

/// Why an operation returned no result.
pub(crate) enum Halt {
    Failed(String),
    /// It asked the user these questions.
    Input(Vec<Ask>),
}

impl From<Halt> for CallError {
    fn from(halt: Halt) -> Self {
        match halt {
            Halt::Failed(message) => Self::Failed(message),
            Halt::Input(asked) => Self::InputRequired(asked),
        }
    }
}

/// A task's progress, kept as its status message.
struct TaskProgress<K> {
    kit: Arc<K>,
    id: String,
}

impl<M: Module, I: Identity, E: Entitlements, T: Metering, S: TaskStore> Progress
    for TaskProgress<Kit<M, I, E, T, S>>
{
    fn report(
        &self,
        done: f64,
        total: Option<f64>,
        message: Option<String>,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        let status = match (total, message) {
            (Some(total), Some(message)) => format!("{done}/{total}: {message}"),
            (Some(total), None) => format!("{done}/{total}"),
            (None, Some(message)) => format!("{done}: {message}"),
            (None, None) => format!("{done}"),
        };
        Box::pin(async move {
            if let Err(error) = self.kit.status(&self.id, status).await {
                tracing::warn!(%error, "a task's progress could not be stored");
            }
        })
    }
}

fn unavailable(error: std::io::Error) -> CallError {
    CallError::Unavailable(error.to_string())
}

/// Why a call returned no result.
#[derive(Debug, PartialEq)]
pub enum CallError {
    /// The identity seam refused the caller.
    Unidentified,
    Unknown(String),
    NotEntitled(Feature),
    /// The arguments did not parse, or the operation failed; the message says
    /// which.
    Failed(String),
    /// The operation asked the user these questions; the client answers
    /// and calls again.
    InputRequired(Vec<Ask>),
    /// No task with this id is kept for the caller.
    NoTask(String),
    /// The task store failed.
    Unavailable(String),
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unidentified => f.write_str("the caller is not identified"),
            Self::Unknown(name) => write!(f, "no operation is named {name}"),
            Self::NotEntitled(Feature::Core) => f.write_str("this operation is not enabled"),
            Self::NotEntitled(Feature::Pro(feature)) => {
                write!(
                    f,
                    "this operation needs the {feature} feature, which is not enabled"
                )
            }
            Self::Failed(message) => f.write_str(message),
            Self::InputRequired(asked) => {
                write!(f, "the operation asks the user {} question(s)", asked.len())
            }
            Self::NoTask(id) => write!(f, "no task {id} is kept for this caller"),
            Self::Unavailable(message) => write!(f, "task store unavailable: {message}"),
        }
    }
}

impl std::error::Error for CallError {}
