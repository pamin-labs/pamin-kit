//! The kit under concurrent load: many tasks on a multi-threaded runtime
//! calling one kit at once, as a hosted service is called. Shows contention
//! the single-threaded `call` benchmark cannot: locks and shared atomics on
//! the call path.
//!
//! `cargo bench --bench concurrent`

use std::sync::Arc;
use std::time::Instant;

use pamin_kit::{Kit, Local};
use serde_json::json;

#[path = "../examples/echo/main.rs"]
mod echo;

const TASKS: usize = 256;
const CALLS: usize = 2_000;

fn main() {
    let threads = std::thread::available_parallelism().map_or(4, usize::from);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(threads)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let kit = Arc::new(Kit::new(echo::Echo, Local::new("bench")));
        let arguments = json!({"text": "hi"}).as_object().unwrap().clone();
        let started = Instant::now();
        let workers: Vec<_> = (0..TASKS)
            .map(|_| {
                let kit = kit.clone();
                let arguments = arguments.clone();
                tokio::spawn(async move {
                    let mut nanos = Vec::with_capacity(CALLS);
                    for _ in 0..CALLS {
                        let arguments = arguments.clone();
                        let call = Instant::now();
                        let result = kit.call("echo_say", None, arguments).await;
                        nanos.push(call.elapsed().as_nanos() as u64);
                        assert!(result.is_ok());
                    }
                    nanos
                })
            })
            .collect();
        let mut nanos = Vec::with_capacity(TASKS * CALLS);
        for worker in workers {
            nanos.extend(worker.await.unwrap());
        }
        let elapsed = started.elapsed();
        nanos.sort_unstable();
        let total = nanos.len();
        let at = |q: f64| nanos[((total as f64 * q) as usize).min(total - 1)] as f64 / 1000.0;
        println!(
            "{total} calls from {TASKS} tasks on {threads} threads: {:.0} calls/s; \
             per call p50 {:.2} µs, p95 {:.2} µs, p99 {:.2} µs",
            total as f64 / elapsed.as_secs_f64(),
            at(0.50),
            at(0.95),
            at(0.99)
        );
    });
}
