//! Deliverable and transcript-tail extraction from session records:
//! bounded scans over run content — hostile bytes degrade, never panic.

use craft_core::types::ChatMessage;

use super::{DELIVERABLES_CAP, FINAL_MESSAGE_CAP};

const URL_PREFIX: &str = "https://github.com/";
/// Owner/repo component cap; real GitHub slugs are far shorter.
const SLUG_CAP: usize = 100;
/// Pull-request number digit cap.
const NUMBER_CAP: usize = 9;

/// The final assistant text, capped for display; a run that ended
/// mid-turn has no final message.
pub fn final_message(messages: &[ChatMessage]) -> Option<&str> {
    let last = messages
        .iter()
        .rev()
        .find(|message| matches!(message, ChatMessage::Assistant { .. }))?;
    match last {
        ChatMessage::Assistant {
            text, tool_calls, ..
        } if tool_calls.is_empty() && !text.is_empty() => Some(cap_chars(text, FINAL_MESSAGE_CAP)),
        _ => None,
    }
}

/// Pull-request links and branch-push targets the run's own content
/// surfaced, deduped in order and capped.
pub fn deliverables(messages: &[ChatMessage]) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for message in messages {
        if found.len() >= DELIVERABLES_CAP {
            break;
        }
        let text = match message {
            ChatMessage::Tool { content, .. } => content,
            ChatMessage::Assistant { text, .. } => text,
            _ => continue,
        };
        scan_pr_urls(text, &mut found);
        scan_push_targets(text, &mut found);
    }
    found
}

/// The newest messages as one bounded preview; `cap` counts characters.
/// The newest content always survives — the cap eats the oldest.
pub fn transcript_tail(messages: &[ChatMessage], cap: usize) -> Option<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut used = 0usize;
    for message in messages.iter().rev() {
        let remaining = cap.saturating_sub(used);
        if remaining <= 1 {
            break;
        }
        let line = render_message(message, remaining);
        let count = line.chars().count();
        if count + 1 > remaining {
            let trimmed = cap_chars(&line, remaining - 1).to_string();
            if trimmed.is_empty() {
                break;
            }
            lines.push(trimmed);
            break;
        }
        used += count + 1;
        lines.push(line);
    }
    if lines.is_empty() {
        return None;
    }
    lines.reverse();
    let joined = lines.join("\n");
    Some(cap_chars(&joined, cap).to_string())
}

/// Prefix of at most `cap` characters, cut on a character boundary.
pub fn cap_chars(text: &str, cap: usize) -> &str {
    match text.char_indices().nth(cap) {
        Some((idx, _)) => &text[..idx],
        None => text,
    }
}

fn render_message(message: &ChatMessage, cap: usize) -> String {
    match message {
        ChatMessage::User { text, .. } => format!("[user] {}", cap_chars(text, cap)),
        ChatMessage::Assistant {
            text, tool_calls, ..
        } if !tool_calls.is_empty() => {
            let names: Vec<&str> = tool_calls.iter().map(|call| call.name.as_str()).collect();
            format!(
                "[assistant → {}] {}",
                cap_chars(&names.join(", "), SLUG_CAP),
                cap_chars(text, cap)
            )
        }
        ChatMessage::Assistant { text, .. } => format!("[assistant] {}", cap_chars(text, cap)),
        ChatMessage::Tool {
            content, is_error, ..
        } => {
            let tag = if *is_error { "tool-error" } else { "tool" };
            format!("[{tag}] {}", cap_chars(content, cap))
        }
    }
}

fn scan_pr_urls(text: &str, found: &mut Vec<String>) {
    let mut at = 0;
    while found.len() < DELIVERABLES_CAP {
        let Some(hit) = text[at..].find(URL_PREFIX) else {
            return;
        };
        let start = at + hit;
        match parse_pr_url(&text[start..]) {
            Some((bullet, consumed)) => {
                if !found.contains(&bullet) {
                    found.push(bullet);
                }
                at = start + consumed;
            }
            None => at = start + URL_PREFIX.len(),
        }
    }
}

/// One `https://github.com/<owner>/<repo>/pull/<n>` as a deliverable
/// bullet, plus the bytes it consumed.
fn parse_pr_url(text: &str) -> Option<(String, usize)> {
    let rest = text.strip_prefix(URL_PREFIX)?;
    let (owner, rest) = slug(rest)?;
    let rest = rest.strip_prefix('/')?;
    let (repo, rest) = slug(rest)?;
    let rest = rest.strip_prefix("/pull/")?;
    let (number, rest) = number(rest)?;
    if rest.starts_with(|c: char| c.is_ascii_alphanumeric()) {
        return None;
    }
    let bullet =
        format!("- [PR #{number} ({owner}/{repo})]({URL_PREFIX}{owner}/{repo}/pull/{number})");
    Some((bullet, text.len() - rest.len()))
}

fn scan_push_targets(text: &str, found: &mut Vec<String>) {
    let mut remote: Option<(String, String)> = None;
    for line in text.lines() {
        if found.len() >= DELIVERABLES_CAP {
            return;
        }
        if let Some(slug) = push_remote(line) {
            remote = Some(slug);
            continue;
        }
        let Some((owner, repo)) = &remote else {
            continue;
        };
        if let Some(branch) = push_ref(line) {
            let bullet = format!("- pushed `{owner}/{repo}@{branch}`");
            if !found.contains(&bullet) {
                found.push(bullet);
            }
        }
    }
}

/// The `<owner>/<repo>` a `To <github url>` push header names.
fn push_remote(line: &str) -> Option<(String, String)> {
    let rest = line.trim().strip_prefix("To ")?;
    let at = rest.find("github.com")?;
    let rest = rest.get(at + "github.com".len()..)?;
    let rest = rest.strip_prefix(':').or_else(|| rest.strip_prefix('/'))?;
    let (owner, rest) = slug(rest)?;
    let rest = rest.strip_prefix('/')?;
    let (repo, _) = slug(rest)?;
    let repo = repo.strip_suffix(".git").unwrap_or(repo);
    Some((owner.to_string(), repo.to_string()))
}

/// The destination branch a git push summary row names.
fn push_ref(line: &str) -> Option<&str> {
    let rest =
        line.trim_start_matches(|c: char| c.is_whitespace() || matches!(c, '*' | '+' | '!' | '-'));
    let tail = if let Some(inner) = rest.strip_prefix('[') {
        let close = inner.find(']')?;
        if inner.get(..close)? != "new branch" {
            return None;
        }
        inner.get(close + 1..)?
    } else {
        let (_, rest) = object_id(rest)?;
        let rest = rest.strip_prefix("..")?;
        let (_, tail) = object_id(rest)?;
        tail
    };
    let mut fields = tail.split_whitespace();
    let _source = fields.next()?;
    if fields.next()? != "->" {
        return None;
    }
    let destination = fields.next()?;
    if fields.next().is_some() {
        return None;
    }
    Some(
        destination
            .strip_prefix("refs/heads/")
            .unwrap_or(destination),
    )
}

fn is_slug_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
}

/// One bounded GitHub slug component: the slug and the remainder.
fn slug(text: &str) -> Option<(&str, &str)> {
    let bytes = text.as_bytes();
    let mut n = 0;
    while n < bytes.len() && n < SLUG_CAP && is_slug_byte(bytes[n]) {
        n += 1;
    }
    if n == 0 {
        return None;
    }
    Some((text.get(..n)?, text.get(n..)?))
}

/// One bounded pull-request number: the digits and the remainder.
fn number(text: &str) -> Option<(&str, &str)> {
    let bytes = text.as_bytes();
    let mut n = 0;
    while n < bytes.len() && n < NUMBER_CAP && bytes[n].is_ascii_digit() {
        n += 1;
    }
    if n == 0 || (n == NUMBER_CAP && bytes.get(n).is_some_and(|byte| byte.is_ascii_digit())) {
        return None;
    }
    Some((text.get(..n)?, text.get(n..)?))
}

/// One bounded hexadecimal object id: the id and the remainder.
fn object_id(text: &str) -> Option<(&str, &str)> {
    let bytes = text.as_bytes();
    let mut n = 0;
    while n < bytes.len() && n < 40 && bytes[n].is_ascii_hexdigit() {
        n += 1;
    }
    if n < 7 || (n == 40 && bytes.get(n).is_some_and(|byte| byte.is_ascii_hexdigit())) {
        return None;
    }
    Some((text.get(..n)?, text.get(n..)?))
}
