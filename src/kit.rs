//! A module with its seams: the one place every call is identified, checked,
//! run and metered, whatever transport it arrived on.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime};

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::module::{Manifest, Module};
use crate::ops::{App, Ctx, JsonObject, Ops, Progress, Tool};
use crate::seams::{
    Entitlements, Feature, Identity, Local, Metering, Open, Space, Unmetered, Usage,
};
use crate::tasks::{MemoryTasks, StoredTask, TaskState, TaskStore};

/// A module ready to serve, with the seams its calls go through.
pub struct Kit<M, I = Local, E = Open, T = Unmetered, S = MemoryTasks> {
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
    /// Tasks running in this process, to stop when a caller cancels one.
    running: Mutex<HashMap<String, CancellationToken>>,
}

/// What a transport knows about a call besides its name and arguments.
#[derive(Default)]
pub(crate) struct Caller<'a> {
    /// The request's bearer token: absent over stdio.
    pub bearer: Option<&'a str>,
    /// Cancelled when the client cancels the request.
    pub cancel: CancellationToken,
    /// Where progress goes, when the client asked for it.
    pub progress: Option<Arc<dyn Progress>>,
}

impl<M: Module, I: Identity> Kit<M, I> {
    /// The module with the open-source entitlements, metering and task store.
    pub fn new(module: M, identity: I) -> Self {
        let mut ops = Ops::new();
        module.register(&mut ops);
        Self {
            core: Core {
                manifest: module.manifest(),
                module: Arc::new(module),
                ops,
                running: Mutex::default(),
            },
            identity,
            entitlements: Open,
            metering: Unmetered,
            tasks: MemoryTasks::default(),
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

    /// Every registered MCP App page.
    pub fn apps(&self) -> &[App] {
        &self.core.ops.apps
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
    ) -> Result<Value, CallError> {
        let caller = Caller {
            bearer,
            ..Caller::default()
        };
        self.call_as(name, caller, arguments).await
    }

    /// [`Self::call`] with everything the transport knows about the call.
    pub(crate) async fn call_as(
        &self,
        name: &str,
        caller: Caller<'_>,
        arguments: JsonObject,
    ) -> Result<Value, CallError> {
        let (index, space) = self.admit(name, caller.bearer).await?;
        let ctx = Ctx::new(space, caller.cancel, caller.progress);
        self.run(index, ctx, arguments)
            .await
            .map_err(CallError::Failed)
    }

    /// Starts operation `name` as a task for the caller holding `bearer`, and
    /// returns it while the operation runs.
    pub async fn start(
        self: &Arc<Self>,
        name: &str,
        bearer: Option<&str>,
        arguments: JsonObject,
    ) -> Result<StoredTask, CallError> {
        let (index, space) = self.admit(name, bearer).await?;
        let now = SystemTime::now();
        let task = StoredTask {
            id: uuid::Uuid::new_v4().to_string(),
            space: Space::clone(&space),
            operation: self.core.ops.list[index].tool.name.to_owned(),
            state: TaskState::Working,
            status: None,
            created_at: now,
            updated_at: now,
        };
        self.tasks.put(task.clone()).await.map_err(unavailable)?;
        // The request that started the task ends now, so the task gets a
        // token of its own, which cancelling the task triggers.
        let cancel = CancellationToken::new();
        self.running().insert(task.id.clone(), cancel.clone());
        let progress = Arc::new(TaskProgress {
            kit: self.clone(),
            id: task.id.clone(),
        });
        let ctx = Ctx::new(space, cancel, Some(progress));
        let kit = self.clone();
        let started = task.clone();
        // The task's work stays in the span of the request that started it.
        tokio::spawn(tracing::Instrument::in_current_span(async move {
            let state = match kit.run(index, ctx, arguments).await {
                Ok(result) => TaskState::Succeeded(result),
                Err(message) => TaskState::Errored(message),
            };
            kit.running().remove(&started.id);
            if let Err(error) = kit.finish(started, state).await {
                tracing::error!(%error, "a finished task could not be stored");
            }
        }));
        Ok(task)
    }

    /// The caller's task `id`.
    pub async fn task(&self, bearer: Option<&str>, id: &str) -> Result<StoredTask, CallError> {
        let space = self.identify(bearer).await?;
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
        let task = self.task(bearer, id).await?;
        if task.state.is_terminal() {
            return Ok(task);
        }
        if let Some(cancel) = self.running().get(id) {
            cancel.cancel();
        }
        self.finish(task, TaskState::Cancelled)
            .await
            .map_err(unavailable)
    }

    /// Sets a running task's status message, which pollers see.
    async fn status(&self, id: &str, status: String) -> std::io::Result<()> {
        match self.tasks.get(id).await? {
            Some(task) if !task.state.is_terminal() => {
                let task = StoredTask {
                    status: Some(status),
                    updated_at: SystemTime::now(),
                    ..task
                };
                self.tasks.put(task).await
            }
            _ => Ok(()),
        }
    }

    fn running(&self) -> std::sync::MutexGuard<'_, HashMap<String, CancellationToken>> {
        self.core
            .running
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    async fn identify(&self, bearer: Option<&str>) -> Result<Arc<Space>, CallError> {
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
        bearer: Option<&str>,
    ) -> Result<(usize, Arc<Space>), CallError> {
        // Identity before the name, so an unidentified caller learns nothing
        // about which operations exist.
        let space = self.identify(bearer).await?;
        // ponytail: linear scan; a module has a handful of operations.
        let index = self
            .core
            .ops
            .list
            .iter()
            .position(|op| op.tool.name == name)
            .ok_or_else(|| CallError::Unknown(name.to_owned()))?;
        let feature = self.core.ops.list[index].tool.feature;
        if !self.entitlements.allows(&space, feature) {
            return Err(CallError::NotEntitled(feature));
        }
        Ok((index, space))
    }

    /// Runs an admitted operation and meters it.
    async fn run(&self, index: usize, ctx: Ctx, arguments: JsonObject) -> Result<Value, String> {
        let op = &self.core.ops.list[index];
        let space = ctx.space().clone();
        let started = Instant::now();
        let result = (op.call)(self.core.module.clone(), ctx, arguments).await;
        self.metering.record(
            &space,
            Usage {
                operation: op.tool.name,
                feature: op.tool.feature,
                elapsed: started.elapsed(),
                ok: result.is_ok(),
            },
        );
        result
    }

    /// Stores `task` in its final `state`, unless it was cancelled meanwhile.
    async fn finish(&self, task: StoredTask, state: TaskState) -> std::io::Result<StoredTask> {
        if let Some(now) = self.tasks.get(&task.id).await?
            && now.state.is_terminal()
        {
            return Ok(now);
        }
        let task = StoredTask {
            state,
            updated_at: SystemTime::now(),
            ..task
        };
        self.tasks.put(task.clone()).await?;
        Ok(task)
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
#[derive(Debug, PartialEq, Eq)]
pub enum CallError {
    /// The identity seam refused the caller.
    Unidentified,
    Unknown(String),
    NotEntitled(Feature),
    /// The arguments did not parse, or the operation failed; the message says
    /// which.
    Failed(String),
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
            Self::NoTask(id) => write!(f, "no task {id} is kept for this caller"),
            Self::Unavailable(message) => write!(f, "task store unavailable: {message}"),
        }
    }
}

impl std::error::Error for CallError {}
