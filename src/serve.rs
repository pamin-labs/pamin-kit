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
    ListResourceTemplatesResult, ListResourcesResult, ListToolsResult, MetaObject,
    PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult,
    Resource, ResourceContents, ResourceTemplate, ServerCapabilities, ServerConfig, Task,
    TaskPayload, TaskStatus, ToolAnnotations,
};
use rmcp::model::{
    ClientCapabilities, CompleteRequestParams, CompleteResult, CompletionInfo, ElicitRequest,
    ElicitRequestParams, ElicitationSchema, GetPromptRequestParams, GetPromptResponse,
    GetPromptResult, InputRequest, InputRequests, InputRequiredResult, InputResponses,
    ListPromptsResult, ProgressNotificationParam, ProgressToken, PromptMessage, ProtocolVersion,
    UpdateTaskParams,
};
use rmcp::model::{
    ResourceUpdatedNotification, ResourceUpdatedNotificationParam, ResourcesCapability,
    ServerNotification, SubscriptionFilter,
};
use rmcp::service::{RequestContext, RoleServer, SubscriptionContext};
use serde_json::Value;
use tokio::sync::broadcast;
use tracing::Instrument;

use crate::kit::Reply;
use crate::kit::{CallError, Caller, Kit};
use crate::module::Module;
use crate::ops::{APP_MIME, Ask, Body, Content, Hints, Progress, Role};
use crate::seams::{Entitlements, Identity, Metering, Space};
use crate::tasks::{StoredTask, TASK_TTL, TaskState, TaskStore};
use crate::trace;

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
        if !self.kit.apps().is_empty() || self.kit.resources().next().is_some() {
            let mut resources = ResourcesCapability::default();
            // Module resources can change; app pages cannot.
            resources.subscribe = self.kit.resources().next().is_some().then_some(true);
            capabilities.resources = Some(resources);
        }
        if self.kit.prompts().next().is_some() {
            capabilities.prompts = Some(Default::default());
        }
        if self.kit.has_completions() {
            capabilities.completions = Some(JsonObject::new());
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

    fn accepted_subscription_filter(
        &self,
        requested: &SubscriptionFilter,
    ) -> Option<SubscriptionFilter> {
        // Only resources change: the lists of tools, prompts and resources
        // are fixed when the kit is built.
        self.kit.resources().next()?;
        let mut accepted = SubscriptionFilter::default();
        accepted.resource_subscriptions = requested.resource_subscriptions.clone();
        Some(accepted)
    }

    async fn listen(&self, subscription: SubscriptionContext) -> Result<(), ErrorData> {
        let request = subscription.request_context();
        let space = self
            .kit
            .identified(&caller(request, None))
            .await
            .or_else(refusal)?;
        let mut changes = self.kit.changes();
        loop {
            let changed = tokio::select! {
                () = subscription.cancelled() => return Ok(()),
                changed = changes.recv() => changed,
            };
            let changed = match changed {
                Ok(changed) => changed,
                // Fell behind: the changes skipped are lost to this listener,
                // which rereads what it cares about on the next one.
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return Ok(()),
            };
            // Another space's change is not this listener's to see.
            if changed.space != space {
                continue;
            }
            let wanted = subscription
                .sink()
                .accepted()
                .resource_subscriptions
                .as_ref()
                .is_some_and(|uris| uris.contains(&changed.uri));
            if wanted {
                let note = ResourceUpdatedNotification::new(ResourceUpdatedNotificationParam::new(
                    changed.uri,
                ));
                if subscription
                    .sink()
                    .send(ServerNotification::ResourceUpdatedNotification(note))
                    .await
                    .is_err()
                {
                    return Ok(());
                }
            }
        }
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
        let resources = self
            .kit
            .resources()
            .filter(|resource| !resource.is_template())
            .map(|resource| Resource::new(resource.uri, resource.name));
        Ok(ListResourcesResult::with_all_items(
            pages.chain(resources).collect(),
        ))
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        let templates = self
            .kit
            .resources()
            .filter(|resource| resource.is_template())
            .map(|resource| {
                let template = ResourceTemplate::new(resource.uri, resource.name);
                match &resource.description {
                    Some(text) => template.with_description(text),
                    None => template,
                }
            });
        Ok(ListResourceTemplatesResult::with_all_items(
            templates.collect(),
        ))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let span = trace::request("resources/read", &request.uri, &context);
        let result = answered(&context, request.input_responses, |answers| {
            self.kit.read_as(&request.uri, caller(&context, answers))
        })
        .instrument(span.clone())
        .await;
        let result = match result {
            Err(CallError::Unknown(uri)) => Err(ErrorData::resource_not_found(uri, None)),
            result => respond(result, |body| {
                ReadResourceResult::new(vec![contents(&request.uri, body)])
            }),
        };
        trace::outcome(&span, &result);
        result
    }

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, ErrorData> {
        let prompts = self.kit.prompts().map(|prompt| {
            let arguments = prompt
                .arguments
                .iter()
                .map(|argument| {
                    let listed = rmcp::model::PromptArgument::new(&argument.name)
                        .with_required(argument.required);
                    match &argument.description {
                        Some(text) => listed.with_description(text),
                        None => listed,
                    }
                })
                .collect();
            rmcp::model::Prompt::new(prompt.name, prompt.description.as_deref(), Some(arguments))
        });
        Ok(ListPromptsResult::with_all_items(prompts.collect()))
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResponse, ErrorData> {
        let span = trace::request("prompts/get", &request.name, &context);
        let arguments = request.arguments.unwrap_or_default();
        let result = answered(&context, request.input_responses, |answers| {
            self.kit
                .prompt_as(&request.name, caller(&context, answers), arguments.clone())
        })
        .instrument(span.clone())
        .await;
        let result = respond(result, |messages| {
            let messages = messages
                .into_iter()
                .map(|message| {
                    let role = match message.role {
                        Role::User => rmcp::model::Role::User,
                        Role::Assistant => rmcp::model::Role::Assistant,
                    };
                    PromptMessage::new(role, block(message.content))
                })
                .collect();
            GetPromptResult::new(messages)
        });
        trace::outcome(&span, &result);
        result
    }

    async fn complete(
        &self,
        request: CompleteRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CompleteResult, ErrorData> {
        let reference = &request.r#ref;
        // A kind of reference newer than this kit has nothing to complete.
        let Some(target) = reference
            .as_prompt_name()
            .or_else(|| reference.as_resource_uri())
        else {
            return Ok(completion(Vec::new()));
        };
        let span = trace::request("completion/complete", target, &context);
        let result = self
            .kit
            .complete_as(
                target,
                &request.argument.name,
                request.argument.value.clone(),
                caller(&context, None),
            )
            .instrument(span.clone())
            .await
            .map(completion)
            .or_else(refusal);
        trace::outcome(&span, &result);
        result
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
            .task_as(&caller(&context, None), &request.task_id)
            .instrument(span.clone())
            .await
            .map(|task| GetTaskResult::new(detailed(task)))
            .or_else(refusal);
        trace::outcome(&span, &result);
        result
    }

    async fn update_task(
        &self,
        request: UpdateTaskParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        let span = trace::request("tasks/update", &request.task_id, &context);
        let result = self
            .kit
            .update_as(
                &caller(&context, None),
                &request.task_id,
                request.input_responses,
            )
            .instrument(span.clone())
            .await
            .map(drop)
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
            .cancel_as(&caller(&context, None), &request.task_id)
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
        let arguments = request.arguments.unwrap_or_default();
        let long = self.kit.tool(&request.name).is_some_and(|tool| tool.long);
        if long && supports_tasks(&context) {
            let task = self
                .kit
                .start_as(&request.name, &caller(&context, None), arguments)
                .await
                .or_else(refusal)?;
            return Ok(CallToolResponse::Task(CreateTaskResult::new(listed(&task))));
        }
        let result = answered(&context, request.input_responses, |answers| {
            self.kit
                .call_as(&request.name, caller(&context, answers), arguments.clone())
        })
        .await;
        match result {
            Err(error @ (CallError::NotEntitled(_) | CallError::Failed(_))) => {
                Ok(tool_error(error).into())
            }
            result => respond(result, replied),
        }
    }
}

/// A kit result as the MCP response `R`: the result, the questions the
/// operation asks the user (MRTR), or a refusal.
fn respond<V, D, R>(result: Result<V, CallError>, done: impl FnOnce(V) -> D) -> Result<R, ErrorData>
where
    D: Into<R>,
    R: From<InputRequiredResult>,
{
    match result {
        Ok(value) => Ok(done(value).into()),
        Err(CallError::InputRequired(asked)) => Ok(questions(asked)?.into()),
        Err(error) => refusal(error),
    }
}

/// What an operation asks the user, as MCP elicitation requests.
fn questions(asked: Vec<Ask>) -> Result<InputRequiredResult, ErrorData> {
    let requests = asked
        .into_iter()
        .map(|ask| {
            let (key, form) = form(ask)?;
            Ok((key, InputRequest::Elicitation(ElicitRequest::new(form))))
        })
        .collect::<Result<InputRequests, ErrorData>>()?;
    Ok(InputRequiredResult::from_input_requests(requests))
}

/// One question as an elicitation form, with the key its answer comes back
/// under.
fn form(ask: Ask) -> Result<(String, ElicitRequestParams), ErrorData> {
    let schema = ElicitationSchema::from_json_schema(ask.schema).map_err(|error| {
        // The module asked with a type an elicitation form cannot show: a
        // mistake in the module.
        ErrorData::internal_error(format!("cannot ask {}: {error}", ask.key), None)
    })?;
    let form = ElicitRequestParams::FormElicitationParams {
        meta: None,
        message: ask.message,
        requested_schema: schema,
    };
    Ok((ask.key, form))
}

/// Suggestions as MCP completion, cut to the most MCP allows with the rest
/// counted.
fn completion(mut values: Vec<String>) -> CompleteResult {
    let total = values.len();
    values.truncate(CompletionInfo::MAX_VALUES);
    let mut info = CompletionInfo::new(values).expect("cut to the maximum");
    if total > CompletionInfo::MAX_VALUES {
        info.total = u32::try_from(total).ok();
        info.has_more = Some(true);
    }
    CompleteResult::new(info)
}

/// A tool's reply as an MCP tool result: the structured result (with its
/// JSON as text, for clients that read only content), then the content the
/// operation attached.
fn replied(reply: Reply) -> CallToolResult {
    let mut result = CallToolResult::structured(reply.value);
    result.content.extend(reply.content.into_iter().map(block));
    result
}

/// Kit content as an MCP content block.
fn block(content: Content) -> ContentBlock {
    use base64::Engine;
    let encode = |data: Vec<u8>| base64::engine::general_purpose::STANDARD.encode(data);
    match content {
        Content::Text(text) => ContentBlock::text(text),
        Content::Image { mime, data } => ContentBlock::image(encode(data), mime),
        Content::Audio { mime, data } => ContentBlock::audio(encode(data), mime),
        Content::Link { uri, name, mime } => {
            let link = Resource::new(uri, name);
            ContentBlock::resource_link(match mime {
                Some(mime) => link.with_mime_type(mime),
                None => link,
            })
        }
        Content::Resource { uri, body } => ContentBlock::resource(contents(&uri, body)),
    }
}

/// A resource's body as MCP resource contents.
fn contents(uri: &str, body: Body) -> ResourceContents {
    match body {
        Body::Text { mime, text } => ResourceContents::text(text, uri).with_mime_type(mime),
        Body::Bytes { mime, bytes } => {
            use base64::Engine;
            let blob = base64::engine::general_purpose::STANDARD.encode(bytes);
            ResourceContents::blob(blob, uri).with_mime_type(mime)
        }
    }
}

/// What the transport knows about a request, for the kit.
fn caller(context: &RequestContext<RoleServer>, answers: Option<InputResponses>) -> Caller<'_> {
    Caller {
        space: identified(context),
        answers,
        bearer: bearer(context),
        cancel: Some(context.ct.clone()),
        progress: context.meta.get_progress_token().map(|token| {
            Arc::new(PeerProgress {
                peer: context.peer.clone(),
                token,
            }) as Arc<dyn Progress>
        }),
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
    capabilities(context).is_some_and(|capabilities| capabilities.supports_tasks())
}

/// What the client declared it can do: per request over stateless HTTP, at
/// initialization over stdio.
fn capabilities(context: &RequestContext<RoleServer>) -> Option<ClientCapabilities> {
    context.meta.client_capabilities().or_else(|| {
        context
            .peer
            .peer_info()
            .map(|info| info.capabilities.clone())
    })
}

/// The most rounds of questions one request may take, as rmcp's client
/// allows.
const MAX_ROUNDS: usize = 10;

/// Runs `attempt` with `answers`, and when the operation asks the user and
/// the client speaks a protocol before MRTR (2026-07-28), asks the client
/// itself (`elicitation/create`) and runs it again with the answers. A newer
/// client gets the questions back as MRTR and calls again on its own; a
/// client that cannot ask at all gets the questions as a refusal.
async fn answered<V, Fut>(
    context: &RequestContext<RoleServer>,
    mut answers: Option<InputResponses>,
    attempt: impl Fn(Option<InputResponses>) -> Fut,
) -> Result<V, CallError>
where
    Fut: Future<Output = Result<V, CallError>>,
{
    let mrtr = context
        .protocol_version()
        .is_some_and(|version| version >= ProtocolVersion::V_2026_07_28);
    let can_ask = capabilities(context).is_some_and(|c| c.elicitation.is_some());
    for _ in 0..MAX_ROUNDS {
        let asked = match attempt(answers.clone()).await {
            Err(CallError::InputRequired(asked)) if !mrtr && can_ask => asked,
            result => return result,
        };
        let given = answers.get_or_insert_with(InputResponses::new);
        for ask in asked {
            let (key, form) = form(ask).map_err(|error| CallError::Failed(error.message.into()))?;
            let answer = context
                .peer
                .create_elicitation(form)
                .await
                .map_err(|error| CallError::Failed(format!("asking the user failed: {error}")))?;
            let answer = serde_json::to_value(answer)
                .map_err(|error| CallError::Failed(error.to_string()))?;
            given.insert(key, answer);
        }
    }
    Err(CallError::Failed(format!(
        "the operation asked the user more than {MAX_ROUNDS} rounds of questions"
    )))
}

fn listed(task: &StoredTask) -> Task {
    let status = match task.state {
        TaskState::Working => TaskStatus::Working,
        TaskState::InputRequired(_) => TaskStatus::InputRequired,
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
        TaskState::InputRequired(asked) => {
            let payload = match questions(asked) {
                Ok(result) => TaskPayload::InputRequired {
                    input_requests: result.input_requests.unwrap_or_default(),
                },
                // The module asked with a form MCP cannot show: the task
                // cannot go on, and the poller is told why.
                Err(error) => {
                    let Ok(Value::Object(error)) = serde_json::to_value(error) else {
                        unreachable!("an error serializes to an object")
                    };
                    TaskPayload::Failed { error }
                }
            };
            return DetailedTask::new(listed, payload);
        }
        TaskState::Succeeded(reply) => replied(reply),
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

/// The space the HTTP layer already identified for this request, so the
/// identity seam runs once.
#[cfg(feature = "http")]
fn identified(context: &RequestContext<RoleServer>) -> Option<Arc<Space>> {
    context
        .extensions
        .get::<http::request::Parts>()?
        .extensions
        .get::<Arc<Space>>()
        .cloned()
}

#[cfg(not(feature = "http"))]
fn identified(_context: &RequestContext<RoleServer>) -> Option<Arc<Space>> {
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

/// How [`Kit::serve_http`] serves.
#[cfg(feature = "http")]
#[derive(Clone, Debug, Default)]
pub struct HttpOptions {
    /// `Host` headers accepted; empty accepts loopback hosts only, which
    /// stops DNS rebinding against a local server.
    pub allowed_hosts: Vec<String>,
    /// When set, the server is an OAuth protected resource: it publishes
    /// its metadata (RFC 9728) and answers an unidentified request with 401,
    /// which is how an MCP client learns where to sign in.
    pub protected: Option<ProtectedResource>,
}

/// An MCP server as an OAuth protected resource (RFC 9728).
#[cfg(feature = "http")]
#[derive(Clone, Debug)]
pub struct ProtectedResource {
    /// The server's public URL, such as `https://memory.example.com/mcp`.
    pub resource: String,
    /// The authorization servers that issue its tokens.
    pub authorization_servers: Vec<String>,
}

/// The metadata a protected resource publishes (RFC 9728, section 2).
#[cfg(feature = "http")]
#[derive(Clone, serde::Serialize)]
struct ResourceMetadata {
    resource: String,
    authorization_servers: Vec<String>,
    scopes_supported: Vec<&'static str>,
    bearer_methods_supported: [&'static str; 1],
    resource_name: &'static str,
}

#[cfg(feature = "http")]
impl<M: Module, I: Identity, E: Entitlements, T: Metering, S: TaskStore> Kit<M, I, E, T, S> {
    /// Serves this kit as Streamable HTTP at `/mcp` on `listener`, until the
    /// listener fails.
    ///
    /// Stateless: no session outlives a request, so any instance serves any
    /// request.
    pub async fn serve_http(
        self,
        listener: tokio::net::TcpListener,
        options: HttpOptions,
    ) -> std::io::Result<()> {
        use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
        use rmcp::transport::streamable_http_server::{
            StreamableHttpServerConfig, StreamableHttpService,
        };
        let server = Server::new(self);
        let kit = server.kit.clone();
        let mut config = StreamableHttpServerConfig::default()
            .with_legacy_session_mode(false)
            .with_json_response(true);
        if !options.allowed_hosts.is_empty() {
            config = config.with_allowed_hosts(options.allowed_hosts);
        }
        let service = StreamableHttpService::new(
            move || Ok(server.clone()),
            Arc::new(NeverSessionManager::default()),
            config,
        );
        let mut router = axum::Router::new();
        match options.protected {
            None => router = router.nest_service("/mcp", service),
            Some(protected) => {
                let (path, metadata) = published(&protected, kit.manifest())?;
                let challenge = format!(
                    "Bearer resource_metadata=\"{}\"",
                    protected_url(&protected.resource, &path)?
                );
                // Only /mcp needs an identity; the metadata is public.
                let mcp = axum::Router::new().nest_service("/mcp", service).layer(
                    axum::middleware::from_fn(move |request, next| {
                        identify_request(kit.clone(), challenge.clone(), request, next)
                    }),
                );
                router = router
                    .route(
                        &path,
                        axum::routing::get(move || {
                            std::future::ready(axum::Json(metadata.clone()))
                        }),
                    )
                    .merge(mcp);
            }
        }
        axum::serve(listener, router).await
    }
}

/// Identifies an HTTP request once, before MCP sees it: an unidentified one
/// gets 401 with where to sign in; an identified one carries its space to
/// the kit, which does not ask the identity seam again.
#[cfg(feature = "http")]
async fn identify_request<M, I, E, T, S>(
    kit: Arc<Kit<M, I, E, T, S>>,
    challenge: String,
    mut request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response
where
    M: Module,
    I: Identity,
    E: Entitlements,
    T: Metering,
    S: TaskStore,
{
    let bearer = request
        .headers()
        .get(http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    match kit.identify(bearer).await {
        Ok(space) => {
            request.extensions_mut().insert(space);
            next.run(request).await
        }
        Err(_) => axum::response::IntoResponse::into_response((
            http::StatusCode::UNAUTHORIZED,
            [(http::header::WWW_AUTHENTICATE, challenge)],
        )),
    }
}

/// Where the metadata is published and what it says. RFC 9728 puts it at
/// `/.well-known/oauth-protected-resource` followed by the resource's path.
#[cfg(feature = "http")]
fn published(
    protected: &ProtectedResource,
    manifest: &crate::module::Manifest,
) -> std::io::Result<(String, ResourceMetadata)> {
    let resource: http::Uri = protected.resource.parse().map_err(std::io::Error::other)?;
    let path = format!(
        "/.well-known/oauth-protected-resource{}",
        resource.path().trim_end_matches('/')
    );
    let metadata = ResourceMetadata {
        resource: protected.resource.clone(),
        authorization_servers: protected.authorization_servers.clone(),
        scopes_supported: manifest.scopes.clone(),
        bearer_methods_supported: ["header"],
        resource_name: manifest.name,
    };
    Ok((path, metadata))
}

/// The metadata's absolute URL: the resource's origin and `path`.
#[cfg(feature = "http")]
fn protected_url(resource: &str, path: &str) -> std::io::Result<String> {
    let resource: http::Uri = resource.parse().map_err(std::io::Error::other)?;
    let (Some(scheme), Some(authority)) = (resource.scheme_str(), resource.authority()) else {
        return Err(std::io::Error::other(format!(
            "the protected resource {resource} is not an absolute URL"
        )));
    };
    Ok(format!("{scheme}://{authority}{path}"))
}
