//! Collection over the pulls REST surface; a repo failure is a row.

use serde::Serialize;

use crate::Truncation;
use crate::error::{HelperError, bounded};
use crate::fetch::{Fetch, FetchResponse};
use crate::prs::{
    FileRow, MAX_FILE_ROWS, PullRow, parse_file_rows, parse_pull_detail, parse_pull_page,
};
use crate::time::parse_rfc3339_secs;

pub const API_BASE: &str = "https://api.github.com";
pub const PER_PAGE: usize = 100;
/// Pages fetched per repository, however full they come back.
pub const MAX_PAGES: usize = 5;
/// `--limit-per-repo` ceiling.
pub const MAX_LIMIT_PER_REPO: usize = 100;
/// `--max-prs` ceiling.
pub const MAX_MAX_PRS: usize = 500;
pub const TOKEN_ENV: &str = "GH_TOKEN";

pub fn pulls_page_url(org: &str, repo: &str, page: usize) -> String {
    format!(
        "{API_BASE}/repos/{org}/{repo}/pulls?state=closed&sort=updated&direction=desc&per_page={PER_PAGE}&page={page}"
    )
}

pub fn pull_detail_url(org: &str, repo: &str, number: u64) -> String {
    format!("{API_BASE}/repos/{org}/{repo}/pulls/{number}")
}

pub fn pull_files_url(org: &str, repo: &str, number: u64) -> String {
    format!("{API_BASE}/repos/{org}/{repo}/pulls/{number}/files?per_page={MAX_FILE_ROWS}")
}

/// The bearer token from an injected environment; missing or blank is a
/// typed error naming the variable. The value itself is never echoed.
pub fn resolve_token(lookup: impl Fn(&str) -> Option<String>) -> Result<String, HelperError> {
    let token = lookup(TOKEN_ENV).ok_or(HelperError::MissingToken)?;
    let token = token.trim();
    if token.is_empty() {
        return Err(HelperError::MissingToken);
    }
    Ok(token.to_string())
}

#[derive(Debug, Clone)]
pub struct PrsQuery {
    pub org: String,
    pub repos: Vec<String>,
    pub now_secs: u64,
    pub lookback_secs: u64,
    pub limit_per_repo: usize,
    pub max_prs: usize,
    pub since_label: String,
}

#[derive(Debug, Serialize)]
pub struct PrsReport {
    pub org: String,
    pub since: String,
    pub repos: Vec<RepoReport>,
    pub errors: Vec<ErrorRow>,
    pub truncated: Vec<Truncation>,
}

#[derive(Debug, Serialize)]
pub struct RepoReport {
    pub repo: String,
    pub prs: Vec<PrsEntry>,
}

#[derive(Debug, Serialize)]
pub struct PrsEntry {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub author: String,
    pub merged_at: String,
    pub additions: u64,
    pub deletions: u64,
    pub changed_files: Vec<FileRow>,
}

#[derive(Debug, Serialize)]
pub struct ErrorRow {
    pub repo: String,
    pub message: String,
}
/// Shared collection context: transport, bounds, and truncation notes.
struct Collect<'a> {
    fetch: &'a dyn Fetch,
    query: &'a PrsQuery,
    budget: usize,
    truncated: Vec<Truncation>,
}

/// Collect every repo; failures become error rows, never a fatal Err.
pub fn collect_prs(fetch: &dyn Fetch, query: &PrsQuery) -> PrsReport {
    let cutoff = query.now_secs.saturating_sub(query.lookback_secs);
    let mut report = PrsReport {
        org: query.org.clone(),
        since: query.since_label.clone(),
        repos: Vec::new(),
        errors: Vec::new(),
        truncated: Vec::new(),
    };
    let mut collect = Collect {
        fetch,
        query,
        budget: query.max_prs,
        truncated: Vec::new(),
    };
    for repo in &query.repos {
        let (prs, messages) = collect.repo(repo, cutoff);
        for message in messages {
            report.errors.push(ErrorRow {
                repo: repo.clone(),
                message: bounded(&message),
            });
        }
        report.repos.push(RepoReport {
            repo: repo.clone(),
            prs,
        });
    }
    report.truncated = collect.truncated;
    report
}

fn ensure_ok(response: &FetchResponse) -> Result<(), HelperError> {
    if response.status == 403 || response.status == 429 {
        return Err(HelperError::RateLimited {
            status: response.status,
            retry_after: response.retry_after.clone(),
        });
    }
    if !(200..300).contains(&response.status) {
        return Err(HelperError::Http(format!("HTTP {}", response.status)));
    }
    Ok(())
}

impl Collect<'_> {
    /// One repository's merged pull requests; every failure is a message.
    fn repo(&mut self, repo: &str, cutoff: u64) -> (Vec<PrsEntry>, Vec<String>) {
        let matched = match self.matched_pulls(repo, cutoff) {
            Ok(matched) => matched,
            Err(err) => return (Vec::new(), vec![err.to_string()]),
        };
        self.note_per_repo_cap(repo, &matched);
        self.entries(repo, matched)
    }

    fn matched_pulls(&self, repo: &str, cutoff: u64) -> Result<MatchedPulls, HelperError> {
        let mut matched: Vec<(u64, PullRow)> = Vec::new();
        let mut more_may_exist = false;
        for page in 1..=MAX_PAGES {
            let response = self
                .fetch
                .get(&pulls_page_url(&self.query.org, repo, page))?;
            ensure_ok(&response)?;
            let row = parse_pull_page(&response.body)?;
            let page_len = row.len();
            let clipped_on_page = self.collect_page(row, cutoff, &mut matched);
            more_may_exist = clipped_on_page || page_len == PER_PAGE;
            if matched.len() >= self.query.limit_per_repo || page_len < PER_PAGE {
                break;
            }
        }
        // Sort by the parsed instant: offset-form timestamps order wrongly as text.
        matched.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.number.cmp(&a.1.number)));
        matched.truncate(self.query.limit_per_repo);
        Ok(MatchedPulls {
            rows: matched,
            more_may_exist,
        })
    }

    /// Keep window-matching pulls up to the per-repo limit; the return
    /// marks a page that held more matches than the limit allowed.
    fn collect_page(
        &self,
        row: Vec<PullRow>,
        cutoff: u64,
        matched: &mut Vec<(u64, PullRow)>,
    ) -> bool {
        let mut clipped_on_page = false;
        for pull in row {
            if matched.len() >= self.query.limit_per_repo {
                clipped_on_page = true;
                break;
            }
            if let Some(secs) = parse_rfc3339_secs(&pull.merged_at)
                && secs >= cutoff
            {
                matched.push((secs, pull));
            }
        }
        clipped_on_page
    }

    fn note_per_repo_cap(&mut self, repo: &str, matched: &MatchedPulls) {
        if matched.rows.len() < self.query.limit_per_repo || !matched.more_may_exist {
            return;
        }
        self.truncated.push(Truncation::new(
            "prs-per-repo",
            self.query.limit_per_repo,
            &format!(
                "{repo}: more merged PRs may exist; raise --limit-per-repo (ceiling {MAX_LIMIT_PER_REPO})"
            ),
        ));
    }

    fn entries(&mut self, repo: &str, matched: MatchedPulls) -> (Vec<PrsEntry>, Vec<String>) {
        let mut prs = Vec::new();
        let mut errors = Vec::new();
        let matched_count = matched.rows.len();
        for (_, pull) in matched.rows {
            if self.budget == 0 {
                break;
            }
            match self.fetch_entry(repo, &pull) {
                Ok(entry) => {
                    prs.push(entry);
                    self.budget -= 1;
                }
                Err(err @ HelperError::RateLimited { .. }) => {
                    errors.push(err.to_string());
                    break;
                }
                Err(err) => errors.push(err.to_string()),
            }
        }
        if self.budget == 0 {
            self.note_total_cap(repo, prs.len(), matched_count);
        }
        (prs, errors)
    }

    fn note_total_cap(&mut self, repo: &str, kept: usize, matched_count: usize) {
        if kept >= matched_count {
            return;
        }
        self.truncated.push(Truncation::new(
            "prs-total",
            self.query.max_prs,
            &format!("{repo}: total cap reached; raise --max-prs (ceiling {MAX_MAX_PRS})"),
        ));
    }

    /// The PR-total additions/deletions come from the pull detail; the
    /// changed-files list is the first REST page, capped per file row. A
    /// declared total past the collected rows marks a clipped file list.
    fn fetch_entry(&mut self, repo: &str, pull: &PullRow) -> Result<PrsEntry, HelperError> {
        let response = self
            .fetch
            .get(&pull_detail_url(&self.query.org, repo, pull.number))?;
        ensure_ok(&response)?;
        let detail = parse_pull_detail(&response.body)?;
        let response = self
            .fetch
            .get(&pull_files_url(&self.query.org, repo, pull.number))?;
        ensure_ok(&response)?;
        let changed_files = parse_file_rows(&response.body)?;
        if detail.changed_files > changed_files.len() as u64 {
            self.truncated.push(Truncation::new(
                "files-per-pr",
                MAX_FILE_ROWS,
                &format!(
                    "{repo}#{}: file list clipped; {} of {} files kept",
                    pull.number,
                    changed_files.len(),
                    detail.changed_files
                ),
            ));
        }
        Ok(PrsEntry {
            number: pull.number,
            title: pull.title.clone(),
            url: pull.url.clone(),
            author: pull.author.clone(),
            merged_at: pull.merged_at.clone(),
            additions: detail.additions,
            deletions: detail.deletions,
            changed_files,
        })
    }
}

/// One repository's kept pulls plus whether the page window clipped.
struct MatchedPulls {
    rows: Vec<(u64, PullRow)>,
    more_may_exist: bool,
}
