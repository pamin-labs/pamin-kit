//! The call path every transport shares: identify, find, check, run, meter.

use std::sync::{Arc, Mutex};

use pamin_kit::{CallError, Feature, Identity, Kit, Local, Metering, Space, Usage};
use serde_json::{Value, json};

#[path = "../examples/echo/main.rs"]
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

#[tokio::test]
async fn the_local_project_is_the_working_directory() {
    let local = Local::from_working_directory().unwrap();
    let space = local.identify(None).await.unwrap();
    let here = std::env::current_dir().unwrap().canonicalize().unwrap();
    assert_eq!(space.project, here.to_string_lossy());
}

#[tokio::test]
async fn cancelling_a_task_reaches_its_operation() {
    let recorder = Recorder::default();
    let kit = Arc::new(Kit::new(Echo, Local::new("notes")).with_metering(recorder.clone()));
    let wait = args(json!({"text": "later", "millis": 2000}));
    let task = kit.start("echo_wait", None, wait).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Progress shows while it works.
    let working = kit.task(None, &task.id).await.unwrap();
    assert!(working.status.unwrap().ends_with("waiting"));

    kit.cancel(None, &task.id).await.unwrap();
    // The operation sees the cancellation at its next step, long before its
    // two seconds are up, and stops with an error.
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    assert_eq!(*recorder.0.lock().unwrap(), [("echo_wait", false)]);
    let cancelled = kit.task(None, &task.id).await.unwrap();
    assert_eq!(cancelled.state, pamin_kit::TaskState::Cancelled);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_first_of_finishing_and_cancelling_wins() {
    let kit = Arc::new(Kit::new(Echo, Local::new("notes")));
    for _ in 0..200 {
        let wait = args(json!({"text": "now", "millis": 0}));
        let task = kit.start("echo_wait", None, wait).await.unwrap();
        let cancelled = kit.cancel(None, &task.id).await.unwrap();
        // Whichever won, the task never changes after it is final.
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        let now = kit.task(None, &task.id).await.unwrap();
        assert!(now.state.is_terminal());
        if cancelled.state.is_terminal() {
            assert_eq!(now.state, cancelled.state, "a final state was overwritten");
        }
    }
}

/// Asks once, then says what it was told.
struct Asking;

/// The user's word.
#[derive(serde::Deserialize, pamin_kit::schemars::JsonSchema)]
struct Word {
    /// A word.
    word: String,
}

/// Nothing to give.
#[derive(serde::Deserialize, pamin_kit::schemars::JsonSchema)]
struct Nothing {}

impl pamin_kit::Module for Asking {
    fn manifest(&self) -> pamin_kit::Manifest {
        pamin_kit::Manifest {
            long_running: true,
            ..Echo.manifest()
        }
    }

    fn register(&self, ops: &mut pamin_kit::Ops<Self>) {
        ops.task(
            "asking_word",
            Feature::Core,
            |_: Arc<Self>, ctx: pamin_kit::Ctx, _: Nothing| async move {
                let word: Option<Word> = ctx.ask("word", "Which word?")?;
                Ok::<_, String>(word.map(|w| w.word))
            },
        );
    }
}

#[tokio::test]
async fn a_task_waits_for_the_users_answer_and_goes_on() {
    let kit = Arc::new(Kit::new(Asking, Local::new("notes")));
    let task = kit
        .start("asking_word", None, args(json!({})))
        .await
        .unwrap();
    let asked = loop {
        let now = kit.task(None, &task.id).await.unwrap();
        if now.state != pamin_kit::TaskState::Working {
            break now;
        }
        tokio::task::yield_now().await;
    };
    let pamin_kit::TaskState::InputRequired(questions) = &asked.state else {
        panic!("{:?}", asked.state);
    };
    assert_eq!(questions[0].key, "word");

    let answer = json!({"action": "accept", "content": {"word": "hello"}});
    let answers = [("word".to_owned(), answer)].into_iter().collect();
    kit.update(None, &task.id, answers).await.unwrap();
    let done = loop {
        let now = kit.task(None, &task.id).await.unwrap();
        if now.state.is_terminal() {
            break now;
        }
        tokio::task::yield_now().await;
    };
    assert_eq!(done.state, pamin_kit::TaskState::Succeeded(json!("hello")));
}
