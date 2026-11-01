//! Per-call overhead of the kit: identity, lookup, entitlement check,
//! argument parsing, dispatch, result serialization and metering, around an
//! operation that does almost nothing. Its p95 is an upper bound on the
//! kit's own share, and the specification's gate is 50 µs.
//!
//! `cargo bench --bench call`

use std::time::Instant;

use pamin_kit::{Kit, Local};
use serde_json::json;

#[path = "../examples/echo/main.rs"]
mod echo;

const CALLS: usize = 100_000;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let kit = Kit::new(echo::Echo, Local::new("bench"));
    let arguments = json!({"text": "hi"}).as_object().unwrap().clone();
    let mut nanos = Vec::with_capacity(CALLS);
    for _ in 0..CALLS {
        let arguments = arguments.clone();
        let started = Instant::now();
        let result = kit.call("echo_say", None, arguments).await;
        nanos.push(started.elapsed().as_nanos() as u64);
        assert!(result.is_ok());
    }
    nanos.sort_unstable();
    let at = |q: f64| nanos[((CALLS as f64 * q) as usize).min(CALLS - 1)] as f64 / 1000.0;
    println!(
        "kit.call over {CALLS} calls: p50 {:.2} µs, p95 {:.2} µs, p99 {:.2} µs",
        at(0.50),
        at(0.95),
        at(0.99)
    );
}
