//! Serving a [`Kit`] as MCP. The only module in the system that names `rmcp`.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::SystemTime;

use rmcp::ErrorData;
use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, CancelTaskParams, ContentBlock,
    CreateTaskResult, DetailedTask, GetTaskParams, GetTaskResult, Implementation, JsonObject,
    ListResourcesResult, ListToolsResult, MetaObject, PaginatedRequestParams,
    ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, Resource,
    ResourceContents, ServerCapabilities, ServerConfig, Task, TaskPayload, TaskStatus,
    ToolAnnotations,
};
use rmcp::model::{ProgressNotificationParam, ProgressToken};
use rmcp::service::{RequestContext, RoleServer};
use serde_json::Value;
use tracing::Instrument;

use crate::kit::{CallError, Caller, Kit};
use crate::module::Module;
use crate::ops::{Hints, Progress};
use crate::seams::{Entitlements, Identity, Metering};
use crate::tasks::{StoredTask, TASK_TTL, TaskState, TaskStore};
use crate::trace;

/// The MIME type MCP Apps hosts render.
const APP_MIME: &str = "text/html;profile=mcp-app";

/// A tool's `_meta` naming the MCP App that shows its results.
fn ui(uri: &str) -> JsonObject {
    let Value::Object(meta) = serde_json::json!({"ui": {"resourceUri": uri}}) else {
        unreachable!()
    };
    meta
}

/// A tool with its hints as MCP annotations; the title also names the tool.
fn annotated(tool: rmcp::model::Tool, hints: Hints) -> rmcp::model::Tool {
    let title = hints.title.map(str::to_owned);
    let annotations = ToolAnnotations::from_raw(
        title.clone(),
        Some(hints.read_only),
        Some(hints.destructive),
        Some(hints.idempotent),
        Some(hints.open_world),
    );
    let tool = tool.annotate(annotations);
    match title {
        Some(title) => tool.with_title(title),
        None => tool,
    }
}

/// How often a caller should poll a task, in milliseconds.
const POLL_MS: u64 = 500;

/// A kit as an MCP server handler. Cheap to clone: one per HTTP request.
pub(crate) struct Server<M, I, E, T, S> {
    kit: Arc<Kit<M, I, E, T, S>>,
    /// Built once: listing tools is the first thing every client does.
    tools: Arc<[rmcp::model::Tool]>,
}

impl<M, I, E, T, S> Clone for Server<M, I, E, T, S> {
    fn clone(&self) -> Self {
        Self {
            kit: self.kit.clone(),
            tools: self.tools.clone(),
        }
    }
}

impl<M: Module, I: Identity, E: Entitlements, T: Metering, S: TaskStore> Server<M, I, E, T, S> {
    pub(crate) fn new(kit: Kit<M, I, E, T, S>) -> Self {
        let tools = kit
            .tools()
            .map(|tool| {
                let listed = rmcp::model::Tool::new_with_raw(
                    tool.name,
                    tool.description.clone().map(Into::into),
                    tool.input.clone(),
                );
                let listed = match &tool.output {
                    Some(output) => listed.with_raw_output_schema(output.clone()),
                    None => listed,
                };
                let listed = match tool.ui {
                    Some(uri) => listed.with_meta(MetaObject(ui(uri))),
                    None => listed,
                };
                match tool.hints {
                    Some(hints) => annotated(listed, hints),
                    None => listed,
                }
            })
            .collect();
        Self {
            kit: Arc::new(kit),
            tools,
        }
    }
}

impl<M: Module, I: Identity, E: Entitlements, T: Metering, S: TaskStore> ServerHandler
    for Server<M, I, E, T, S>
{
    fn get_info(&self) -> ServerConfig {
        let manifest = self.kit.manifest();
        let mut capabilities = ServerCapabilities::builder().enable_tools().build();
        if !self.kit.apps().is_empty() {
            capabilities.resources = Some(Default::default());
        }
        if self.kit.tools().any(|tool| tool.long) {
            capabilities.extensions = ServerCapabilities::builder()
                .enable_tasks()
                .build()
                .extensions;
        }
        let info = ServerConfig::new(capabilities).with_server_info(Implementation::new(
            manifest.name,
            manifest.contract_version,
        ));
        match self.kit.instructions() {
            Some(text) => info.with_instructions(text),
            None => info,
        }
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(self.tools.to_vec()))
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        let pages = self
            .kit
            .apps()
            .iter()
            .map(|app| Resource::new(app.uri, app.uri).with_mime_type(APP_MIME));
        Ok(ListResourcesResult::with_all_items(pages.collect()))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        // A page is the module's code, the same for every caller; the data it
        // shows arrives through tool calls, which are identified.
        let app = self
            .kit
            .apps()
            .iter()
            .find(|app| app.uri == request.uri)
            .ok_or_else(|| ErrorData::resource_not_found(request.uri.clone(), None))?;
        let page = ResourceContents::text(app.html, app.uri).with_mime_type(APP_MIME);
        Ok(ReadResourceResult::new(vec![page]).into())
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let span = trace::request("tools/call", &request.name, &context);
        let result = self.call(request, context).instrument(span.clone()).await;
        trace::outcome(&span, &result);
        result
    }

    async fn get_task(
        &self,
        request: GetTaskParams,
        context: RequestContext<RoleServer>,
    ) -> Result<GetTaskResult, ErrorData> {
        let span = trace::request("tasks/get", &request.task_id, &context);
        let result = self
            .kit
            .task(bearer(&context), &request.task_id)
            .instrument(span.clone())
            .await
            .map(|task| GetTaskResult::new(detailed(task)))
            .or_else(refusal);
        trace::outcome(&span, &result);
        result
    }

    async fn cancel_task(
        &self,
        request: CancelTaskParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        let span = trace::request("tasks/cancel", &request.task_id, &context);
        let result = self
            .kit
            .cancel(bearer(&context), &request.task_id)
            .instrument(span.clone())
            .await
            .map(drop)
            .or_else(refusal);
        trace::outcome(&span, &result);
        result
    }
}

impl<M: Module, I: Identity, E: Entitlements, T: Metering, S: TaskStore> Server<M, I, E, T, S> {
    async fn call(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let bearer = bearer(&context);
        let arguments = request.arguments.unwrap_or_default();
        let long = self.kit.tool(&request.name).is_some_and(|tool| tool.long);
        if long && supports_tasks(&context) {
            let task = self
                .kit
                .start(&request.name, bearer, arguments)
                .await
                .or_else(refusal)?;
            return Ok(CallToolResponse::Task(CreateTaskResult::new(listed(&task))));
        }
        let caller = Caller {
            bearer,
            cancel: context.ct.clone(),
            progress: context.meta.get_progress_token().map(|token| {
                Arc::new(PeerProgress {
                    peer: context.peer.clone(),
                    token,
                }) as Arc<dyn Progress>
            }),
        };
        match self.kit.call_as(&request.name, caller, arguments).await {
            Ok(value) => Ok(CallToolResult::structured(value).into()),
            Err(error @ (CallError::NotEntitled(_) | CallError::Failed(_))) => {
                Ok(tool_error(error).into())
            }
            Err(error) => refusal(error),
        }
    }
}

/// Progress sent to the client as `notifications/progress`.
struct PeerProgress {
    peer: rmcp::service::Peer<RoleServer>,
    token: ProgressToken,
}

impl Progress for PeerProgress {
    fn report(
        &self,
        done: f64,
        total: Option<f64>,
        message: Option<String>,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        let mut note = ProgressNotificationParam::new(self.token.clone(), done);
        note.total = total;
        note.message = message;
        Box::pin(async move {
            // A client that has gone away cannot be told; the call goes on.
            let _ = self.peer.notify_progress(note).await;
        })
    }
}

/// A call the model should read and act on: not entitled, or failed.
fn tool_error(error: CallError) -> CallToolResult {
    trace::tool_error();
    CallToolResult::error(vec![ContentBlock::text(error.to_string())])
}

/// What no caller should retry as it is, as a protocol error. When starting
/// a task, any error is one: there is no task to report it in.
fn refusal<V>(error: CallError) -> Result<V, ErrorData> {
    let message = error.to_string();
    Err(match error {
        CallError::Unidentified => ErrorData::invalid_request(message, None),
        CallError::Unavailable(_) => ErrorData::internal_error(message, None),
        _ => ErrorData::invalid_params(message, None),
    })
}

/// Whether this request's client declared the tasks extension: per request
/// over stateless HTTP, at initialization over stdio.
fn supports_tasks(context: &RequestContext<RoleServer>) -> bool {
    context
        .meta
        .client_capabilities()
        .or_else(|| {
            context
                .peer
                .peer_info()
                .map(|info| info.capabilities.clone())
        })
        .is_some_and(|capabilities| capabilities.supports_tasks())
}

fn listed(task: &StoredTask) -> Task {
    let status = match task.state {
        TaskState::Working => TaskStatus::Working,
        TaskState::Succeeded(_) | TaskState::Errored(_) => TaskStatus::Completed,
        TaskState::Cancelled => TaskStatus::Cancelled,
    };
    let listed = Task::new(&task.id, status, iso(task.created_at), iso(task.updated_at))
        .with_ttl_ms(TASK_TTL.as_millis() as u64)
        .with_poll_interval_ms(POLL_MS);
    match &task.status {
        Some(status) => listed.with_status_message(status),
        None => listed,
    }
}

fn detailed(task: StoredTask) -> DetailedTask {
    let listed = listed(&task);
    let result = match task.state {
        TaskState::Working => return DetailedTask::new(listed, TaskPayload::Working),
        TaskState::Cancelled => return DetailedTask::new(listed, TaskPayload::Cancelled),
        TaskState::Succeeded(value) => CallToolResult::structured(value),
        // An operation's error is a completed call whose result is an error,
        // as MCP has it; `failed` is for protocol errors, and there are none.
        TaskState::Errored(message) => CallToolResult::error(vec![ContentBlock::text(message)]),
    };
    let Ok(Value::Object(result)) = serde_json::to_value(result) else {
        unreachable!("a tool result serializes to an object")
    };
    DetailedTask::new(listed, TaskPayload::Completed { result })
}

fn iso(time: SystemTime) -> String {
    chrono::DateTime::<chrono::Utc>::from(time).to_rfc3339()
}

/// The request's bearer token: present only over HTTP.
#[cfg(feature = "http")]
fn bearer(context: &RequestContext<RoleServer>) -> Option<&str> {
    context
        .extensions
        .get::<http::request::Parts>()?
        .headers
        .get(http::header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

#[cfg(not(feature = "http"))]
fn bearer(_context: &RequestContext<RoleServer>) -> Option<&str> {
    None
}

impl<M: Module, I: Identity, E: Entitlements, T: Metering, S: TaskStore> Kit<M, I, E, T, S> {
    /// Serves this kit over stdin and stdout until the client disconnects.
    pub async fn serve_stdio(self) -> std::io::Result<()> {
        use rmcp::ServiceExt;
        let running = Server::new(self)
            .serve(rmcp::transport::stdio())
            .await
            .map_err(std::io::Error::other)?;
        running.waiting().await.map_err(std::io::Error::other)?;
        Ok(())
    }
}

#[cfg(feature = "http")]
impl<M: Module, I: Identity, E: Entitlements, T: Metering, S: TaskStore> Kit<M, I, E, T, S> {
    /// Serves this kit as Streamable HTTP at `/mcp` on `listener`, until the
    /// listener fails.
    ///
    /// Stateless: no session outlives a request, so any instance serves any
    /// request. Requests whose `Host` is not in `allowed_hosts` are refused,
    /// which stops DNS rebinding against a local server.
    pub async fn serve_http(
        self,
        listener: tokio::net::TcpListener,
        allowed_hosts: &[&str],
    ) -> std::io::Result<()> {
        use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
        use rmcp::transport::streamable_http_server::{
            StreamableHttpServerConfig, StreamableHttpService,
        };
        let server = Server::new(self);
        let config = StreamableHttpServerConfig::default()
            .with_legacy_session_mode(false)
            .with_json_response(true)
            .with_allowed_hosts(allowed_hosts.iter().copied());
        let service = StreamableHttpService::new(
            move || Ok(server.clone()),
            Arc::new(NeverSessionManager::default()),
            config,
        );
        axum::serve(listener, axum::Router::new().nest_service("/mcp", service)).await
    }
}
