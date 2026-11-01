//! The checks every module runs in CI, through [`conformance!`](crate::conformance!).

use std::future::Future;
use std::path::Path;
use std::pin::pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use serde_json::{Value, json};

#[doc(hidden)]
pub use insta;

use crate::kit::{CallError, Kit};
use crate::module::Module;
use crate::ops::JsonObject;
use crate::seams::{Feature, Identity, Space};

/// Property names that would let a caller choose whose data a call touches.
/// The space comes from the identity seam alone.
const TENANCY: &[&str] = &[
    "tenant",
    "tenant_id",
    "user",
    "user_id",
    "project",
    "project_id",
    "space",
    "org",
    "organization",
    "account",
    "account_id",
];

const DRAFT_2020_12: &str = "https://json-schema.org/draft/2020-12/schema";

/// Checks `module` and returns its tools as served, for the snapshot.
///
/// # Panics
///
/// With every failed check listed.
pub fn check<M: Module>(module: M, skill: &Path) -> Value {
    let kit = Kit::new(module, Refuse);
    let skill = std::fs::read_to_string(skill)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", skill.display()));
    let mut failures = Vec::new();
    for tool in kit.tools() {
        let name = tool.name;
        for schema in std::iter::once(&tool.input).chain(&tool.output) {
            if let Err(error) = valid(schema) {
                failures.push(format!("{name}: {error}"));
            }
        }
        for field in tenancy(&tool.input) {
            failures.push(format!(
                "{name}: argument {field:?} names a tenant, user or project; \
                 read the space from the context instead"
            ));
        }
        match now(kit.call(name, None, JsonObject::new())) {
            Err(CallError::Unidentified) => {}
            other => failures.push(format!(
                "{name}: a call without an identity returned {other:?} instead of a refusal"
            )),
        }
        if !names(&skill, name) {
            failures.push(format!("{name}: SKILL.md does not name it"));
        }
    }
    assert!(
        failures.is_empty(),
        "conformance failed:\n{}",
        failures.join("\n")
    );
    kit.tools()
        .map(|tool| {
            json!({
                "name": tool.name,
                "feature": match tool.feature {
                    Feature::Core => "core",
                    Feature::Pro(feature) => feature,
                },
                "description": tool.description,
                "input": *tool.input,
                "output": tool.output.as_deref(),
                "task": tool.long,
            })
        })
        .collect()
}

/// Runs the module's conformance checks and snapshots its tool schemas.
///
/// ```ignore
/// #[test]
/// fn conformance() {
///     pamin_kit::conformance!(MyModule::default(), "SKILL.md");
/// }
/// ```
///
/// `skill` is relative to the calling crate's manifest directory. The
/// snapshot is an `insta` snapshot in the calling crate; review changes with
/// `cargo insta review`.
#[macro_export]
macro_rules! conformance {
    ($module:expr, $skill:expr) => {{
        let tools = $crate::conformance::check(
            $module,
            &::std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join($skill),
        );
        $crate::conformance::insta::assert_json_snapshot!("tools", tools);
    }};
}

/// A JSON Schema 2020-12 that the meta-schema accepts.
fn valid(schema: &JsonObject) -> Result<(), String> {
    if schema.get("$schema").and_then(Value::as_str) != Some(DRAFT_2020_12) {
        return Err(format!(
            "schema is not JSON Schema 2020-12 ({DRAFT_2020_12})"
        ));
    }
    let schema = Value::Object(schema.clone());
    jsonschema::meta::validate(&schema).map_err(|error| format!("invalid schema: {error}"))
}

/// Every property name, at any depth, that names a tenancy.
fn tenancy(schema: &JsonObject) -> Vec<String> {
    let mut found = Vec::new();
    let mut pending = vec![schema];
    while let Some(object) = pending.pop() {
        if let Some(Value::Object(properties)) = object.get("properties") {
            found.extend(
                properties
                    .keys()
                    .filter(|key| TENANCY.contains(&key.to_ascii_lowercase().as_str()))
                    .cloned(),
            );
        }
        for value in object.values() {
            match value {
                Value::Object(child) => pending.push(child),
                Value::Array(items) => {
                    pending.extend(items.iter().filter_map(Value::as_object));
                }
                _ => {}
            }
        }
    }
    found
}

/// Whether `text` names `tool` as a whole word.
fn names(text: &str, tool: &str) -> bool {
    let word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    text.match_indices(tool)
        .any(|(at, _)| !text[..at].ends_with(word) && !text[at + tool.len()..].starts_with(word))
}

/// Refuses everyone.
struct Refuse;

impl Identity for Refuse {
    async fn identify(&self, _bearer: Option<&str>) -> Option<Arc<Space>> {
        None
    }
}

/// The value of a future that is ready at once, as a refused call is: it
/// returns before any operation runs.
fn now<F: Future>(future: F) -> F::Output {
    match pin!(future).poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(output) => output,
        Poll::Pending => panic!("a refused call waited on something"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tenancy_is_found_at_any_depth() {
        let schema = json!({
            "properties": {"query": {}, "filter": {"$ref": "#/$defs/Filter"}},
            "$defs": {"Filter": {"properties": {"Project_Id": {}}}}
        });
        assert_eq!(tenancy(schema.as_object().unwrap()), ["Project_Id"]);
    }

    #[test]
    fn a_tool_is_named_as_a_whole_word() {
        assert!(names("Use `memory_read` to open a hit.", "memory_read"));
        assert!(!names("memory_read_all", "memory_read"));
        assert!(!names("my_memory_read", "memory_read"));
    }
}
