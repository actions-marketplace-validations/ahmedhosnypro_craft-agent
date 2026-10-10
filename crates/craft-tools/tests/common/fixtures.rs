//! Workspace and fixture builders: throwaway directories, file
//! writes, and the fake `gh` binary.

#![allow(dead_code)]
// Corpus fixtures: expect() is the loud-test idiom here.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicUsize, Ordering};

// Parallel tests share the pid; the sequence keeps every call's path unique.
static WS_SEQ: AtomicUsize = AtomicUsize::new(0);

pub fn tmp_ws(name: &str) -> PathBuf {
    let seq = WS_SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/tmp/craft-tools-corpus")
        .join(format!("{}-{}-{}", name, process::id(), seq));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("workspace create");
    fs::canonicalize(&dir).unwrap_or(dir)
}

pub fn write(ws: &Path, rel: &str, content: &str) {
    let path = ws.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("parent create");
    }
    fs::write(&path, content).expect("fixture write");
}

pub fn write_bytes(ws: &Path, rel: &str, content: &[u8]) {
    let path = ws.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("parent create");
    }
    fs::write(&path, content).expect("fixture write");
}

/// A fake `gh` on PATH, driven by FAKE_GH_MODE from the (scrubbed) child env.
pub fn fake_gh(dir: &Path, body: &str) -> PathBuf {
    let script = format!("#!/bin/sh\n{body}\n");
    let path = dir.join("gh");
    fs::write(&path, script).expect("fake gh write");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("fake gh chmod");
    fs::File::open(&path)
        .and_then(|f| f.sync_all())
        .expect("fake gh sync");
    dir.to_path_buf()
}

/// Explicit env for the child: fake keys to prove scrubbing, plus PATH.
pub fn fake_child_env(extra_path: Option<PathBuf>) -> Vec<(String, String)> {
    let path = match extra_path {
        Some(p) => format!(
            "{}:{}",
            p.display(),
            std::env::var("PATH").unwrap_or_default()
        ),
        None => std::env::var("PATH").unwrap_or_default(),
    };
    vec![
        ("PATH".to_string(), path),
        ("HOME".to_string(), "/tmp".to_string()),
        ("OPENAI_API_KEY".to_string(), "sk-fake-openai".to_string()),
        (
            "ANTHROPIC_API_KEY".to_string(),
            "sk-fake-anthropic".to_string(),
        ),
        ("GOOGLE_API_KEY".to_string(), "fake-google".to_string()),
        ("STRIPE_API_KEY".to_string(), "sk-fake-stripe".to_string()),
        ("API_KEY".to_string(), "bare-fake".to_string()),
        ("TEST_BYOK_API_KEY".to_string(), "fake-byok".to_string()),
        ("TEST_BYOK_BASE_URL".to_string(), "http://fake".to_string()),
        ("FAKE_GH_MODE".to_string(), "ok".to_string()),
        ("GH_TOKEN".to_string(), "ghp-fake-token".to_string()),
    ]
}
