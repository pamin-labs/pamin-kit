//! What a module is to the kit: a manifest that describes it, the operations
//! it registers and the rule its name follows.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::ops::Ops;

/// A module: an engine plus its operation contract, served by the kit.
pub trait Module: Send + Sync + Sized + 'static {
    /// How hosts route to, scale and present this module.
    fn manifest(&self) -> Manifest;

    /// Registers every operation, each with the feature it needs.
    fn register(&self, ops: &mut Ops<Self>);
}

/// What a module says about itself, so that gateways and hosts never branch
/// on its name.
///
/// A module declares it with `&'static str`s; a host reads one back from its
/// JSON as `Manifest<String, ModuleName>`, which refuses a name that breaks
/// the rule.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest<S = &'static str, N = S> {
    pub name: N,
    /// The version of the operation contract, not of the crate.
    pub contract_version: S,
    pub routing: Routing,
    /// Whether operations may run long enough to need a task.
    pub long_running: bool,
    /// The OAuth scopes a caller needs.
    pub scopes: Vec<S>,
    /// What one metered call counts as, for example `call`.
    pub metering_unit: S,
    pub hardware: Hardware,
    pub audience: Audience,
}

/// How a gateway spreads calls over instances.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Routing {
    /// One instance owns a project's data; calls for it go there.
    ShardedByProject,
    /// Any instance serves any call.
    Stateless,
    /// Instances on GPU machines, shared by every caller.
    GpuPool,
}

/// What one instance needs to run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hardware {
    /// Resident memory one instance needs, in MiB.
    pub memory_mib: u32,
    pub gpu: bool,
}

/// Who a module's tools are for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Audience {
    /// Listed to the model, which decides when to call them.
    #[default]
    Model,
    /// Called by a host itself and never listed to a model, such as moderation.
    Host,
}

/// A module's name: a lowercase ASCII letter, then lowercase ASCII letters and
/// digits. Its tools, prompts, resources and app pages are named after it, and
/// the conformance suite holds a module to it; a host reading a manifest back
/// parses its name into this, so the rule is written once.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub struct ModuleName(String);

/// A name [`ModuleName`] refused, as given.
#[derive(Debug)]
pub struct BadModuleName(String);

impl ModuleName {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ModuleName {
    type Error = BadModuleName;

    fn try_from(name: String) -> Result<Self, Self::Error> {
        if name.starts_with(|c: char| c.is_ascii_lowercase()) && name.chars().all(name_char) {
            Ok(Self(name))
        } else {
            Err(BadModuleName(name))
        }
    }
}

/// Written as the plain string it is read from, in every format.
impl Serialize for ModuleName {
    fn serialize<Z: serde::Serializer>(&self, serializer: Z) -> Result<Z::Ok, Z::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl fmt::Display for BadModuleName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "module name {:?} is not a lowercase letter, then lowercase letters and digits",
            self.0
        )
    }
}

impl std::error::Error for BadModuleName {}

/// Whether `c` may appear in a module's name, and so in its operations' names.
pub(crate) fn name_char(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit()
}
