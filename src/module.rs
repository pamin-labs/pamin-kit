//! What a module is to the kit: a manifest that describes it and the
//! operations it registers.

use serde::Serialize;

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
#[derive(Clone, Debug, Serialize)]
pub struct Manifest {
    pub name: &'static str,
    /// The version of the operation contract, not of the crate.
    pub contract_version: &'static str,
    pub routing: Routing,
    /// Whether operations may run long enough to need a task.
    pub long_running: bool,
    /// The OAuth scopes a caller needs.
    pub scopes: Vec<&'static str>,
    /// What one metered call counts as, for example `call`.
    pub metering_unit: &'static str,
    pub hardware: Hardware,
    pub audience: Audience,
}

/// How a gateway spreads calls over instances.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
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
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Hardware {
    /// Resident memory one instance needs, in MiB.
    pub memory_mib: u32,
    pub gpu: bool,
}

/// Who a module's tools are for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Audience {
    /// Listed to the model, which decides when to call them.
    #[default]
    Model,
    /// Called by a host itself and never listed to a model, such as moderation.
    Host,
}
