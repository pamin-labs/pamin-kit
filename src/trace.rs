//! One span per MCP request, named and attributed as OpenTelemetry's MCP
//! conventions have it. With the `otel` feature it joins the caller's trace
//! from the request's `_meta` (`traceparent`, `tracestate`), so a gateway,
//! the kit and the module share one trace. The binary installs the exporter
//! and the propagator; the kit only reads the context.

use rmcp::ErrorData;
use rmcp::service::{RequestContext, RoleServer};
use tracing::Span;

/// The span of one request for `method` on `target` (a tool or a task).
pub(crate) fn request(
    method: &'static str,
    target: &str,
    context: &RequestContext<RoleServer>,
) -> Span {
    let span = tracing::info_span!(
        "mcp",
        otel.name = %format_args!("{method} {target}"),
        otel.kind = "server",
        mcp.method.name = method,
        jsonrpc.request.id = %context.id,
        gen_ai.operation.name = tracing::field::Empty,
        gen_ai.tool.name = tracing::field::Empty,
        error.type = tracing::field::Empty,
    );
    if method == "tools/call" {
        span.record("gen_ai.operation.name", "execute_tool");
        span.record("gen_ai.tool.name", target);
    }
    #[cfg(feature = "otel")]
    join(&span, &context.meta.0.0);
    span
}

/// Records a protocol error's JSON-RPC code as the span's `error.type`.
pub(crate) fn outcome<V>(span: &Span, result: &Result<V, ErrorData>) {
    if let Err(error) = result {
        span.record("error.type", error.code.0);
    }
}

/// Records a tool error, which MCP returns as a result, on the current span.
pub(crate) fn tool_error() {
    Span::current().record("error.type", "tool_error");
}

#[cfg(feature = "otel")]
fn join(span: &Span, meta: &rmcp::model::JsonObject) {
    use tracing_opentelemetry::OpenTelemetrySpanExt;

    struct Carrier<'a>(&'a rmcp::model::JsonObject);

    impl opentelemetry::propagation::Extractor for Carrier<'_> {
        fn get(&self, key: &str) -> Option<&str> {
            self.0.get(key)?.as_str()
        }

        fn keys(&self) -> Vec<&str> {
            self.0.keys().map(String::as_str).collect()
        }
    }

    let parent = opentelemetry::global::get_text_map_propagator(|propagator| {
        propagator.extract(&Carrier(meta))
    });
    // Fails only when no OpenTelemetry layer is installed, and then there is
    // no trace to join.
    let _ = span.set_parent(parent);
}
