//! Markdown digest assembly.

use crate::bundle::{Bundle, RepoDigest};
use crate::collect::PrRecord;
use crate::error::truncate_chars;
use crate::jscpd::CloneSite;
use crate::window::{Window, WindowSource, marker};

/// Digest output cap; a longer digest is truncated with a note.
pub const DIGEST_CHAR_CAP: usize = 30_000;

/// Assemble the digest; PR titles and bodies appear as quoted data.
pub fn render_digest(bundle: &Bundle) -> String {
    let mut body = digest_header(bundle);
    body.push_str(&merged_prs_section(bundle));
    body.push_str(&analysis_section(bundle));
    body.push_str(&duplicates_section(bundle));
    body.push_str(&errors_section(bundle));
    body.push_str("\n## Usage\n\n_(usage footer is recorded by the agent run)_\n");
    digest_footer(body, &bundle.window)
}

fn digest_header(bundle: &Bundle) -> String {
    let window = &bundle.window;
    let mut body = format!(
        "# {} digest {}..{}\n",
        bundle.org, window.since, window.until
    );
    if matches!(window.source, WindowSource::Fallback) {
        body.push_str("\n_window fallback: 24h_\n");
    }
    body.push_str("\n## Merged PRs\n");
    body
}

fn merged_prs_section(bundle: &Bundle) -> String {
    if bundle.repos.iter().all(|repo| repo.merged_prs.is_empty()) {
        return "\nNo merged PRs in this window.\n".to_string();
    }
    bundle.repos.iter().map(repo_section).collect()
}

fn repo_section(repo: &RepoDigest) -> String {
    let mut body = format!("\n### {} ({})\n", repo.repo, repo.merged_prs.len());
    if repo.merged_prs.is_empty() {
        body.push_str("\n(no merged PRs)\n");
    }
    for pr in &repo.merged_prs {
        body.push_str(&pr_block(pr));
    }
    body
}

fn analysis_section(bundle: &Bundle) -> String {
    match suite_analysis(bundle) {
        "" => "\n## Suite analysis\n\n_(the model turn produced no analysis)_\n".to_string(),
        analysis => format!("\n## Suite analysis\n\n{analysis}\n"),
    }
}

fn duplicates_section(bundle: &Bundle) -> String {
    if bundle.duplicates.is_empty() {
        return String::new();
    }
    let mut body = String::from("\n## Duplicate-code candidates\n");
    for dup in &bundle.duplicates {
        body.push_str(&format!(
            "\n- {} tokens: {} and {}\n",
            dup.tokens,
            site_link(&bundle.org, &dup.first),
            site_link(&bundle.org, &dup.second)
        ));
    }
    body
}

fn errors_section(bundle: &Bundle) -> String {
    if bundle.errors.is_empty() {
        return String::new();
    }
    let mut body = String::from("\n## Collection errors\n");
    for err in &bundle.errors {
        body.push_str(&format!(
            "\n- collection-error: {}: {}: {}\n",
            err.repo, err.stage, err.message
        ));
    }
    body
}

fn digest_footer(mut body: String, window: &Window) -> String {
    if body.chars().count() > DIGEST_CHAR_CAP {
        body = truncate_chars(&body, DIGEST_CHAR_CAP);
        body.push_str(&format!(
            "\n_(digest truncated at {DIGEST_CHAR_CAP} characters)_\n"
        ));
    }
    body.push_str(&format!("\n{}\n", marker(window)));
    body
}

/// One merged PR as a data block: summary line plus a quoted body.
pub(crate) fn pr_block(pr: &PrRecord) -> String {
    let mut out = format!(
        "\n- [#{}]({}) {} - {} (+{}/-{})",
        pr.number, pr.url, pr.title, pr.author, pr.additions, pr.deletions
    );
    if !pr.labels.is_empty() {
        let labels: Vec<String> = pr.labels.iter().map(|l| format!("`{l}`")).collect();
        out.push(' ');
        out.push_str(&labels.join(" "));
    }
    out.push('\n');
    if !pr.body.is_empty() {
        for line in pr.body.split('\n') {
            if line.is_empty() {
                out.push_str("  >\n");
            } else {
                out.push_str(&format!("  > {line}\n"));
            }
        }
    }
    out
}

fn site_link(org: &str, site: &CloneSite) -> String {
    let url = format!(
        "https://github.com/{org}/{}/blob/HEAD/{}#L{}",
        site.repo, site.file, site.line
    );
    format!("[{}/{}:{}]({url})", site.repo, site.file, site.line)
}

/// The model turn's answer, mirrored into every repo row; rendered once.
fn suite_analysis(bundle: &Bundle) -> &str {
    for repo in &bundle.repos {
        if !repo.agents_excerpt.is_empty() {
            return &repo.agents_excerpt;
        }
    }
    ""
}
