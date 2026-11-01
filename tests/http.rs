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
