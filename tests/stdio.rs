//! The example module over real stdio, driven by an MCP client.

use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, ReadResourceRequestParams, ResourceContents};
use rmcp::transport::TokioChildProcess;
use serde_json::json;

/// The example binary `cargo test` builds beside this test's own binary.
fn example() -> std::path::PathBuf {
    let deps = std::env::current_exe().unwrap();
    deps.parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("examples/echo")
}

#[tokio::test]
async fn a_client_lists_and_calls_tools_over_stdio() {
    let transport = TokioChildProcess::new(tokio::process::Command::new(example())).unwrap();
    let client = ().serve(transport).await.unwrap();

    let info = client.peer_info().unwrap();
    assert_eq!(info.server_info.as_ref().unwrap().name, "echo");
    assert!(info.instructions.as_deref().unwrap().contains("echo_say"));

    let tools = client.list_all_tools().await.unwrap();
    let names: Vec<_> = tools.iter().map(|tool| tool.name.as_ref()).collect();
    assert_eq!(
        names,
        ["echo_say", "echo_shout", "echo_confirm", "echo_wait"]
    );
    assert!(tools[0].output_schema.is_some());
    assert_eq!(tools[0].title.as_deref(), Some("Say"));
    let hints = tools[0].annotations.as_ref().unwrap();
    assert_eq!(hints.read_only_hint, Some(true));
    assert_eq!(hints.open_world_hint, Some(false));
    assert!(tools[1].annotations.is_none());
    let meta = tools[0].meta.as_ref().unwrap();
    assert_eq!(meta.0["ui"]["resourceUri"], "ui://echo/said");

    let page = client
        .read_resource(ReadResourceRequestParams::new("ui://echo/said"))
        .await
        .unwrap();
    let ResourceContents::TextResourceContents {
        mime_type, text, ..
    } = &page.contents[0]
    else {
        panic!("the page is not text");
    };
    assert_eq!(mime_type.as_deref(), Some("text/html;profile=mcp-app"));
    assert!(text.contains("structuredContent"));

    let said = client
        .call_tool(
            CallToolRequestParams::new("echo_say")
                .with_arguments(json!({"text": "hi"}).as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    assert_eq!(said.is_error, Some(false));
    assert_eq!(
        said.structured_content,
        Some(json!({"text": "hi", "project": "example"}))
    );

    // A paid feature is a tool error the model can read, not a protocol error.
    let paid = client
        .call_tool(
            CallToolRequestParams::new("echo_shout")
                .with_arguments(json!({"text": "hi"}).as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    assert_eq!(paid.is_error, Some(true));

    client.cancel().await.unwrap();
}

/// One progress notification: done, and the total when known.
type Step = (f64, Option<f64>);

/// A client that keeps the progress notifications it receives.
#[derive(Clone, Default)]
struct Watching(std::sync::Arc<std::sync::Mutex<Vec<Step>>>);

impl rmcp::ClientHandler for Watching {
    async fn on_progress(
        &self,
        params: rmcp::model::ProgressNotificationParam,
        _context: rmcp::service::NotificationContext<rmcp::RoleClient>,
    ) {
        self.0.lock().unwrap().push((params.progress, params.total));
    }
}

#[tokio::test]
async fn a_long_call_reports_progress_to_a_client_that_asks() {
    let transport = TokioChildProcess::new(tokio::process::Command::new(example())).unwrap();
    let watching = Watching::default();
    let client = watching.clone().serve(transport).await.unwrap();

    let mut meta = rmcp::model::RequestMetaObject::new();
    meta.set_progress_token(rmcp::model::ProgressToken(
        rmcp::model::NumberOrString::Number(7),
    ));
    let mut wait = CallToolRequestParams::new("echo_wait").with_arguments(
        json!({"text": "later", "millis": 50})
            .as_object()
            .unwrap()
            .clone(),
    );
    wait.meta = Some(meta);
    // A client without tasks waits for the result, with progress meanwhile.
    let waited = client.call_tool(wait).await.unwrap();
    assert_eq!(waited.is_error, Some(false));

    let seen = watching.0.lock().unwrap().clone();
    assert_eq!(seen.len(), 10, "{seen:?}");
    assert_eq!(seen[9], (9.0, Some(10.0)));
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn resources_and_templates_are_listed_and_read() {
    let transport = TokioChildProcess::new(tokio::process::Command::new(example())).unwrap();
    let client = ().serve(transport).await.unwrap();

    let listed = client.list_all_resources().await.unwrap();
    let uris: Vec<_> = listed.iter().map(|r| r.uri.as_str()).collect();
    assert_eq!(uris, ["ui://echo/said", "echo://about"]);

    let templates = client.list_all_resource_templates().await.unwrap();
    assert_eq!(templates[0].uri_template, "echo://say/{text}");
    assert_eq!(
        templates[0].description.as_deref(),
        Some("Text to say, as a resource: what reading it would say back.")
    );

    // A template's variable arrives percent-decoded.
    let read = client
        .read_resource(ReadResourceRequestParams::new("echo://say/hello%20there"))
        .await
        .unwrap();
    let ResourceContents::TextResourceContents {
        mime_type, text, ..
    } = &read.contents[0]
    else {
        panic!("not text");
    };
    assert_eq!(mime_type.as_deref(), Some("application/json"));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(text).unwrap(),
        json!({"text": "hello there", "project": "example"})
    );

    let missing = client
        .read_resource(ReadResourceRequestParams::new("echo://nowhere"))
        .await;
    assert!(missing.is_err());
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn prompts_are_listed_filled_and_completed() {
    let transport = TokioChildProcess::new(tokio::process::Command::new(example())).unwrap();
    let client = ().serve(transport).await.unwrap();

    let prompts = client.list_all_prompts().await.unwrap();
    assert_eq!(prompts[0].name, "echo_greet");
    let argument = &prompts[0].arguments.as_ref().unwrap()[0];
    assert_eq!(argument.name, "name");
    assert_eq!(argument.required, Some(true));

    let mut arguments = serde_json::Map::new();
    arguments.insert("name".into(), json!("Ada"));
    let filled = client
        .get_prompt(
            rmcp::model::GetPromptRequestParams::new("echo_greet").with_arguments(arguments),
        )
        .await
        .unwrap();
    let text = serde_json::to_value(&filled.messages[0]).unwrap();
    assert_eq!(text["content"]["text"], "Say hello to Ada with echo_say.");

    let completed = client
        .complete(rmcp::model::CompleteRequestParams::new(
            rmcp::model::Reference::for_prompt("echo_greet"),
            rmcp::model::ArgumentInfo::new("name", "A"),
        ))
        .await
        .unwrap();
    assert_eq!(completed.completion.values, ["Ada", "Alan"]);
    client.cancel().await.unwrap();
}

/// A client whose user answers every question with `yes` or `no`.
#[derive(Clone)]
struct Answering(bool);

impl rmcp::ClientHandler for Answering {
    fn get_info(&self) -> rmcp::model::ClientConfig {
        let mut info = rmcp::model::ClientConfig::default();
        info.capabilities = rmcp::model::ClientCapabilities::builder()
            .enable_elicitation()
            .build();
        info
    }

    async fn create_elicitation(
        &self,
        _request: rmcp::model::ElicitRequestParams,
        _context: rmcp::service::RequestContext<rmcp::RoleClient>,
    ) -> Result<rmcp::model::ElicitResult, rmcp::ErrorData> {
        let mut answer = rmcp::model::ElicitResult::new(rmcp::model::ElicitationAction::Accept);
        answer.content = Some(json!({"yes": self.0}));
        Ok(answer)
    }
}

#[tokio::test]
async fn an_operation_asks_the_user_and_runs_again_with_the_answer() {
    for (yes, said) in [(true, false), (false, true)] {
        let transport = TokioChildProcess::new(tokio::process::Command::new(example())).unwrap();
        let client = Answering(yes).serve(transport).await.unwrap();
        // The client answers the question and calls again, as MRTR has it.
        let result = client
            .call_tool(
                CallToolRequestParams::new("echo_confirm")
                    .with_arguments(json!({"text": "hi"}).as_object().unwrap().clone()),
            )
            .await
            .unwrap();
        assert_eq!(result.is_error, Some(said), "{result:?}");
        client.cancel().await.unwrap();
    }
}
