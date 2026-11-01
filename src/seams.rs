//! Where the platform plugs in: who is calling, what they may use, and what
//! they used. Each seam has an open-source default; the SaaS implements them in
//! `pamin-platform`.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

/// Who a call is for and what it works on. It comes only from the caller's
/// identity, never from tool arguments.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Space {
    pub tenant: String,
    pub user: String,
    pub project: String,
}

/// What an operation needs the caller to be entitled to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Feature {
    /// Part of every edition.
    Core,
    /// A named paid feature.
    Pro(&'static str),
}

/// Resolves a request to the space it works on.
pub trait Identity: Send + Sync + 'static {
    /// The caller's space, or `None` to refuse the call. `bearer` is the
    /// request's bearer token, absent over stdio.
    fn identify(&self, bearer: Option<&str>) -> impl Future<Output = Option<Arc<Space>>> + Send;
}

/// Decides whether a space may use a feature.
pub trait Entitlements: Send + Sync + 'static {
    fn allows(&self, space: &Space, feature: Feature) -> bool;
}

/// Records what each call used.
pub trait Metering: Send + Sync + 'static {
    fn record(&self, space: &Space, usage: Usage);
}

/// One call, as metering sees it.
#[derive(Clone, Copy, Debug)]
pub struct Usage {
    pub operation: &'static str,
    pub feature: Feature,
    pub elapsed: Duration,
    /// Whether the operation succeeded.
    pub ok: bool,
}

/// The open-source identity: one local user working on one project, whatever
/// the request carries.
#[derive(Clone, Debug)]
pub struct Local {
    space: Arc<Space>,
}

impl Local {
    pub fn new(project: impl Into<String>) -> Self {
        Self {
            space: Arc::new(Space {
                tenant: "local".into(),
                user: "local".into(),
                project: project.into(),
            }),
        }
    }
}

impl Identity for Local {
    async fn identify(&self, _bearer: Option<&str>) -> Option<Arc<Space>> {
        Some(self.space.clone())
    }
}

/// The open-source entitlements: every core feature, no paid one.
#[derive(Clone, Copy, Debug, Default)]
pub struct Open;

impl Entitlements for Open {
    fn allows(&self, _space: &Space, feature: Feature) -> bool {
        feature == Feature::Core
    }
}

/// The open-source metering: records nothing.
#[derive(Clone, Copy, Debug, Default)]
pub struct Unmetered;

impl Metering for Unmetered {
    fn record(&self, _space: &Space, _usage: Usage) {}
}
