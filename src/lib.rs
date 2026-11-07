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
#[cfg(feature = "http")]
pub use serve::{HttpOptions, ProtectedResource};
mod tasks;
mod trace;

// The README's example compiles as a doctest, so it cannot drift from the
// API.
#[doc = include_str!("../README.md")]
#[cfg(doctest)]
pub struct ReadmeDoctests;

pub use kit::{CallError, Kit, Reply};
pub use module::{Audience, BadModuleName, Hardware, Manifest, Module, ModuleName, Routing};
pub use ops::{
    App, Ask, Body, Completing, Content, Ctx, Form, Hints, Icon, JsonObject, Looks, Message,
    NeedsInput, Ops, Prompt, PromptArgument, Resource, Role, Tool,
};
/// The schema generator contract types derive with, re-exported so a module
/// and the kit cannot disagree on its version.
pub use schemars;
pub use seams::{
    Change, ChangeFeed, Entitlements, Feature, Identity, InProcessChanges, Local, Metering, Open,
    Space, Unmetered, Usage,
};
pub use tasks::{InProcessTasks, StoredTask, TASK_TTL, TaskState, TaskStore};
