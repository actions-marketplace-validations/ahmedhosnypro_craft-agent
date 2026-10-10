//! The per-run truncation policy: the cap pass that replaces every
//! over-cap tool result with its head plus a marker, and the spill
//! writer that parks the tail under the runner's temp mount.

use std::path::{Path, PathBuf};

use craft_core::error::ProviderError;
use craft_core::types::ChatMessage;
use std::sync::Arc;

use super::{result_truncate_bytes_from_env, session_stem, spill_root_from_env};
use crate::runner::RunConfig;

/// One spilled tail never grows past this; the bound guards future
/// tools and hostile content.
const MAX_SPILL_FILE_BYTES: usize = 8 * 1024 * 1024;

/// Total new spill bytes one run may write.
const MAX_SPILL_TOTAL_BYTES: u64 = 64 * 1024 * 1024;

const SPILL_DIR: &str = "craft-agent-spill";

/// The per-run truncation policy: the byte cap, the resolved spill
/// root, and the run's running spill total.
pub(crate) struct Truncate {
    cap: u64,
    root: PathBuf,
    stem: String,
    written: u64,
    max_total: u64,
}

/// Resolves the policy from the config and the environment; a hostile
/// knob fails the run here, before turn one.
pub(crate) fn resolve(config: &RunConfig) -> Result<Truncate, ProviderError> {
    let cap = match config.result_truncate_bytes {
        Some(cap) => cap,
        None => result_truncate_bytes_from_env()?,
    };
    Ok(Truncate {
        cap,
        root: spill_root_from_env(config.spill_root.clone()),
        stem: session_stem(config.session_path.as_deref()),
        written: 0,
        max_total: MAX_SPILL_TOTAL_BYTES,
    })
}

/// The messages for the next request with every over-cap result
/// reduced to its head plus a marker; trim runs first, so a stub an
/// earlier pass left is never capped a second time. Sharing is by
/// pointer: only an over-cap entry is newly allocated.
pub(crate) async fn cap(
    messages: Vec<Arc<ChatMessage>>,
    policy: &mut Truncate,
) -> Vec<Arc<ChatMessage>> {
    if policy.cap == 0 {
        return messages;
    }
    let mut capped = Vec::with_capacity(messages.len());
    for (index, message) in messages.into_iter().enumerate() {
        capped.push(policy.cap_message(index, message).await);
    }
    capped
}

impl Truncate {
    /// One message through the cap: non-tool and at-cap messages ride
    /// unchanged; an over-cap result rides a fresh head-plus-marker
    /// message.
    async fn cap_message(&mut self, index: usize, message: Arc<ChatMessage>) -> Arc<ChatMessage> {
        let ChatMessage::Tool {
            call_id,
            content,
            is_error,
            images,
        } = &*message
        else {
            return message;
        };
        let Some(capped) = self.cap_content(index, content).await else {
            return message;
        };
        Arc::new(ChatMessage::Tool {
            call_id: call_id.clone(),
            content: capped,
            is_error: *is_error,
            images: images.clone(),
        })
    }

    /// None rides the content verbatim; Some replaces it with the
    /// head plus a marker naming where the tail went.
    async fn cap_content(&mut self, index: usize, content: &str) -> Option<String> {
        if content.len() as u64 <= self.cap {
            return None;
        }
        let cap = usize::try_from(self.cap).unwrap_or(usize::MAX);
        let head = craft_tools::util::truncate_char_boundary(content, cap);
        let marker = match self.spill(index, content).await {
            Some(path) => with_spill_marker(head.len(), content.len(), &path),
            None => without_spill_marker(head.len(), content.len()),
        };
        Some(format!("{head}\n{marker}"))
    }

    /// The tail's spill path, or None when this tail does not spill:
    /// over the per-file bound, over the run's remaining spill
    /// budget, or storage that cannot be written. Never an error —
    /// the run outlives its scratch mount. Payloads at or above the
    /// offload bound write on a writer thread so the executor keeps
    /// driving in-flight work.
    async fn spill(&mut self, index: usize, content: &str) -> Option<PathBuf> {
        if self.spill_denied(content.len()) {
            return None;
        }
        let dir = self.root.join(SPILL_DIR).join(&self.stem);
        let path = dir.join(format!("result-{index:06}.txt"));
        if spill_matches(&path, content.len()) {
            return Some(path);
        }
        let part = dir.join(format!("result-{index:06}.txt.part"));
        if !self.write_tail(&part, &path, content).await {
            return None;
        }
        Some(path)
    }

    /// Whether the bounds refuse this tail outright.
    fn spill_denied(&self, len: usize) -> bool {
        len > MAX_SPILL_FILE_BYTES || self.written.saturating_add(len as u64) > self.max_total
    }

    /// Writes the tail through the bounded writer and books the bytes
    /// on success.
    async fn write_tail(&mut self, part: &Path, path: &Path, content: &str) -> bool {
        let part = part.to_path_buf();
        let path = path.to_path_buf();
        let payload = String::from(content);
        let result = crate::session::write_bounded(content.len(), move || {
            write_spill(&part, &path, &payload)
        })
        .await;
        if result.is_err() {
            return false;
        }
        self.written = self.written.saturating_add(content.len() as u64);
        true
    }
}

/// A spilled tail of the same size is the same content (this run's
/// writes are whole-content); a stale or tampered file is rewritten.
fn spill_matches(path: &Path, len: usize) -> bool {
    std::fs::metadata(path)
        .map(|meta| meta.len() == len as u64)
        .unwrap_or(false)
}

/// Writes the tail whole then renames it into place; a failed write
/// removes the partial file so a later run never reuses its size.
fn write_spill(part: &Path, path: &Path, payload: &str) -> std::io::Result<()> {
    if let Some(dir) = part.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let result = std::fs::write(part, payload).and_then(|()| std::fs::rename(part, path));
    if result.is_err() {
        let _ = std::fs::remove_file(part);
    }
    result
}

fn with_spill_marker(kept: usize, total: usize, path: &Path) -> String {
    let shown = path.display();
    format!(
        "[tool result truncated: kept first {kept} of {total} bytes; the full result is saved at {shown}; read it back in ranges with bash, e.g. sed -n '1,80p' {shown}]"
    )
}

fn without_spill_marker(kept: usize, total: usize) -> String {
    format!(
        "[tool result truncated: kept first {kept} of {total} bytes; the tail is not recoverable in this run — re-derive it if needed]"
    )
}

#[cfg(test)]
mod tests;
