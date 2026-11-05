//! Shutting a Streamable HTTP server down gracefully: it stops accepting,
//! finishes what it is serving, and does not wait on idle connections.
#![cfg(feature = "http")]

use std::time::Duration;

use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::oneshot;

mod common;
use common::echo::Echo;
use common::{call, request, rpc, serve_until};

/// Far longer than any shutdown here takes, so only a hang reaches it.
const BOUND: Duration = Duration::from_secs(5);

/// A server of the example module, and the switch that shuts it down.
async fn serve() -> (
    std::net::SocketAddr,
    tokio::task::JoinHandle<std::io::Result<()>>,
    oneshot::Sender<()>,
) {
    let (stop, stopped) = oneshot::channel::<()>();
    let (address, server) = serve_until(Echo, async move {
        let _ = stopped.await;
    })
    .await;
    (address, server, stop)
}

/// Reads from `stream` until `done` holds for what was read, or the stream
/// ends: everything read.
async fn read_until(stream: &mut TcpStream, done: impl Fn(&str) -> bool) -> String {
    let mut seen = String::new();
    let mut buffer = [0u8; 4096];
    loop {
        let read = tokio::time::timeout(BOUND, stream.read(&mut buffer))
            .await
            .expect("the server neither answered nor closed")
            .unwrap();
        if read == 0 {
            return seen;
        }
        seen.push_str(&String::from_utf8_lossy(&buffer[..read]));
        if done(&seen) {
            return seen;
        }
    }
}

/// Whether `response` holds a whole response whose length its head gives.
fn complete(response: &str) -> bool {
    let Some((head, body)) = response.split_once("\r\n\r\n") else {
        return false;
    };
    let length = head
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length: ")?
                .parse()
                .ok()
        })
        .expect("a response with a length");
    body.len() >= length
}

#[tokio::test]
async fn the_server_ends_and_refuses_connections_once_signalled() {
    let (address, server, stop) = serve().await;
    let (status, _) = rpc(
        address,
        "ada",
        false,
        "tools/call",
        call("echo_say", json!({"text": "hi"})),
    )
    .await;
    assert_eq!(status, 200);

    stop.send(()).unwrap();
    tokio::time::timeout(BOUND, server)
        .await
        .expect("the server did not shut down")
        .unwrap()
        .unwrap();
    assert!(
        TcpStream::connect(address).await.is_err(),
        "a connection was accepted after shutdown"
    );
}

#[tokio::test]
async fn a_call_in_flight_finishes_when_the_signal_fires() {
    let (address, server, stop) = serve().await;
    // A progress token makes the call report each step as it goes, so the
    // test sees it running before shutting the server down.
    let mut wait = call("echo_wait", json!({"text": "late", "millis": 300}));
    wait["_meta"] = json!({"progressToken": "p"});
    let mut stream = TcpStream::connect(address).await.unwrap();
    let asked = request("ada", false, "tools/call", wait, "keep-alive");
    stream.write_all(asked.as_bytes()).await.unwrap();
    let started = read_until(&mut stream, |seen| seen.contains("notifications/progress")).await;
    assert!(started.starts_with("HTTP/1.1 200"), "{started}");

    stop.send(()).unwrap();
    // The rest of the reply, then the server closes the connection.
    let rest = read_until(&mut stream, |_| false).await;
    assert!(rest.contains(r#""structuredContent":{"#), "{rest}");
    assert!(rest.contains(r#""text":"late""#), "{rest}");
    tokio::time::timeout(BOUND, server)
        .await
        .expect("the server did not shut down")
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn an_idle_keep_alive_connection_does_not_hold_shutdown() {
    let (address, server, stop) = serve().await;
    let mut stream = TcpStream::connect(address).await.unwrap();
    let say = request(
        "ada",
        false,
        "tools/call",
        call("echo_say", json!({"text": "hi"})),
        "keep-alive",
    );
    stream.write_all(say.as_bytes()).await.unwrap();
    let said = read_until(&mut stream, complete).await;
    assert!(said.starts_with("HTTP/1.1 200"), "{said}");

    // The connection stays open, idle, while the server shuts down.
    stop.send(()).unwrap();
    tokio::time::timeout(BOUND, server)
        .await
        .expect("an idle connection held the server open")
        .unwrap()
        .unwrap();
    let after = read_until(&mut stream, |_| false).await;
    assert!(after.is_empty(), "{after}");
}

#[tokio::test]
async fn an_open_subscription_ends_when_the_signal_fires() {
    let (address, server, stop) = serve().await;
    let mut stream = TcpStream::connect(address).await.unwrap();
    let listen = request(
        "ada",
        false,
        "subscriptions/listen",
        json!({"notifications": {"resourceSubscriptions": ["echo://say/hi"]}}),
        "keep-alive",
    );
    stream.write_all(listen.as_bytes()).await.unwrap();
    // The acknowledgment: the subscription is up.
    let opened = read_until(&mut stream, |seen| seen.contains("\n\n")).await;
    assert!(opened.starts_with("HTTP/1.1 200"), "{opened}");

    stop.send(()).unwrap();
    // The final result that marks a graceful teardown, then the server
    // closes the connection.
    let rest = read_until(&mut stream, |_| false).await;
    assert!(rest.contains(r#""result":"#), "{rest}");
    tokio::time::timeout(BOUND, server)
        .await
        .expect("an open subscription held the server open")
        .unwrap()
        .unwrap();
}
