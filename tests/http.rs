//! The example module over stateless Streamable HTTP.
#![cfg(feature = "http")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use pamin_kit::{Identity, Kit, Space};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[path = "../examples/echo/main.rs"]
mod echo;

/// One space per token, so the test sees the bearer token reach the seam.
struct Tokens;

impl Identity for Tokens {
    async fn identify(&self, bearer: Option<&str>) -> Option<Arc<Space>> {
        let user = match bearer? {
            "ada" => "ada",
            "bob" => "bob",
            _ => return None,
        };
        Some(Arc::new(Space {
            tenant: "acme".into(),
            user: user.into(),
            project: "notes".into(),
        }))
    }
}

async fn serve() -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(Kit::new(echo::Echo, Tokens).serve_http(listener, &["127.0.0.1"]));
    address
}

/// One JSON-RPC request over a fresh connection, from a client that does or
/// does not support tasks: status and parsed body.
async fn rpc(
    address: SocketAddr,
    bearer: &str,
    tasks: bool,
    method: &str,
    mut params: Value,
) -> (u16, Value) {
    let capabilities = if tasks {
        json!({"extensions": {"io.modelcontextprotocol/tasks": {}}})
    } else {
        json!({})
    };
    params["_meta"] = json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": {"name": "test", "version": "1"},
        "io.modelcontextprotocol/clientCapabilities": capabilities,
    });
    // The header names the tool, or the task.
    let name = params["name"]
        .as_str()
        .or(params["taskId"].as_str())
        .map_or(String::new(), |name| format!("Mcp-Name: {name}\r\n"));
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).to_string();
    let request = format!(
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\n\
         Accept: application/json, text/event-stream\r\nMCP-Protocol-Version: 2026-07-28\r\n\
         Mcp-Method: {method}\r\n{name}Authorization: Bearer {bearer}\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    let (head, body) = response.split_once("\r\n\r\n").unwrap();
    let status = head[9..12].parse().unwrap();
    (status, serde_json::from_str(body).unwrap_or(Value::Null))
}

fn call(tool: &str, arguments: Value) -> Value {
    json!({"name": tool, "arguments": arguments})
}

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
