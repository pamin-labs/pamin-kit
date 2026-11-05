//! What the HTTP tests share: a server of the example module and a raw
//! JSON-RPC client.
// Each test binary uses only some of it.
#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::Arc;

use pamin_kit::{Identity, Kit, Module, Space};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::task::JoinHandle;

#[path = "../../examples/echo/main.rs"]
pub mod echo;

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

pub async fn serve() -> SocketAddr {
    serve_module(echo::Echo).await
}

/// Serves `module` with one space per token.
pub async fn serve_module<M: Module>(module: M) -> SocketAddr {
    serve_until(module, std::future::pending()).await.0
}

/// Serves `module` with one space per token until `signal` completes: its
/// address, and the server, which ends once it has shut down.
pub async fn serve_until<M: Module>(
    module: M,
    signal: impl Future<Output = ()> + Send + 'static,
) -> (SocketAddr, JoinHandle<std::io::Result<()>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let options = pamin_kit::HttpOptions {
        allowed_hosts: vec!["127.0.0.1".into()],
        protected: None,
    };
    let server =
        tokio::spawn(Kit::new(module, Tokens).serve_http_with_shutdown(listener, options, signal));
    (address, server)
}

/// One JSON-RPC request over a fresh connection, from a client that does or
/// does not support tasks: status and parsed body.
pub async fn rpc(
    address: SocketAddr,
    bearer: &str,
    tasks: bool,
    method: &str,
    params: Value,
) -> (u16, Value) {
    let request = request(bearer, tasks, method, params, "close");
    let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    let (head, body) = response.split_once("\r\n\r\n").unwrap();
    let status = head[9..12].parse().unwrap();
    (status, serde_json::from_str(body).unwrap_or(Value::Null))
}

/// The HTTP request [`rpc`] sends, with its `Connection` header set to
/// `connection`.
pub fn request(
    bearer: &str,
    tasks: bool,
    method: &str,
    mut params: Value,
    connection: &str,
) -> String {
    let capabilities = if tasks {
        json!({"extensions": {"io.modelcontextprotocol/tasks": {}}})
    } else {
        json!({})
    };
    let meta = params["_meta"].as_object_mut().map(std::mem::take);
    params["_meta"] = json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": {"name": "test", "version": "1"},
        "io.modelcontextprotocol/clientCapabilities": capabilities,
    });
    // Anything the caller put in `_meta` too, such as a trace context.
    params["_meta"]
        .as_object_mut()
        .unwrap()
        .extend(meta.unwrap_or_default());
    // The header names the tool, or the task.
    let name = params["name"]
        .as_str()
        .or(params["taskId"].as_str())
        .map_or(String::new(), |name| format!("Mcp-Name: {name}\r\n"));
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).to_string();
    format!(
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\n\
         Accept: application/json, text/event-stream\r\nMCP-Protocol-Version: 2026-07-28\r\n\
         Mcp-Method: {method}\r\n{name}Authorization: Bearer {bearer}\r\n\
         Content-Length: {}\r\nConnection: {connection}\r\n\r\n{body}",
        body.len()
    )
}

pub fn call(tool: &str, arguments: Value) -> Value {
    json!({"name": tool, "arguments": arguments})
}
