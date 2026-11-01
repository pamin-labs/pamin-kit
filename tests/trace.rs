//! A request's span joins the caller's OpenTelemetry trace. Its own test
//! binary: a scoped subscriber shares the global callsite cache with every
//! other test running at the same time.
#![cfg(all(feature = "http", feature = "otel"))]

use serde_json::json;

mod common;
use common::{call, rpc, serve};

#[tokio::test]
async fn a_call_joins_the_callers_trace() {
    use opentelemetry::trace::TracerProvider as _;
    use opentelemetry_sdk::propagation::TraceContextPropagator;
    use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
    use tracing_subscriber::layer::SubscriberExt;

    opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let subscriber = tracing_subscriber::registry()
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")));
    // This test's runtime has one thread, which the server runs on too.
    let _guard = tracing::subscriber::set_default(subscriber);

    let address = serve().await;
    let trace = "4bf92f3577b34da6a3ce929d0e0e4736";
    let mut say = call("echo_say", json!({"text": "hi"}));
    say["_meta"] = json!({"traceparent": format!("00-{trace}-00f067aa0ba902b7-01")});
    let (status, said) = rpc(address, "ada", false, "tools/call", say).await;
    assert_eq!(status, 200, "{said}");

    provider.force_flush().unwrap();
    let spans = exporter.get_finished_spans().unwrap();
    let span = spans
        .iter()
        .find(|span| span.name == "tools/call echo_say")
        .unwrap_or_else(|| panic!("no tools/call span in {spans:?}"));
    assert_eq!(span.span_context.trace_id().to_string(), trace);
    assert_eq!(span.span_kind, opentelemetry::trace::SpanKind::Server);
    let tool = span
        .attributes
        .iter()
        .find(|attribute| attribute.key.as_str() == "gen_ai.tool.name");
    assert_eq!(tool.unwrap().value.as_str(), "echo_say");
}
