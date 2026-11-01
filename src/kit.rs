//! A module with its seams: the one place every call is identified, checked,
//! run and metered, whatever transport it arrived on.

use std::sync::Arc;
use std::time::{Instant, SystemTime};

use serde_json::Value;

use crate::module::{Manifest, Module};
use crate::ops::{App, Ctx, JsonObject, Ops, Tool};
use crate::seams::{
    Entitlements, Feature, Identity, Local, Metering, Open, Space, Unmetered, Usage,
};
use crate::tasks::{MemoryTasks, StoredTask, TaskState, TaskStore};

/// A module ready to serve, with the seams its calls go through.
pub struct Kit<M, I = Local, E = Open, T = Unmetered, S = MemoryTasks> {
    module: Arc<M>,
    manifest: Manifest,
    /// Everything the module registered.
    ops: Ops<M>,
    identity: I,
    entitlements: E,
    metering: T,
    tasks: S,
}

impl<M: Module, I: Identity> Kit<M, I> {
    /// The module with the open-source entitlements, metering and task store.
    pub fn new(module: M, identity: I) -> Self {
        let mut ops = Ops::new();
        module.register(&mut ops);
        Self {
            manifest: module.manifest(),
            module: Arc::new(module),
            ops,
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
            module: self.module,
            manifest: self.manifest,
            ops: self.ops,
            identity: self.identity,
            entitlements,
            metering: self.metering,
            tasks: self.tasks,
        }
    }

    pub fn with_metering<T2: Metering>(self, metering: T2) -> Kit<M, I, E, T2, S> {
        Kit {
            module: self.module,
            manifest: self.manifest,
            ops: self.ops,
            identity: self.identity,
            entitlements: self.entitlements,
            metering,
            tasks: self.tasks,
        }
    }

    pub fn with_tasks<S2: TaskStore>(self, tasks: S2) -> Kit<M, I, E, T, S2> {
        Kit {
            module: self.module,
            manifest: self.manifest,
            ops: self.ops,
            identity: self.identity,
            entitlements: self.entitlements,
            metering: self.metering,
            tasks,
        }
    }

    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// Every registered operation, in registration order.
    pub fn tools(&self) -> impl Iterator<Item = &Tool> {
        self.ops.list.iter().map(|op| &op.tool)
    }

    /// Every registered MCP App page.
    pub fn apps(&self) -> &[App] {
        &self.ops.apps
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
        let (index, space) = self.admit(name, bearer).await?;
        self.run(index, space, arguments)
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
            operation: self.ops.list[index].tool.name.to_owned(),
            state: TaskState::Working,
            created_at: now,
            updated_at: now,
        };
        self.tasks.put(task.clone()).await.map_err(unavailable)?;
        let kit = self.clone();
        let started = task.clone();
        // The task's work stays in the span of the request that started it.
        tokio::spawn(tracing::Instrument::in_current_span(async move {
            let state = match kit.run(index, space, arguments).await {
                Ok(result) => TaskState::Succeeded(result),
                Err(message) => TaskState::Errored(message),
            };
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

    /// Marks the caller's task `id` cancelled. Cooperative: an operation
    /// already running finishes, and its result is discarded, because
    /// stopping it between two awaits could leave the module's state half
    /// written.
    pub async fn cancel(&self, bearer: Option<&str>, id: &str) -> Result<StoredTask, CallError> {
        let task = self.task(bearer, id).await?;
        if task.state.is_terminal() {
            return Ok(task);
        }
        self.finish(task, TaskState::Cancelled)
            .await
            .map_err(unavailable)
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
            .ops
            .list
            .iter()
            .position(|op| op.tool.name == name)
            .ok_or_else(|| CallError::Unknown(name.to_owned()))?;
        let feature = self.ops.list[index].tool.feature;
        if !self.entitlements.allows(&space, feature) {
            return Err(CallError::NotEntitled(feature));
        }
        Ok((index, space))
    }

    /// Runs an admitted operation and meters it.
    async fn run(
        &self,
        index: usize,
        space: Arc<Space>,
        arguments: JsonObject,
    ) -> Result<Value, String> {
        let op = &self.ops.list[index];
        let started = Instant::now();
        let result = (op.call)(self.module.clone(), Ctx::new(space.clone()), arguments).await;
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
