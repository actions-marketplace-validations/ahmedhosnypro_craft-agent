use super::*;
use std::sync::atomic::{AtomicU32, Ordering};

fn tool(call_id: &str, content: &str, is_error: bool) -> Arc<ChatMessage> {
    Arc::new(ChatMessage::Tool {
        call_id: String::from(call_id),
        content: String::from(content),
        is_error,
        images: Vec::new(),
    })
}

fn scratch(tag: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/tmp");
    let _ = std::fs::create_dir_all(&dir);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    dir.join(format!("truncate-{tag}-{}-{n}", std::process::id()))
}

/// A file-shaped path: any spill write under it fails with ENOTDIR.
fn blocked(tag: &str) -> PathBuf {
    let path = scratch(tag);
    std::fs::write(&path, b"blocked").unwrap();
    path
}

fn policy_at(cap: u64, root: PathBuf) -> Truncate {
    Truncate {
        cap,
        root,
        stem: String::from("stem"),
        written: 0,
        max_total: MAX_SPILL_TOTAL_BYTES,
    }
}

fn content_of(message: &Arc<ChatMessage>) -> &str {
    let ChatMessage::Tool { content, .. } = message.as_ref() else {
        panic!("expected a tool message, got {message:?}");
    };
    content
}

fn spill_file(root: &Path) -> PathBuf {
    root.join(SPILL_DIR).join("stem").join("result-000000.txt")
}

#[tokio::test]
async fn at_cap_content_rides_verbatim_and_one_more_byte_truncates() {
    let mut policy = policy_at(4, scratch("atcap"));
    let at_cap = vec![tool("c1", "abcd", false)];
    assert_eq!(cap(at_cap.clone(), &mut policy).await, at_cap);

    let capped = cap(vec![tool("c1", "abcde", false)], &mut policy).await;
    let path = spill_file(&policy.root);
    assert_eq!(
        content_of(&capped[0]),
        &format!(
            "abcd\n[tool result truncated: kept first 4 of 5 bytes; the full result is saved at {}; read it back in ranges with bash, e.g. sed -n '1,80p' {}]",
            path.display(),
            path.display()
        )
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "abcde");
}

#[tokio::test]
async fn the_head_cut_backs_off_to_a_char_boundary() {
    let mut policy = policy_at(5, blocked("boundary"));
    let capped = cap(vec![tool("c1", "ééé", false)], &mut policy).await;
    assert!(
        content_of(&capped[0]).starts_with("éé\n[tool result truncated: kept first 4 of 6 bytes;"),
        "the head is the largest boundary prefix of the 6-byte payload"
    );
}

#[tokio::test]
async fn markers_count_bytes_not_characters() {
    let mut policy = policy_at(4, blocked("bytes"));
    let capped = cap(vec![tool("c1", "héllo…", false)], &mut policy).await;
    let content = content_of(&capped[0]);
    assert!(
        content.starts_with("hél\n"),
        "the head is h + é + l: 4 bytes"
    );
    assert!(
        content.contains("kept first 4 of 9 bytes"),
        "héllo… is 9 bytes and 6 characters: {content}"
    );
}

#[tokio::test]
async fn the_with_spill_marker_names_path_kept_and_total() {
    let root = scratch("markers");
    let mut policy = policy_at(4, root.clone());
    let capped = cap(vec![tool("c1", "abcde", false)], &mut policy).await;
    let path = spill_file(&root);
    assert_eq!(
        content_of(&capped[0]),
        &format!(
            "abcd\n[tool result truncated: kept first 4 of 5 bytes; the full result is saved at {}; read it back in ranges with bash, e.g. sed -n '1,80p' {}]",
            path.display(),
            path.display()
        )
    );

    let mut denied = policy_at(4, blocked("markers"));
    let capped = cap(vec![tool("c1", "abcde", false)], &mut denied).await;
    assert_eq!(
        content_of(&capped[0]),
        "abcd\n[tool result truncated: kept first 4 of 5 bytes; the tail is not recoverable in this run — re-derive it if needed]"
    );
}

#[tokio::test]
async fn spill_write_failure_downgrades_to_the_no_spill_marker() {
    let mut policy = policy_at(4, blocked("downgrade").join("spill"));
    let capped = cap(vec![tool("c1", "abcde", false)], &mut policy).await;
    let content = content_of(&capped[0]);
    assert!(content.starts_with("abcd\n[tool result truncated: kept first 4 of 5 bytes;"));
    assert!(content.contains("not recoverable"), "{content}");
    assert!(!content.contains("saved at"), "{content}");
}

#[tokio::test]
async fn spill_writes_once_and_is_reused() {
    let root = scratch("reuse");
    let mut policy = policy_at(4, root.clone());
    cap(vec![tool("c1", "abcde", false)], &mut policy).await;
    let path = spill_file(&root);
    std::fs::write(&path, "SAME!").unwrap();

    let again = cap(vec![tool("c1", "abcde", false)], &mut policy).await;
    assert!(
        content_of(&again[0]).contains("saved at"),
        "the existing file is reused as the pointer"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "SAME!");

    std::fs::write(&path, "WRONG-LENGTH").unwrap();
    cap(vec![tool("c1", "abcde", false)], &mut policy).await;
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "abcde",
        "a wrong-size stale file is rewritten"
    );
}

#[tokio::test]
async fn the_aggregate_ceiling_stops_spilling_but_never_capping() {
    let root = scratch("ceiling");
    let mut policy = policy_at(4, root.clone());
    policy.max_total = 5;
    let messages = vec![tool("c1", "abcde", false), tool("c2", "fghij", false)];
    let capped = cap(messages, &mut policy).await;
    assert!(
        content_of(&capped[0]).contains("saved at"),
        "the first tail fits the budget"
    );
    assert!(
        content_of(&capped[1]).contains("not recoverable"),
        "the second tail exceeds the budget and caps without a pointer"
    );
    assert_eq!(
        std::fs::read_to_string(spill_file(&root)).unwrap(),
        "abcde",
        "earlier files stand"
    );
}

#[tokio::test]
async fn results_over_the_per_file_guard_never_spill() {
    let root = scratch("perfile");
    let mut policy = policy_at(4, root.clone());
    let huge = "x".repeat(MAX_SPILL_FILE_BYTES + 1);
    let capped = cap(vec![tool("c1", &huge, false)], &mut policy).await;
    assert!(
        content_of(&capped[0]).contains("not recoverable"),
        "no pointer for a tail over the per-file guard"
    );
    assert!(
        !root.join(SPILL_DIR).exists(),
        "the guard fires before any write"
    );
}

#[tokio::test]
async fn error_results_keep_their_flag_when_truncated() {
    let mut policy = policy_at(4, blocked("flag"));
    let capped = cap(vec![tool("c1", "abcde", true)], &mut policy).await;
    assert!(matches!(
        capped[0].as_ref(),
        ChatMessage::Tool { is_error, content, .. }
        if *is_error && content.starts_with("abcd\n[tool result truncated: kept first 4 of 5 bytes;")
    ));
}
