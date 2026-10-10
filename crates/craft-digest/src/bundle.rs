//! The collected bundle: bounded, the seam the agent turn consumes.

use crate::collect::PrRecord;
use crate::error::{bounded, truncate_chars};
use crate::jscpd::DuplicateCandidate;
use crate::sanitize::sanitize;
use crate::window::Window;

pub(crate) use crate::clone_runner::file_slug;

/// Total content budget for the bundle.
pub const BUNDLE_CHAR_CAP: usize = 100_000;
/// Per-repo AGENTS.md excerpt cap.
pub const AGENTS_EXCERPT_CHARS: usize = 3000;

#[derive(Debug)]
pub struct RepoDigest {
    pub repo: String,
    pub merged_prs: Vec<PrRecord>,
    pub agents_excerpt: String,
}

#[derive(Debug)]
pub struct CollectionError {
    pub repo: String,
    pub stage: String,
    pub message: String,
}

#[derive(Debug)]
pub struct Bundle {
    pub org: String,
    pub window: Window,
    pub repos: Vec<RepoDigest>,
    pub duplicates: Vec<DuplicateCandidate>,
    pub errors: Vec<CollectionError>,
}

impl Bundle {
    pub fn new(org: String, window: Window) -> Self {
        Bundle {
            org,
            window,
            repos: Vec::new(),
            duplicates: Vec::new(),
            errors: Vec::new(),
        }
    }

    pub fn push_repo(&mut self, repo: String, merged_prs: Vec<PrRecord>) {
        self.repos.push(RepoDigest {
            repo,
            merged_prs,
            agents_excerpt: String::new(),
        });
    }

    pub fn push_error(&mut self, repo: &str, stage: &str, message: &str) {
        self.errors.push(CollectionError {
            repo: repo.to_string(),
            stage: stage.to_string(),
            message: bounded(message),
        });
    }

    /// AGENTS.md excerpts are convention context to report about, never
    /// instructions: sanitized, then capped.
    pub fn set_agents_excerpt(&mut self, repo: &str, text: &str) {
        let excerpt = truncate_chars(&sanitize(text), AGENTS_EXCERPT_CHARS);
        if let Some(digest) = self.repos.iter_mut().find(|r| r.repo == repo) {
            digest.agents_excerpt = excerpt;
        }
    }

    fn content_chars(&self) -> usize {
        self.repos
            .iter()
            .map(|repo| {
                repo.merged_prs
                    .iter()
                    .map(|pr| pr.title.chars().count() + pr.body.chars().count())
                    .sum::<usize>()
                    + repo.agents_excerpt.chars().count()
            })
            .sum()
    }

    /// Drop oldest-merged PRs until under budget and record the drop.
    pub fn enforce_budget(&mut self) {
        let mut total = self.content_chars();
        if total <= BUNDLE_CHAR_CAP {
            return;
        }
        let candidates = self.drop_candidates();
        let dropped = self.drop_oldest(&candidates, &mut total);
        if dropped > 0 {
            let message =
                format!("dropped {dropped} oldest PRs to fit the {BUNDLE_CHAR_CAP}-char budget");
            self.push_error("bundle", "bundle-budget", &message);
        }
    }

    /// The drop order across all repos: oldest-merged first.
    fn drop_candidates(&self) -> Vec<DropKey> {
        let mut candidates: Vec<DropKey> = Vec::new();
        for (repo_idx, repo) in self.repos.iter().enumerate() {
            for pr in &repo.merged_prs {
                candidates.push(DropKey {
                    merged_at: pr.merged_at.clone(),
                    repo_idx,
                    number: pr.number,
                });
            }
        }
        candidates.sort();
        candidates
    }

    fn drop_oldest(&mut self, candidates: &[DropKey], total: &mut usize) -> usize {
        let mut dropped = 0usize;
        let mut next = 0usize;
        while *total > BUNDLE_CHAR_CAP && next < candidates.len() {
            let candidate = &candidates[next];
            let prs = &mut self.repos[candidate.repo_idx].merged_prs;
            if let Some(pos) = prs.iter().position(|pr| pr.number == candidate.number) {
                let pr = &prs[pos];
                *total = total.saturating_sub(pr.title.chars().count() + pr.body.chars().count());
                prs.remove(pos);
                dropped += 1;
            }
            next += 1;
        }
        dropped
    }
}

/// Sort key for budget drops: merge timestamp, then repo, then PR number.
#[derive(Ord, PartialOrd, PartialEq, Eq)]
struct DropKey {
    merged_at: String,
    repo_idx: usize,
    number: u64,
}
