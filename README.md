# pamin-kit

The framework Pamin modules are built on. A module registers its operations once; the kit serves them as MCP over stdio and Streamable HTTP, and does around every call what each module would otherwise repeat: knowing who is calling, checking what they may use, recording usage, running long work as tasks, asking the user, and tracing. Used by [PaminMemory](https://github.com/pamin-labs/PaminMemory).

Status: not released yet. Licensed under Apache-2.0.

## A module in one file

```rust,no_run
use std::sync::Arc;

use pamin_kit::schemars::JsonSchema;
use pamin_kit::{Ctx, Feature, Kit, Local, Manifest, Module, Ops};
use serde::{Deserialize, Serialize};

struct Notes;

/// Finds notes that mention the query.
#[derive(Deserialize, JsonSchema)]
struct Search {
    /// What to look for.
    query: String,
}

#[derive(Serialize, JsonSchema)]
struct Found {
    titles: Vec<String>,
}

impl Module for Notes {
    fn manifest(&self) -> Manifest {
        Manifest {
            name: "notes",
            contract_version: "1.0.0",
            routing: pamin_kit::Routing::Stateless,
            long_running: false,
            scopes: vec![],
            metering_unit: "call",
            hardware: Default::default(),
            audience: Default::default(),
        }
    }

    fn register(&self, ops: &mut Ops<Self>) {
        ops.tool(
            "notes_search",
            Feature::Core,
            |_: Arc<Self>, ctx: Ctx, search: Search| async move {
                // ctx.space() is the caller's tenant, user and project.
                let _ = (ctx.space(), search.query);
                Ok::<_, String>(Found { titles: vec![] })
            },
        );
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    Kit::new(Notes, Local::from_working_directory()?).serve_stdio().await
}
```

The arguments type's doc comment becomes the tool's description, its fields the input schema (JSON Schema 2020-12, by `schemars`), and the result type the output schema. A handler returns `Result<R, E>` for any `R: Serialize + JsonSchema` and `E: Display`.

[`examples/echo`](examples/echo/main.rs) uses every feature below, and the kit's tests run against it.

## Operations

Everything is registered on `Ops` in `Module::register`, and each registration returns `&mut Ops` to chain.

| Registration | What it serves |
| --- | --- |
| `tool(name, feature, handler)` | An MCP tool, with structured output |
| `task(name, feature, handler)` | A long operation, run as an MCP task for clients that support tasks and awaited for those that do not; set `Manifest::long_running` |
| `app(tool, "ui://<module>/page", html)` | An MCP App: the page a host shows with the tool's result |
| `app_only(tool)` | Hides a tool from the model; only an MCP App page calls it (an undo button, say) |
| `resource(uri, name, feature, handler)` | Data by URI; `{name}` variables make a template, passed percent-decoded |
| `prompt(name, feature, handler)` | Messages a user picks; arguments are the fields of its arguments type, as strings |
| `complete(target, argument, handler)` | Suggestions for a prompt argument or template variable, given what was typed and the arguments already given (`Completing`) |
| `hints(tool, Hints)` | MCP tool annotations: read-only, destructive, idempotent, open world |
| `looks(target, Looks)` | Title, icons, and for resources MIME type, audience and priority |
| `server(Looks)` | The server's title, icons, description (`about`) and website |
| `instructions(text)` | Guidance sent at connection, for hosts that do not load `SKILL.md` |

Resource handlers return a `Body` (`Body::text`, `Body::bytes`, `Body::json`); prompt handlers return `Vec<Message>` (`Message::text(role, text)` or any `Content`).

## What a handler gets: `Ctx`

| Method | Use |
| --- | --- |
| `space()` / `shared_space()` | The caller's tenant, user and project, resolved by the `Identity` seam. Never take these from arguments; conformance fails a schema that does |
| `is_cancelled()` / `cancelled().await` | The client cancelled, or `tasks/cancel` arrived. Stop where state is whole; the kit never stops an operation half way |
| `progress(done, total, message).await` | Progress notifications, or a task's status message |
| `ask::<T>(key, message)?` | Asks the user for an answer shaped like `T` (elicitation). Returns `Ok(Some(answer))`, `Ok(None)` when declined; `?` on `NeedsInput` ends this run, and the kit runs the operation again from the start with the answer |
| `ask_url(key, message, url)?` | Sends the user to a web page for what must not pass through the client: signing in elsewhere, paying, secrets |
| `attach(Content)` | Returns an image, audio, text, or a resource (linked or embedded) beside the structured result |
| `updated(uri)` | A resource changed: clients listening with `subscriptions/listen` in the same space hear of it |

Asking works for every client: a 2026-07-28 client gets the questions back in the result (MRTR) and calls again; an older client that can elicit is asked by the kit with `elicitation/create`; a task that asks waits in `input_required` until `tasks/update` brings the answers.

## Seams

The kit calls these around every operation. Each has an open-source default; a hosted platform supplies its own.

| Seam | Decides | Default | Set with |
| --- | --- | --- | --- |
| `Identity` | Who calls, from the bearer token, as a `Space` | `Local` (one user, the project from the working directory) | `Kit::new(module, identity)` |
| `Entitlements` | Whether the space may use a `Feature` (`Core` or `Pro("name")`) | `Open` (core only) | `with_entitlements` |
| `Metering` | Records each call's `Usage` | `Unmetered` | `with_metering` |
| `TaskStore` | Keeps task state, written by compare-and-set on a version, so finishing, cancelling and answers never overwrite each other across instances | `InProcessTasks` | `with_tasks` |
| `ChangeFeed` | Carries resource changes to listeners | `InProcessChanges` | `with_changes` |

`Identity`, `Entitlements`, `Metering` and `TaskStore` are type parameters of `Kit`, so the call path dispatches statically. `TaskStore` is two methods (`put` with an expected version, `get`), so any database, file or cache can back it.

## Serving

- **stdio:** `kit.serve_stdio().await`, for local hosts that start the module.
- **Streamable HTTP** (feature `http`): `kit.serve_http(listener, HttpOptions { allowed_hosts, protected })` serves `/mcp`, stateless, so any instance serves any request. With `protected: Some(ProtectedResource { resource, authorization_servers })` the server publishes OAuth protected resource metadata (RFC 9728) and answers an unidentified request with 401 and `WWW-Authenticate`, which is how a client learns where to sign in. Tool arguments marked `#[schemars(extend("x-mcp-header" = "Name"))]` are checked against their `Mcp-Param-Name` headers (SEP-2243).
- **In process:** `kit.call(name, bearer, arguments)`, `read`, `prompt`, `start`, `task`, `update`, `cancel` run the same pipeline without MCP, for a CLI or a test.
- **Tracing:** one `tracing` span per MCP request with the OpenTelemetry MCP attributes; feature `otel` joins the caller's trace from `_meta`.

Both transports speak MCP 2026-07-28 and fall back to 2025-11-25 for older clients. Not served: logging, sampling and roots (deprecated by SEP-2577), and `resources/subscribe` (replaced by `subscriptions/listen`). The lists of tools, prompts and resources are fixed when the kit is built, so there are no list-changed notifications.

## Conformance

With feature `conformance` in a module's dev-dependencies:

```rust,ignore
#[test]
fn conforms() {
    pamin_kit::conformance!(Notes, "SKILL.md");
}
```

It fails the test when a schema is not valid JSON Schema 2020-12, an argument or template variable names a tenant, user or project, a call without identity is not refused, `SKILL.md` misses a tool, prompt arguments are not strings, `long_running` disagrees with the task operations, or a name breaks the convention: the module in lowercase letters and digits, tools and prompts `<module>_<action>`, resources under `<module>://`, app pages under `ui://<module>/`. It then snapshots the tools, resources and prompts as served with `insta`, so a contract change shows in review.

## Cargo features

| Feature | Adds |
| --- | --- |
| (none) | stdio, every operation kind, the seams and tasks |
| `http` | Streamable HTTP and OAuth protected resource metadata (axum) |
| `otel` | Joining the caller's OpenTelemetry trace |
| `conformance` | The conformance suite, for dev-dependencies |

## Performance

The kit's own work per call (identity, entitlement, metering, dispatch) is measured by `cargo bench --bench call` (single caller, budget 50 µs p95; well under 1 µs measured) and `cargo bench --bench concurrent` (256 concurrent callers, no shared lock on the call path). A call that asks, attaches or changes nothing allocates no per-call state for those.

## Releasing

Bump `version` in `Cargo.toml` in a pull request and merge it, then run the `Release` workflow on `main` (`gh workflow run release.yml`). It publishes that version to crates.io through trusted publishing and tags it `v<version>`.
