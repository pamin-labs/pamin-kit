//! The framework every Pamin module is built on.
//!
//! A module is an engine plus an operation contract. The kit serves that
//! contract as MCP and does what every module would otherwise repeat around
//! each call: knowing who is calling, checking what they may use and recording
//! usage.

#[cfg(feature = "conformance")]
pub mod conformance;
mod kit;
mod module;
mod ops;
mod seams;
mod serve;
mod tasks;
mod trace;

pub use kit::{CallError, Kit};
pub use module::{Audience, Hardware, Manifest, Module, Routing};
pub use ops::{App, Ctx, JsonObject, Ops, Tool};
/// The schema generator contract types derive with, re-exported so a module
/// and the kit cannot disagree on its version.
pub use schemars;
pub use seams::{Entitlements, Feature, Identity, Local, Metering, Open, Space, Unmetered, Usage};
pub use tasks::{MemoryTasks, StoredTask, TASK_TTL, TaskState, TaskStore};
