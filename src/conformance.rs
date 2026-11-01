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
    for resource in kit.resources() {
        let uri = resource.uri;
        for variable in
            variables(uri).filter(|name| TENANCY.contains(&name.to_ascii_lowercase().as_str()))
        {
            failures.push(format!(
                "{uri}: variable {variable:?} names a tenant, user or project; \
                 read the space from the context instead"
            ));
        }
        if !resource.is_template() {
            match now(kit.read(uri, None)) {
                Err(CallError::Unidentified) => {}
                other => failures.push(format!(
                    "{uri}: a read without an identity returned {other:?} instead of a refusal"
                )),
            }
        }
    }
    for prompt in kit.prompts() {
        let name = prompt.name;
        for field in tenancy(&prompt.schema) {
            failures.push(format!(
                "{name}: argument {field:?} names a tenant, user or project; \
                 read the space from the context instead"
            ));
        }
        for argument in strings_only(&prompt.schema) {
            failures.push(format!(
                "{name}: argument {argument:?} is not a string, and MCP passes prompt arguments as strings"
            ));
        }
        match now(kit.prompt(name, None, JsonObject::new())) {
            Err(CallError::Unidentified) => {}
            other => failures.push(format!(
                "{name}: a prompt without an identity returned {other:?} instead of a refusal"
            )),
        }
    }
    // A long operation registered as a tool would hold the connection open
    // and be lost on a reconnect; one registered as a task survives it.
    let tasks = kit.tools().any(|tool| tool.long);
    if kit.manifest().long_running != tasks {
        failures.push(format!(
            "the manifest says long_running: {}, but {} operation is registered with Ops::task",
            kit.manifest().long_running,
            if tasks { "an" } else { "no" }
        ));
    }
    assert!(
        failures.is_empty(),
        "conformance failed:\n{}",
        failures.join("\n")
    );
    let tools: Vec<Value> = kit
        .tools()
        .map(|tool| {
            json!({
                "name": tool.name,
                "feature": feature(tool.feature),
                "description": tool.description,
                "input": *tool.input,
                "output": tool.output.as_deref(),
                "task": tool.long,
                "ui": tool.ui,
                "hints": tool.hints,
            })
        })
        .collect();
    let resources: Vec<Value> = kit
        .resources()
        .map(|resource| {
            json!({
                "uri": resource.uri,
                "name": resource.name,
                "feature": feature(resource.feature),
                "description": resource.description,
            })
        })
        .collect();
    let prompts: Vec<Value> = kit
        .prompts()
        .map(|prompt| {
            json!({
                "name": prompt.name,
                "feature": feature(prompt.feature),
                "description": prompt.description,
                "arguments": prompt.arguments,
            })
        })
        .collect();
    json!({"tools": tools, "resources": resources, "prompts": prompts})
}

fn feature(feature: Feature) -> &'static str {
    match feature {
        Feature::Core => "core",
        Feature::Pro(feature) => feature,
    }
}

/// The arguments of a prompt's schema that are not strings (or optional
/// strings).
fn strings_only(schema: &JsonObject) -> Vec<String> {
    let Some(Value::Object(properties)) = schema.get("properties") else {
        return Vec::new();
    };
    let string = |kind: &Value| match kind {
        Value::String(kind) => kind == "string",
        Value::Array(kinds) => kinds.iter().all(|k| k == "string" || k == "null"),
        _ => false,
    };
    properties
        .iter()
        .filter(|(_, property)| !property.get("type").is_some_and(string))
        .map(|(name, _)| name.clone())
        .collect()
}

/// The `{name}` variables of a resource URI template.
fn variables(uri: &str) -> impl Iterator<Item = &str> {
    uri.split('{')
        .skip(1)
        .filter_map(|part| part.split_once('}').map(|(name, _)| name))
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
    fn template_variables_are_found() {
        let found: Vec<_> = variables("memory://{project}/topics/{topic}").collect();
        assert_eq!(found, ["project", "topic"]);
        assert_eq!(variables("memory://topics").count(), 0);
    }

    #[test]
    fn a_tool_is_named_as_a_whole_word() {
        assert!(names("Use `memory_read` to open a hit.", "memory_read"));
        assert!(!names("memory_read_all", "memory_read"));
        assert!(!names("my_memory_read", "memory_read"));
    }
}
