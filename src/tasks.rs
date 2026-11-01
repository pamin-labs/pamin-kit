//! Long operations as tasks: the call returns at once with a task, and the
//! caller polls it, from the same connection or a new one.
//!
//! Where task state lives is a seam like the others. The kit ships an
//! in-process store for a single local server; anything that must survive a
//! restart or be read by other instances (PostgreSQL, files, object storage)
//! implements [`TaskStore`] in the binary that needs it.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::seams::Space;

/// How long a store keeps a task after its last update, at least. Callers are
/// told this, so they know how long they may come back for a result.
pub const TASK_TTL: Duration = Duration::from_secs(60 * 60);

/// One task, as stored. Serializable so a store can keep it as JSON.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StoredTask {
    pub id: String,
    /// Whose task it is: only the same space may read or cancel it.
    pub space: Space,
    pub operation: String,
    pub state: TaskState,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Working,
    /// The operation's result.
    Succeeded(Value),
    /// The operation's error message, which the model reads.
    Errored(String),
    Cancelled,
}

impl TaskState {
    pub fn is_terminal(&self) -> bool {
        !matches!(self, Self::Working)
    }
}

/// Keeps task state between the call that started a task and the polls that
/// read it.
pub trait TaskStore: Send + Sync + 'static {
    /// Creates or replaces the task with `task.id`. Keeps it for at least
    /// [`TASK_TTL`] after `task.updated_at`.
    fn put(&self, task: StoredTask) -> impl Future<Output = std::io::Result<()>> + Send;

    /// The task with `id`, if it is still kept. The kit checks the space.
    fn get(&self, id: &str) -> impl Future<Output = std::io::Result<Option<StoredTask>>> + Send;
}

/// The open-source store: tasks in this process's memory, gone when it exits.
/// Enough for one local server, where a reconnecting client reaches the same
/// process.
#[derive(Debug, Default)]
pub struct MemoryTasks {
    tasks: Mutex<HashMap<String, StoredTask>>,
}

impl TaskStore for MemoryTasks {
    async fn put(&self, task: StoredTask) -> std::io::Result<()> {
        let mut tasks = self
            .tasks
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        // ponytail: a full sweep per write; a local server has a handful of
        // tasks. Index by expiry if one ever holds thousands.
        let now = SystemTime::now();
        tasks.retain(|_, kept| {
            now.duration_since(kept.updated_at)
                .is_ok_and(|age| age < TASK_TTL)
                || !kept.state.is_terminal()
        });
        tasks.insert(task.id.clone(), task);
        Ok(())
    }

    async fn get(&self, id: &str) -> std::io::Result<Option<StoredTask>> {
        let tasks = self
            .tasks
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        Ok(tasks.get(id).cloned())
    }
}
