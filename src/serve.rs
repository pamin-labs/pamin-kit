//! Serving a [`Kit`] as MCP. The only module in the system that names `rmcp`.

use std::sync::Arc;

use rmcp::ErrorData;
use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig,
};
use rmcp::service::{RequestContext, RoleServer};

use crate::kit::{CallError, Kit};
use crate::module::Module;
use crate::seams::{Entitlements, Identity, Metering};

/// A kit as an MCP server handler. Cheap to clone: one per HTTP request.
pub(crate) struct Server<M, I, E, T> {
    kit: Arc<Kit<M, I, E, T>>,
    /// Built once: listing tools is the first thing every client does.
    tools: Arc<[rmcp::model::Tool]>,
}

impl<M, I, E, T> Clone for Server<M, I, E, T> {
    fn clone(&self) -> Self {
        Self {
            kit: self.kit.clone(),
            tools: self.tools.clone(),
        }
    }
}

impl<M: Module, I: Identity, E: Entitlements, T: Metering> Server<M, I, E, T> {
    pub(crate) fn new(kit: Kit<M, I, E, T>) -> Self {
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

impl<M: Module, I: Identity, E: Entitlements, T: Metering> ServerHandler for Server<M, I, E, T> {
    fn get_info(&self) -> ServerConfig {
        let manifest = self.kit.manifest();
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build()).with_server_info(
            Implementation::new(manifest.name, manifest.contract_version),
        )
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
        let result = match self.kit.call(&request.name, bearer, arguments).await {
            Ok(value) => CallToolResult::structured(value),
            // Protocol errors for what no caller should retry as is...
            Err(CallError::Unidentified) => {
                return Err(ErrorData::invalid_request(
                    "the caller is not identified",
                    None,
                ));
            }
            Err(error @ CallError::Unknown(_)) => {
                return Err(ErrorData::invalid_params(error.to_string(), None));
            }
            // ...and tool errors for what the model should read and act on.
            Err(error) => CallToolResult::error(vec![ContentBlock::text(error.to_string())]),
        };
        Ok(result.into())
    }
}

/// The request's bearer token: stdio carries none.
fn bearer(_context: &RequestContext<RoleServer>) -> Option<&str> {
    None
}

impl<M: Module, I: Identity, E: Entitlements, T: Metering> Kit<M, I, E, T> {
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
