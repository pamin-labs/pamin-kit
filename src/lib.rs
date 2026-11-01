//! The framework every Pamin module is built on.
//!
//! A module is an engine plus an operation contract. The kit serves that
//! contract as MCP and does what every module would otherwise repeat around
//! each call: knowing who is calling, checking what they may use and recording
//! usage.

mod seams;

pub use seams::{Entitlements, Feature, Identity, Local, Metering, Open, Space, Unmetered, Usage};
