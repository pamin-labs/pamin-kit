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

    let tools = client.list_all_tools().await.unwrap();
    let names: Vec<_> = tools.iter().map(|tool| tool.name.as_ref()).collect();
    assert_eq!(names, ["echo_say", "echo_shout", "echo_wait"]);
    assert!(tools[0].output_schema.is_some());
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
