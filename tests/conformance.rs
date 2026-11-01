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

/// Says it runs long but registers no task.
struct Slow;

impl Module for Slow {
    fn manifest(&self) -> Manifest {
        Manifest {
            long_running: true,
            ..Leaky.manifest()
        }
    }

    fn register(&self, _ops: &mut Ops<Self>) {}
}

#[test]
#[should_panic(expected = "the manifest says long_running: true, but no operation")]
fn a_long_running_manifest_without_tasks_fails() {
    pamin_kit::conformance::check(Slow, std::path::Path::new("examples/echo/SKILL.md"));
}

/// Prompts with a number, which MCP cannot pass.
struct Counting;

/// Counts to a number.
#[derive(Deserialize, JsonSchema)]
struct Count {
    #[allow(dead_code)]
    to: u32,
}

impl Module for Counting {
    fn manifest(&self) -> Manifest {
        Leaky.manifest()
    }

    fn register(&self, ops: &mut Ops<Self>) {
        ops.prompt(
            "count",
            Feature::Core,
            |_: Arc<Self>, _: Ctx, _: Count| async { Ok::<_, String>(Vec::new()) },
        );
    }
}

#[test]
#[should_panic(expected = "count: argument \"to\" is not a string")]
fn a_prompt_argument_that_is_not_a_string_fails() {
    pamin_kit::conformance::check(Counting, std::path::Path::new("examples/echo/SKILL.md"));
}
