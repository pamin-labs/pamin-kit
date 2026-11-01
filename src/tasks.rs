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

use crate::ops::Ask;
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
    /// The operation's arguments, to run it again with the user's answers
    /// when it asked for input.
    #[serde(default)]
    pub arguments: Value,
    pub state: TaskState,
    /// What the operation last reported while working.
    #[serde(default)]
    pub status: Option<String>,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
    /// Incremented by every write, so that concurrent writers (a task
    /// finishing while it is cancelled, on one instance or several) cannot
    /// overwrite each other unseen.
    #[serde(default)]
    pub version: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Working,
    /// Waiting for the user's answers to these questions (`tasks/update`).
    InputRequired(Vec<Ask>),
    /// The operation's result.
    Succeeded(Value),
    /// The operation's error message, which the model reads.
    Errored(String),
    Cancelled,
}

impl TaskState {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Succeeded(_) | Self::Errored(_) | Self::Cancelled
        )
    }
}

/// Keeps task state between the call that started a task and the polls that
/// read it.
pub trait TaskStore: Send + Sync + 'static {
    /// Writes `task` only if the stored task with its id is at version
    /// `expected` (`None`: no task with that id exists), atomically, and
    /// returns whether it wrote: `false` means another writer got there
    /// first and the kit reads again. In SQL, an insert or an `UPDATE ...
    /// WHERE version = $expected`. Keeps the task for at least [`TASK_TTL`]
    /// after `task.updated_at`.
    fn put(
        &self,
        task: StoredTask,
        expected: Option<u64>,
    ) -> impl Future<Output = std::io::Result<bool>> + Send;

    /// The task with `id`, if it is still kept. The kit checks the space.
    fn get(&self, id: &str) -> impl Future<Output = std::io::Result<Option<StoredTask>>> + Send;
}

/// The open-source store: tasks in this process's memory, gone when it exits.
/// Enough for one local server, where a reconnecting client reaches the same
/// process.
#[derive(Debug, Default)]
pub struct MemoryTasks {
    tasks: Mutex<Kept>,
}

#[derive(Debug, Default)]
struct Kept {
    tasks: HashMap<String, StoredTask>,
    /// When finished tasks past their TTL were last dropped.
    swept: Option<SystemTime>,
}

/// How often expired tasks are dropped, at most.
const SWEEP: Duration = Duration::from_secs(60);

impl TaskStore for MemoryTasks {
    async fn put(&self, task: StoredTask, expected: Option<u64>) -> std::io::Result<bool> {
        let mut kept = self
            .tasks
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let now = SystemTime::now();
        // ponytail: a full sweep, at most once a minute under the lock; a
        // local server keeps a handful of tasks. Index by expiry if one
        // ever keeps many thousands.
        if kept
            .swept
            .is_none_or(|swept| now.duration_since(swept).is_ok_and(|age| age >= SWEEP))
        {
            kept.tasks.retain(|_, task| {
                !task.state.is_terminal()
                    || now
                        .duration_since(task.updated_at)
                        .is_ok_and(|age| age < TASK_TTL)
            });
            kept.swept = Some(now);
        }
        if kept.tasks.get(&task.id).map(|stored| stored.version) != expected {
            return Ok(false);
        }
        kept.tasks.insert(task.id.clone(), task);
        Ok(true)
    }

    async fn get(&self, id: &str) -> std::io::Result<Option<StoredTask>> {
        let kept = self
            .tasks
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        Ok(kept.tasks.get(id).cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(version: u64) -> StoredTask {
        StoredTask {
            id: "t".into(),
            space: Space {
                tenant: "t".into(),
                user: "u".into(),
                project: "p".into(),
            },
            operation: "op".into(),
            arguments: Value::Null,
            state: TaskState::Working,
            status: None,
            created_at: SystemTime::now(),
            updated_at: SystemTime::now(),
            version,
        }
    }

    #[tokio::test]
    async fn only_the_writer_that_saw_the_current_version_writes() {
        let store = MemoryTasks::default();
        assert!(store.put(task(0), None).await.unwrap());
        assert!(!store.put(task(0), None).await.unwrap(), "the id exists");
        assert!(store.put(task(1), Some(0)).await.unwrap());
        assert!(
            !store.put(task(1), Some(0)).await.unwrap(),
            "version 0 is gone"
        );
        assert_eq!(store.get("t").await.unwrap().unwrap().version, 1);
    }
}
