//! Writer-thread offload corpus: on the single-threaded executor the
//! app runs, session lines and spill payloads at or above the offload
//! bound write on a short-lived writer thread — any in-flight task (a
//! subagent child future, a heartbeat) still polls while a large write
//! runs, and the write itself lands intact and bounded. Both rows run
//! on `current_thread`, the app's flavor, and the heartbeat only ever
//! polls when the run yields, so the rows are deterministic.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use common::*;
use craft_core::types::ChatMessage;
use craft_runtime::session::Session;
use craft_runtime::{RunConfig, Runner};

/// Counts every poll while the workload holds the executor.
fn heartbeat() -> (Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    let progress = Arc::new(AtomicUsize::new(0));
    let task = tokio::spawn({
        let progress = progress.clone();
        async move {
            loop {
                progress.fetch_add(1, Ordering::Relaxed);
                tokio::task::yield_now().await;
            }
        }
    });
    (progress, task)
}

fn payload(len: usize) -> String {
    "x".repeat(len)
}

/// A fresh spill root under the workspace's ignored target/tmp.
fn spill_root(tag: &str) -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/tmp");
    let _ = std::fs::create_dir_all(&dir);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    dir.join(format!("spill-offload-{tag}-{}-{n}", std::process::id()))
}

#[tokio::test(flavor = "current_thread")]
async fn a_large_append_lets_in_flight_tasks_progress() {
    let path = session_path("big-append");
    let mut session = Session::open(path.clone()).expect("open");
    let (progress, heartbeat) = heartbeat();
    let big = ChatMessage::Tool {
        call_id: String::from("c1"),
        content: payload(3 * 1024 * 1024),
        is_error: false,
        images: Vec::new(),
    };
    session.append(big).await.expect("append");
    heartbeat.abort();
    assert!(
        progress.load(Ordering::Relaxed) > 0,
        "the in-flight task polled while the large write ran"
    );
    let reloaded = Session::load(&path).expect("the record reloads");
    assert!(
        matches!(&reloaded[..], [ChatMessage::Tool { content, .. }] if content.len() == 3 * 1024 * 1024),
        "the written line round-trips"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_large_spill_lets_in_flight_tasks_progress() {
    let root = spill_root("run");
    let session = session_path("offload");
    let mut cfg = RunConfig::new(PathBuf::from("."));
    cfg.step_attempts = Some(1);
    cfg.result_truncate_bytes = Some(4096);
    cfg.spill_root = Some(root.clone());
    cfg.session_path = Some(session.clone());
    let big = payload(300 * 1024);
    let (progress, heartbeat) = heartbeat();
    let provider = FakeProvider::new(vec![
        tool_turn("c1", "scripted", "{}", usage(1, 1)),
        text_turn("done", craft_core::types::StopReason::EndTurn, usage(1, 1)),
    ]);
    let runner = Runner::new(provider, registry_with_ok("scripted", &big), cfg);
    let (outcome, _) = run_collect(&runner, "work").await;
    heartbeat.abort();
    assert_eq!(outcome.exit_code(), 0);
    assert!(
        progress.load(Ordering::Relaxed) > 0,
        "the in-flight task polled while the large spill ran"
    );
    let spilled = root
        .join("craft-agent-spill")
        .join(session.file_stem().unwrap_or_default())
        .join("result-000002.txt");
    assert_eq!(
        std::fs::read_to_string(&spilled).unwrap(),
        big,
        "the spill file carries the full result"
    );
}
