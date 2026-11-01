//! Serving a [`Kit`] as MCP. The only module in the system that names `rmcp`.

use std::sync::Arc;
use std::time::SystemTime;

use rmcp::ErrorData;
use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, CancelTaskParams, ContentBlock,
    CreateTaskResult, DetailedTask, GetTaskParams, GetTaskResult, Implementation, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerConfig, Task, TaskPayload, TaskStatus,
};
use rmcp::service::{RequestContext, RoleServer};
use serde_json::Value;

use crate::kit::{CallError, Kit};
use crate::module::Module;
use crate::seams::{Entitlements, Identity, Metering};
use crate::tasks::{StoredTask, TASK_TTL, TaskState, TaskStore};

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
                match &tool.output {
                    Some(output) => listed.with_raw_output_schema(output.clone()),
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
        let capabilities = ServerCapabilities::builder().enable_tools();
        let capabilities = if self.kit.tools().any(|tool| tool.long) {
            capabilities.enable_tasks().build()
        } else {
            capabilities.build()
        };
        ServerConfig::new(capabilities).with_server_info(Implementation::new(
            manifest.name,
            manifest.contract_version,
        ))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(self.tools.to_vec()))
    }

    async fn call_tool(
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
        match self.kit.call(&request.name, bearer, arguments).await {
            Ok(value) => Ok(CallToolResult::structured(value).into()),
            Err(error @ (CallError::NotEntitled(_) | CallError::Failed(_))) => {
                Ok(tool_error(error).into())
            }
            Err(error) => refusal(error),
        }
    }

    async fn get_task(
        &self,
        request: GetTaskParams,
        context: RequestContext<RoleServer>,
    ) -> Result<GetTaskResult, ErrorData> {
        let task = self
            .kit
            .task(bearer(&context), &request.task_id)
            .await
            .or_else(refusal)?;
        Ok(GetTaskResult::new(detailed(task)))
    }

    async fn cancel_task(
        &self,
        request: CancelTaskParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        self.kit
            .cancel(bearer(&context), &request.task_id)
            .await
            .or_else(refusal)?;
        Ok(())
    }
}

/// A call the model should read and act on: not entitled, or failed.
fn tool_error(error: CallError) -> CallToolResult {
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
    Task::new(&task.id, status, iso(task.created_at), iso(task.updated_at))
        .with_ttl_ms(TASK_TTL.as_millis() as u64)
        .with_poll_interval_ms(POLL_MS)
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
