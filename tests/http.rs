//! The example module over stateless Streamable HTTP.
#![cfg(feature = "http")]

use std::time::Duration;

use serde_json::json;

mod common;
use common::{call, rpc, serve};

#[tokio::test]
async fn any_request_is_served_on_its_own_with_the_callers_space() {
    let address = serve().await;

    let say = call("echo_say", json!({"text": "hi"}));
    let (status, said) = rpc(address, "ada", false, "tools/call", say.clone()).await;
    assert_eq!(status, 200, "{said}");
    assert_eq!(
        said["result"]["structuredContent"],
        json!({"text": "hi", "project": "notes"})
    );

    let (_, refused) = rpc(address, "eve", false, "tools/call", say).await;
    assert_eq!(
        refused["error"]["message"], "the caller is not identified",
        "{refused}"
    );
}

#[tokio::test]
async fn a_task_survives_a_reconnect_and_stays_its_callers() {
    let address = serve().await;
    let wait = call("echo_wait", json!({"text": "later", "millis": 50}));

    // A client without tasks waits for the result.
    let (_, waited) = rpc(address, "ada", false, "tools/call", wait.clone()).await;
    assert_eq!(waited["result"]["structuredContent"]["text"], "later");

    // A client with tasks gets one at once, and the connection closes.
    let (_, started) = rpc(address, "ada", true, "tools/call", wait.clone()).await;
    assert_eq!(started["result"]["resultType"], "task", "{started}");
    assert_eq!(started["result"]["status"], "working", "{started}");
    let id = started["result"]["taskId"].clone();

    // Another caller cannot see it.
    let get = json!({"taskId": id});
    let (_, hidden) = rpc(address, "bob", true, "tasks/get", get.clone()).await;
    let message = hidden["error"]["message"].as_str().unwrap_or_default();
    assert!(message.starts_with("no task"), "{hidden}");

    // The caller polls from a new connection until it is done.
    let done = loop {
        let (_, polled) = rpc(address, "ada", true, "tasks/get", get.clone()).await;
        if polled["result"]["status"] != "working" {
            break polled;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    assert_eq!(done["result"]["status"], "completed", "{done}");
    assert_eq!(
        done["result"]["result"]["structuredContent"],
        json!({"text": "later", "project": "notes"})
    );

    // A cancelled task stays cancelled when its operation finishes.
    let (_, started) = rpc(address, "ada", true, "tools/call", wait).await;
    let get = json!({"taskId": started["result"]["taskId"]});
    let (_, cancelled) = rpc(address, "ada", true, "tasks/cancel", get.clone()).await;
    assert!(cancelled.get("error").is_none(), "{cancelled}");
    tokio::time::sleep(Duration::from_millis(100)).await;
    let (_, polled) = rpc(address, "ada", true, "tasks/get", get).await;
    assert_eq!(polled["result"]["status"], "cancelled", "{polled}");
}

#[tokio::test]
async fn an_operation_asks_a_new_client_through_mrtr() {
    let address = serve().await;
    let confirm = call("echo_confirm", json!({"text": "hi"}));
    // A 2026-07-28 client gets the question back and calls again with the
    // answer; nothing waits on the server between the two requests.
    let (_, asked) = rpc(address, "ada", false, "tools/call", confirm.clone()).await;
    assert_eq!(asked["result"]["resultType"], "input_required", "{asked}");
    let question = &asked["result"]["inputRequests"]["sure"];
    assert_eq!(question["method"], "elicitation/create", "{asked}");
    assert_eq!(question["params"]["message"], "Say \"hi\"?");

    let mut answered = confirm;
    answered["inputResponses"] = json!({"sure": {"action": "accept", "content": {"yes": true}}});
    let (_, said) = rpc(address, "ada", false, "tools/call", answered).await;
    assert_eq!(said["result"]["structuredContent"]["text"], "hi", "{said}");
}

/// Opens a subscriptions/listen stream as `bearer` for `uri`, and reads it
/// for `wait`: whether a resources/updated notification arrived.
async fn hears(address: std::net::SocketAddr, bearer: &str, uri: &str, wait: Duration) -> bool {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let body = json!({
        "jsonrpc": "2.0", "id": 9, "method": "subscriptions/listen",
        "params": {
            "notifications": {"resourceSubscriptions": [uri]},
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientInfo": {"name": "test", "version": "1"},
                "io.modelcontextprotocol/clientCapabilities": {}
            }
        }
    })
    .to_string();
    let request = format!(
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\n\
         Accept: application/json, text/event-stream\r\nMCP-Protocol-Version: 2026-07-28\r\n\
         Mcp-Method: subscriptions/listen\r\nAuthorization: Bearer {bearer}\r\n\
         Content-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut seen = String::new();
    let mut buffer = [0u8; 4096];
    let deadline = tokio::time::Instant::now() + wait;
    while let Ok(Ok(read)) = tokio::time::timeout_at(deadline, stream.read(&mut buffer)).await {
        if read == 0 {
            break;
        }
        seen.push_str(&String::from_utf8_lossy(&buffer[..read]));
        if seen.contains("notifications/resources/updated") {
            return true;
        }
    }
    false
}

#[tokio::test]
async fn a_listener_hears_changes_in_its_own_space_only() {
    let address = serve().await;
    let wait = Duration::from_millis(500);
    let ada = tokio::spawn(hears(address, "ada", "echo://say/hi", wait));
    let bob = tokio::spawn(hears(address, "bob", "echo://say/hi", wait));
    // Both listeners are up before Ada says it.
    tokio::time::sleep(Duration::from_millis(100)).await;
    rpc(
        address,
        "ada",
        false,
        "tools/call",
        call("echo_say", json!({"text": "hi"})),
    )
    .await;
    assert!(ada.await.unwrap(), "Ada's listener missed her own change");
    assert!(!bob.await.unwrap(), "Bob heard a change in Ada's space");
}

#[tokio::test]
async fn a_page_to_open_is_asked_only_of_a_client_that_can() {
    let address = serve().await;
    let visit = |capabilities: serde_json::Value| {
        let mut visit = call("echo_visit", json!({}));
        visit["_meta"] = json!({"io.modelcontextprotocol/clientCapabilities": capabilities});
        visit
    };

    let (_, asked) = rpc(
        address,
        "ada",
        false,
        "tools/call",
        visit(json!({"elicitation": {"form": {}, "url": {}}})),
    )
    .await;
    let question = &asked["result"]["inputRequests"]["visit"]["params"];
    assert_eq!(question["mode"], "url", "{asked}");
    assert_eq!(question["url"], "https://example.com/echo");
    assert!(question["elicitationId"].is_string());

    let (_, refused) = rpc(
        address,
        "ada",
        false,
        "tools/call",
        visit(json!({"elicitation": {"form": {}}})),
    )
    .await;
    let text = refused["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default();
    assert!(text.contains("cannot do"), "{refused}");
}
