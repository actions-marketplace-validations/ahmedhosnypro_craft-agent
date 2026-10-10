// Corpus fixtures: unwrap() is the loud-test idiom here.
#![allow(clippy::unwrap_used)]

use craft_digest::bundle::{AGENTS_EXCERPT_CHARS, BUNDLE_CHAR_CAP, Bundle, RepoDigest};
use craft_digest::collect::PrRecord;
use craft_digest::jscpd::{CloneSite, DuplicateCandidate};
use craft_digest::window::{Window, WindowSource};

fn pr(number: u64, body: &str) -> PrRecord {
    PrRecord {
        number,
        title: format!("title {number}"),
        url: format!("https://example.invalid/pr/{number}"),
        author: "dev1".to_string(),
        merged_at: format!("2026-10-0{}T00:00:00Z", 1 + number % 9),
        additions: 1,
        deletions: 0,
        labels: Vec::new(),
        body: body.to_string(),
    }
}

fn sample() -> Bundle {
    Bundle {
        org: "storytold".to_string(),
        window: Window {
            since: "2026-10-01".to_string(),
            until: "2026-10-11".to_string(),
            source: WindowSource::Marker,
        },
        repos: vec![RepoDigest {
            repo: "photocraft".to_string(),
            merged_prs: vec![pr(1, "body one")],
            agents_excerpt: "Run `just check` before committing.".to_string(),
        }],
        duplicates: vec![DuplicateCandidate {
            first: CloneSite {
                repo: "app-a".to_string(),
                file: "src/lib.rs".to_string(),
                line: 3,
            },
            second: CloneSite {
                repo: "app-b".to_string(),
                file: "src/main.rs".to_string(),
                line: 2,
            },
            tokens: 105,
            lines: 15,
        }],
        errors: Vec::new(),
    }
}

fn content_chars(b: &Bundle) -> usize {
    b.repos
        .iter()
        .map(|r| {
            r.merged_prs
                .iter()
                .map(|p| p.title.chars().count() + p.body.chars().count())
                .sum::<usize>()
                + r.agents_excerpt.chars().count()
        })
        .sum()
}

#[test]
fn bundle_budget_drops_oldest_prs_and_records_it() {
    let mut b = sample();
    b.repos = (0..2)
        .map(|r| RepoDigest {
            repo: format!("repo{r}"),
            merged_prs: (0..30).map(|n| pr(n, &"y".repeat(1900))).collect(),
            agents_excerpt: String::new(),
        })
        .collect();
    assert!(content_chars(&b) > BUNDLE_CHAR_CAP);
    b.enforce_budget();
    assert!(content_chars(&b) <= BUNDLE_CHAR_CAP);
    assert!(b.errors.iter().any(|e| e.stage == "bundle-budget"));
    let days: Vec<&str> = b
        .repos
        .iter()
        .flat_map(|r| r.merged_prs.iter().map(|p| p.merged_at.as_str()))
        .collect();
    assert!(!days.contains(&"2026-10-01T00:00:00Z"), "oldest kept");
    assert!(days.contains(&"2026-10-02T00:00:00Z"), "over-dropped");
    assert!(days.contains(&"2026-10-09T00:00:00Z"), "newest dropped");
}

#[test]
fn agents_excerpt_is_capped_and_sanitized() {
    let mut b = sample();
    let long: String = "a".repeat(4000);
    b.set_agents_excerpt("photocraft", &format!("{long}<!-- hidden -->"));
    let excerpt = &b.repos[0].agents_excerpt;
    assert_eq!(excerpt.chars().count(), AGENTS_EXCERPT_CHARS);
    assert!(!excerpt.contains("hidden"));
    assert!(excerpt.starts_with("aaa"));
}

#[test]
fn hostile_excerpt_repo_is_ignored() {
    let mut b = sample();
    b.set_agents_excerpt("no-such-repo", "text");
    assert_eq!(
        b.repos[0].agents_excerpt,
        "Run `just check` before committing."
    );
}
