//! The example module over stateless Streamable HTTP.
#![cfg(feature = "http")]

use std::sync::Arc;

use pamin_kit::{Identity, Kit, Space};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[path = "../examples/echo/main.rs"]
mod echo;

/// Identifies only the holder of one token, so the test sees the bearer
/// token reach the seam.
struct Token;

impl Identity for Token {
    async fn identify(&self, bearer: Option<&str>) -> Option<Arc<Space>> {
        (bearer == Some("secret")).then(|| {
            Arc::new(Space {
                tenant: "acme".into(),
                user: "ada".into(),
                project: "notes".into(),
            })
        })
    }
}

/// One JSON-RPC request over a fresh connection: status and parsed body.
async fn post(address: std::net::SocketAddr, bearer: &str, body: Value) -> (u16, Value) {
    let body = body.to_string();
    let request = format!(
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\n\
         Accept: application/json, text/event-stream\r\nMCP-Protocol-Version: 2026-07-28\r\nMcp-Method: tools/call\r\nMcp-Name: echo_say\r\n\
         Authorization: Bearer {bearer}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
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

fn call(text: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "echo_say",
            "arguments": {"text": text},
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientInfo": {"name": "test", "version": "1"},
                "io.modelcontextprotocol/clientCapabilities": {}
            }
        }
    })
}

#[tokio::test]
async fn any_request_is_served_on_its_own_with_the_callers_space() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(Kit::new(echo::Echo, Token).serve_http(listener, &["127.0.0.1"]));

    let (status, said) = post(address, "secret", call("hi")).await;
    assert_eq!(status, 200, "{said}");
    assert_eq!(
        said["result"]["structuredContent"],
        json!({"text": "hi", "project": "notes"})
    );

    let (_, refused) = post(address, "wrong", call("hi")).await;
    assert_eq!(refused["error"]["message"], "the caller is not identified");
}
