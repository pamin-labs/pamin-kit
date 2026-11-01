//! The conformance suite, on a module that passes and one that does not.
#![cfg(feature = "conformance")]

use std::sync::Arc;

use pamin_kit::schemars::JsonSchema;
use pamin_kit::{Audience, Ctx, Feature, Hardware, Manifest, Module, Ops, Routing};
use serde::Deserialize;

#[path = "../examples/echo/main.rs"]
mod echo;

#[test]
fn the_example_conforms() {
    pamin_kit::conformance!(echo::Echo, "examples/echo/SKILL.md");
}

struct Leaky;

/// Reads a note from any project the caller names.
#[derive(Deserialize, JsonSchema)]
struct Read {
    #[allow(dead_code)]
    project: String,
}

impl Module for Leaky {
    fn manifest(&self) -> Manifest {
        Manifest {
            name: "leaky",
            contract_version: "1.0.0",
            routing: Routing::Stateless,
            long_running: false,
            scopes: vec![],
            metering_unit: "call",
            hardware: Hardware::default(),
            audience: Audience::Model,
        }
    }

    fn register(&self, ops: &mut Ops<Self>) {
        ops.tool(
            "leaky_read",
            Feature::Core,
            |_: Arc<Self>, _: Ctx, _: Read| async { Ok::<_, String>(()) },
        );
    }
}

#[test]
#[should_panic(expected = "leaky_read: argument \"project\" names a tenant")]
fn a_project_argument_fails() {
    pamin_kit::conformance::check(Leaky, std::path::Path::new("examples/echo/SKILL.md"));
}

#[test]
#[should_panic(expected = "leaky_read: SKILL.md does not name it")]
fn a_tool_missing_from_the_skill_fails() {
    pamin_kit::conformance::check(Leaky, std::path::Path::new("examples/echo/SKILL.md"));
}
