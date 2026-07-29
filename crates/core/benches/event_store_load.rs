use std::{
    env, fs,
    sync::Arc,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use devicerail_core::{MemoryEventStore, PendingEvent, SessionEventStore, StartSession};
use devicerail_protocol::{ErrorInfo, TestEventPayload};
use serde_json::{Value, json};

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock")
        .as_millis()
        .try_into()
        .expect("timestamp")
}

fn percentile(sorted: &[u64], percentile: usize) -> u64 {
    let index = sorted
        .len()
        .saturating_mul(percentile)
        .div_ceil(100)
        .saturating_sub(1)
        .min(sorted.len().saturating_sub(1));
    sorted[index]
}

#[cfg(target_os = "macos")]
fn peak_rss_bytes() -> Option<u64> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    // SAFETY: getrusage initializes the supplied rusage on success.
    (unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } == 0)
        .then(|| unsafe { usage.assume_init() }.ru_maxrss as u64)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn peak_rss_bytes() -> Option<u64> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    // SAFETY: getrusage initializes the supplied rusage on success.
    (unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } == 0)
        .then(|| (unsafe { usage.assume_init() }.ru_maxrss as u64).saturating_mul(1024))
}

#[cfg(not(unix))]
fn peak_rss_bytes() -> Option<u64> {
    None
}

fn write_result(result: &Value) {
    let encoded = serde_json::to_vec_pretty(result).expect("serialize benchmark result");
    if let Some(path) = env::var_os("DEVICERAIL_PERF_OUTPUT") {
        let path = std::path::PathBuf::from(path);
        let path = if path.is_absolute() {
            path
        } else {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join(path)
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create benchmark result directory");
        }
        fs::write(path, &encoded).expect("write benchmark result");
    }
    println!("{}", String::from_utf8(encoded).expect("JSON is UTF-8"));
}

fn main() {
    let smoke = env::args().any(|argument| argument == "--smoke");
    let sessions = if smoke { 8 } else { 32 };
    let events_per_session = if smoke { 500 } else { 5_000 };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("runtime");

    runtime.block_on(async move {
        let store = Arc::new(MemoryEventStore::default());
        let mut session_ids = Vec::with_capacity(sessions);
        for _ in 0..sessions {
            let start = StartSession::new(None, None, now_ms());
            session_ids.push(start.session_id.clone());
            store.start_session(start).await.expect("start session");
        }

        let started = Instant::now();
        let mut tasks = Vec::with_capacity(sessions);
        for session_id in session_ids.clone() {
            let store = Arc::clone(&store);
            tasks.push(tokio::spawn(async move {
                let mut latencies = Vec::with_capacity(events_per_session);
                for index in 0..events_per_session {
                    let operation = Instant::now();
                    store
                        .append(PendingEvent {
                            session_id: session_id.clone(),
                            request_id: None,
                            device_id: None,
                            at_ms: now_ms(),
                            payload: TestEventPayload::Error {
                                error: ErrorInfo {
                                    code: "load_sample".to_owned(),
                                    message: format!("sample-{index}"),
                                    retryable: false,
                                    details: None,
                                },
                            },
                        })
                        .await
                        .expect("append event");
                    latencies.push(operation.elapsed());
                }
                latencies
            }));
        }
        let mut latencies = Vec::with_capacity(sessions * events_per_session);
        for task in tasks {
            latencies.extend(task.await.expect("append task"));
        }
        let elapsed = started.elapsed();

        let list_started = Instant::now();
        for session_id in &session_ids {
            let page = store
                .list_page(session_id, None, 1_000)
                .await
                .expect("list page");
            assert_eq!(page.len(), 1_000.min(events_per_session + 1));
        }
        let list_elapsed = list_started.elapsed();

        let mut latency_micros = latencies
            .into_iter()
            .map(|duration| duration.as_micros().min(u128::from(u64::MAX)) as u64)
            .collect::<Vec<_>>();
        latency_micros.sort_unstable();
        let operations = sessions * events_per_session;
        let throughput = operations as f64 / elapsed.as_secs_f64();
        write_result(&json!({
            "schemaVersion": 1,
            "benchmark": "memory-event-store-concurrent-append",
            "profile": if smoke { "smoke" } else { "full" },
            "sessions": sessions,
            "eventsPerSession": events_per_session,
            "operations": operations,
            "elapsedMs": elapsed.as_secs_f64() * 1_000.0,
            "throughputOpsPerSecond": throughput,
            "appendLatencyMicros": {
                "p50": percentile(&latency_micros, 50),
                "p95": percentile(&latency_micros, 95),
                "p99": percentile(&latency_micros, 99),
                "max": latency_micros.last().copied().unwrap_or(0),
            },
            "listPagesElapsedMs": list_elapsed.as_secs_f64() * 1_000.0,
            "peakRssBytes": peak_rss_bytes(),
            "target": format!("{}-{}", env::consts::ARCH, env::consts::OS),
        }));
    });
}
