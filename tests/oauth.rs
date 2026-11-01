//! A kit served as an OAuth protected resource (RFC 9728): what an MCP
//! client sees before and after it signs in.
#![cfg(feature = "http")]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use pamin_kit::{HttpOptions, Identity, Kit, ProtectedResource, Space};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[path = "../examples/echo/main.rs"]
mod echo;

/// Accepts one token and counts how often it is asked.
#[derive(Clone, Default)]
struct Counted(Arc<AtomicUsize>);

impl Identity for Counted {
    async fn identify(&self, bearer: Option<&str>) -> Option<Arc<Space>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        (bearer == Some("good")).then(|| {
            Arc::new(Space {
                tenant: "acme".into(),
                user: "ada".into(),
                project: "notes".into(),
            })
        })
    }
}

/// One raw HTTP exchange: status, headers and body.
async fn exchange(address: std::net::SocketAddr, request: String) -> (u16, String, String) {
    let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    let (head, body) = response.split_once("\r\n\r\n").unwrap();
    (
        head[9..12].parse().unwrap(),
        head.to_owned(),
        body.to_owned(),
    )
}

fn call(token: &str) -> String {
    let body = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {
            "name": "echo_say", "arguments": {"text": "hi"},
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientInfo": {"name": "test", "version": "1"},
                "io.modelcontextprotocol/clientCapabilities": {}
            }
        }
    })
    .to_string();
    format!(
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\n\
         Accept: application/json, text/event-stream\r\nMCP-Protocol-Version: 2026-07-28\r\n\
         Mcp-Method: tools/call\r\nMcp-Name: echo_say\r\nAuthorization: Bearer {token}\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

#[tokio::test]
async fn a_client_learns_where_to_sign_in_and_is_identified_once() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let identity = Counted::default();
    let options = HttpOptions {
        allowed_hosts: vec!["127.0.0.1".into()],
        protected: Some(ProtectedResource {
            resource: "https://echo.example.com/mcp".into(),
            authorization_servers: vec!["https://auth.example.com".into()],
        }),
    };
    tokio::spawn(Kit::new(echo::Echo, identity.clone()).serve_http(listener, options));

    // Unidentified: 401, pointing at the metadata.
    let (status, head, _) = exchange(address, call("bad")).await;
    assert_eq!(status, 401);
    assert!(
        head.to_ascii_lowercase().contains(
            "www-authenticate: bearer resource_metadata=\"https://echo.example.com/.well-known/oauth-protected-resource/mcp\""
        ),
        "{head}"
    );

    // The metadata is public.
    let metadata = "GET /.well-known/oauth-protected-resource/mcp HTTP/1.1\r\n\
                    Host: 127.0.0.1\r\nConnection: close\r\n\r\n";
    let (status, _, body) = exchange(address, metadata.into()).await;
    assert_eq!(status, 200);
    let metadata: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(metadata["resource"], "https://echo.example.com/mcp");
    assert_eq!(
        metadata["authorization_servers"],
        json!(["https://auth.example.com"])
    );
    assert_eq!(metadata["resource_name"], "echo");

    // Signed in: served, with the identity seam asked once for the request.
    let before = identity.0.load(Ordering::SeqCst);
    let (status, _, body) = exchange(address, call("good")).await;
    assert_eq!(status, 200, "{body}");
    let said: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(said["result"]["structuredContent"]["project"], "notes");
    assert_eq!(identity.0.load(Ordering::SeqCst) - before, 1);
}
