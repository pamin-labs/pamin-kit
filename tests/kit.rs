//! The call path every transport shares: identify, find, check, run, meter.

use std::sync::{Arc, Mutex};

use pamin_kit::{CallError, Feature, Identity, Kit, Local, Metering, Space, Usage};
use serde_json::{Value, json};

#[path = "../examples/echo.rs"]
mod echo;
use echo::Echo;

fn args(value: Value) -> pamin_kit::JsonObject {
    match value {
        Value::Object(object) => object,
        _ => unreachable!(),
    }
}

#[derive(Clone, Default)]
struct Recorder(Arc<Mutex<Vec<(&'static str, bool)>>>);

impl Metering for Recorder {
    fn record(&self, _space: &Space, usage: Usage) {
        self.0.lock().unwrap().push((usage.operation, usage.ok));
    }
}

struct Nobody;

impl Identity for Nobody {
    async fn identify(&self, _bearer: Option<&str>) -> Option<Arc<Space>> {
        None
    }
}

#[tokio::test]
async fn calls_are_identified_checked_run_and_metered() {
    let recorder = Recorder::default();
    let kit = Kit::new(Echo, Local::new("notes")).with_metering(recorder.clone());

    let said = kit
        .call("echo_say", None, args(json!({"text": "hi"})))
        .await
        .unwrap();
    assert_eq!(said, json!({"text": "hi", "project": "notes"}));

    let failed = kit.call("echo_say", None, args(json!({"text": ""}))).await;
    assert_eq!(failed, Err(CallError::Failed("nothing to say".into())));

    let invalid = kit.call("echo_say", None, args(json!({}))).await;
    assert!(matches!(invalid, Err(CallError::Failed(m)) if m.starts_with("invalid arguments")));

    let paid = kit
        .call("echo_shout", None, args(json!({"text": "hi"})))
        .await;
    assert_eq!(paid, Err(CallError::NotEntitled(Feature::Pro("shout"))));

    let unknown = kit.call("echo_sing", None, args(json!({}))).await;
    assert_eq!(unknown, Err(CallError::Unknown("echo_sing".into())));

    // Refused and unknown calls ran nothing, so there is nothing to meter.
    assert_eq!(
        *recorder.0.lock().unwrap(),
        [("echo_say", true), ("echo_say", false), ("echo_say", false)]
    );
}

#[tokio::test]
async fn unidentified_callers_are_refused_before_anything_else() {
    let kit = Kit::new(Echo, Nobody);
    let refused = kit.call("echo_sing", None, args(json!({}))).await;
    assert_eq!(refused, Err(CallError::Unidentified));
}

#[test]
fn schemas_come_from_the_contract_types() {
    let kit = Kit::new(Echo, Local::new("notes"));
    let say = kit.tools().next().unwrap();
    assert_eq!(say.name, "echo_say");
    assert_eq!(
        say.description.as_deref(),
        Some("Says the text back, with the project it was said in.")
    );
    assert_eq!(
        say.input["properties"]["text"]["description"],
        "What to say."
    );
    assert!(say.input.get("title").is_none());
    assert_eq!(
        say.output.as_ref().unwrap()["required"],
        json!(["text", "project"])
    );
}
