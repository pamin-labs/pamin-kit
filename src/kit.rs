//! A module with its seams: the one place every call is identified, checked,
//! run and metered, whatever transport it arrived on.

use std::sync::Arc;
use std::time::Instant;

use serde_json::Value;

use crate::module::{Manifest, Module};
use crate::ops::{Ctx, JsonObject, Op, Ops, Tool};
use crate::seams::{Entitlements, Feature, Identity, Local, Metering, Open, Unmetered};

/// A module ready to serve, with the seams its calls go through.
pub struct Kit<M, I = Local, E = Open, T = Unmetered> {
    module: Arc<M>,
    manifest: Manifest,
    ops: Vec<Op<M>>,
    identity: I,
    entitlements: E,
    metering: T,
}

impl<M: Module, I: Identity> Kit<M, I> {
    /// The module with the open-source entitlements and metering.
    pub fn new(module: M, identity: I) -> Self {
        let mut ops = Ops::new();
        module.register(&mut ops);
        Self {
            manifest: module.manifest(),
            module: Arc::new(module),
            ops: ops.list,
            identity,
            entitlements: Open,
            metering: Unmetered,
        }
    }
}

impl<M: Module, I: Identity, E: Entitlements, T: Metering> Kit<M, I, E, T> {
    pub fn with_entitlements<E2: Entitlements>(self, entitlements: E2) -> Kit<M, I, E2, T> {
        Kit {
            module: self.module,
            manifest: self.manifest,
            ops: self.ops,
            identity: self.identity,
            entitlements,
            metering: self.metering,
        }
    }

    pub fn with_metering<T2: Metering>(self, metering: T2) -> Kit<M, I, E, T2> {
        Kit {
            module: self.module,
            manifest: self.manifest,
            ops: self.ops,
            identity: self.identity,
            entitlements: self.entitlements,
            metering,
        }
    }

    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// Every registered operation, in registration order.
    pub fn tools(&self) -> impl Iterator<Item = &Tool> {
        self.ops.iter().map(|op| &op.tool)
    }

    /// Calls operation `name` for the caller holding `bearer`.
    pub async fn call(
        &self,
        name: &str,
        bearer: Option<&str>,
        arguments: JsonObject,
    ) -> Result<Value, CallError> {
        // Identity before the name, so an unidentified caller learns nothing
        // about which operations exist.
        let space = self
            .identity
            .identify(bearer)
            .await
            .ok_or(CallError::Unidentified)?;
        // ponytail: linear scan; a module has a handful of operations.
        let op = self
            .ops
            .iter()
            .find(|op| op.tool.name == name)
            .ok_or_else(|| CallError::Unknown(name.to_owned()))?;
        let Tool { name, feature, .. } = op.tool;
        if !self.entitlements.allows(&space, feature) {
            return Err(CallError::NotEntitled(feature));
        }
        let started = Instant::now();
        let result = (op.call)(self.module.clone(), Ctx::new(space.clone()), arguments).await;
        self.metering.record(
            &space,
            crate::seams::Usage {
                operation: name,
                feature,
                elapsed: started.elapsed(),
                ok: result.is_ok(),
            },
        );
        result.map_err(CallError::Failed)
    }
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
        }
    }
}

impl std::error::Error for CallError {}
