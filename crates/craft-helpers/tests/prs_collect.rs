// prs corpus: collection over fake routes; a repo failure is a row, never fatal.
#![allow(clippy::unwrap_used)]

mod common;

use common::{FakeFetch, fixture};
use craft_helpers::prs::FileRow;
use craft_helpers::prs_run::{
    ErrorRow, MAX_PAGES, PER_PAGE, PrsEntry, PrsQuery, PrsReport, collect_prs, pull_detail_url,
    pull_files_url, pulls_page_url,
};
use craft_helpers::time::parse_rfc3339_secs;

const PAGE1: &str = "https://api.github.com/repos/storytold/craft-agent/pulls?state=closed&sort=updated&direction=desc&per_page=100&page=1";

fn routes_for(fetch: FakeFetch, repo: &str) -> FakeFetch {
    let org = "storytold";
    let page = pulls_page_url(org, repo, 1);
    let detail = |number: u64| pull_detail_url(org, repo, number);
    let files = |number: u64| pull_files_url(org, repo, number);
    fetch
        .route(&page, 200, fixture("pulls_page1.json"))
        .route(detail(101), 200, fixture("pull_101.json"))
        .route(files(101), 200, fixture("files_101.json"))
        .route(detail(98), 200, fixture("pull_98.json"))
        .route(files(98), 200, fixture("files_98.json"))
}

fn query(repos: &[&str], lookback: u64, limit: usize, max_prs: usize) -> PrsQuery {
    PrsQuery {
        org: "storytold".to_string(),
        repos: repos.iter().map(|repo| repo.to_string()).collect(),
        now_secs: parse_rfc3339_secs("2026-10-11T12:00:00Z").unwrap(),
        lookback_secs: lookback,
        limit_per_repo: limit,
        max_prs,
        since_label: "48h".to_string(),
    }
}

#[test]
fn urls_match_the_verified_rest_surface() {
    assert_eq!(pulls_page_url("storytold", "craft-agent", 1), PAGE1);
    assert_eq!(
        pull_detail_url("storytold", "craft-agent", 101),
        "https://api.github.com/repos/storytold/craft-agent/pulls/101"
    );
    assert_eq!(
        pull_files_url("storytold", "craft-agent", 101),
        "https://api.github.com/repos/storytold/craft-agent/pulls/101/files?per_page=100"
    );
}

#[test]
fn collects_merged_prs_inside_the_window() {
    let fetch = routes_for(FakeFetch::new(), "craft-agent");
    let report = collect_prs(&fetch, &query(&["craft-agent"], 48 * 3600, 30, 50));
    assert_summary(&report);
    let repo = &report.repos[0];
    assert_eq!(repo.repo, "craft-agent");
    assert_eq!(pr_numbers(&report, 0), vec![101, 98]);
    assert_pr_101(&repo.prs[0]);
    assert!(report.truncated.is_empty());
}

fn assert_summary(report: &PrsReport) {
    assert!(report.errors.is_empty());
    assert_eq!(report.org, "storytold");
    assert_eq!(report.since, "48h");
    assert_eq!(report.repos.len(), 1);
}

fn assert_pr_101(pr: &PrsEntry) {
    assert_eq!(pr.title, "Fix the digest window clamp");
    assert_eq!(pr.author, "dev1");
    assert_eq!(pr.merged_at, "2026-10-11T08:00:00Z");
    assert_eq!(pr.additions, 120);
    assert_eq!(pr.deletions, 30);
    assert_files_window(&pr.changed_files);
}

fn assert_files_window(files: &[FileRow]) {
    assert_eq!(files.len(), 3);
    assert_eq!(files[0].path, "crates/craft-digest/src/window.rs");
    assert_eq!(files[0].status, "modified");
}

#[test]
fn failing_repos_become_error_rows_never_fatal() {
    let mut fetch = routes_for(FakeFetch::new(), "craft-agent");
    let ratey = pulls_page_url("storytold", "ratey", 1);
    fetch = fetch.route_rate_limited(&ratey, "30");
    let report = collect_prs(&fetch, &query(&["craft-agent", "ratey"], 48 * 3600, 30, 50));
    assert_eq!(report.repos.len(), 2);
    assert_eq!(report.repos[0].prs.len(), 2);
    assert!(report.repos[1].prs.is_empty());
    assert_error_rows(&report.errors, &["ratey"]);
    let message = &report.errors[0].message;
    assert!(message.contains("429"), "{message}");
    assert!(message.contains("30"), "{message}");
}

fn assert_error_rows(errors: &[ErrorRow], repos: &[&str]) {
    assert_eq!(errors.len(), repos.len());
    for (row, repo) in errors.iter().zip(repos) {
        assert_eq!(row.repo, *repo);
    }
}

#[test]
fn malformed_and_unrouted_repos_become_error_rows() {
    let mut fetch = FakeFetch::new();
    let broken = pulls_page_url("storytold", "broken", 1);
    fetch = fetch.route(&broken, 200, "{\"broken\": ");
    let report = collect_prs(&fetch, &query(&["broken", "nolisting"], 48 * 3600, 30, 50));
    assert_eq!(report.repos.len(), 2);
    assert!(report.repos.iter().all(|repo| repo.prs.is_empty()));
    assert_error_rows(&report.errors, &["broken", "nolisting"]);
    assert!(report.errors[0].message.contains("JSON"));
}

fn full_page(first_number: usize) -> String {
    let mut body = String::from('[');
    for offset in 0..PER_PAGE {
        let number = first_number + offset;
        let row = format!(
            "{{\"number\":{number},\"title\":\"t\",\"html_url\":\"u\",\"user\":{{\"login\":\"a\"}},\"merged_at\":null}}"
        );
        body.push_str(&row);
        body.push(',');
    }
    body.pop();
    body.push(']');
    body
}

fn pr_numbers(report: &PrsReport, index: usize) -> Vec<u64> {
    report.repos[index].prs.iter().map(|pr| pr.number).collect()
}

fn assert_truncated(report: &PrsReport, field: &str, cap: usize, hints: &[&str]) {
    assert_eq!(report.truncated.len(), 1);
    let marker = &report.truncated[0];
    assert_eq!(marker.field, field);
    assert_eq!(marker.cap, cap);
    for hint in hints {
        assert!(marker.hint.contains(hint), "{}", marker.hint);
    }
}

#[test]
fn pages_beyond_a_full_first_page() {
    let page1 = pulls_page_url("storytold", "deep", 1);
    let page2 = pulls_page_url("storytold", "deep", 2);
    let detail = pull_detail_url("storytold", "deep", 96);
    let files = pull_files_url("storytold", "deep", 96);
    let fetch = FakeFetch::new()
        .route(&page1, 200, full_page(1000))
        .route(&page2, 200, fixture("pulls_page2.json"))
        .route(&detail, 200, fixture("pull_96.json"))
        .route(&files, 200, fixture("files_96.json"));
    let report = collect_prs(&fetch, &query(&["deep"], 48 * 3600, 30, 50));
    assert!(report.errors.is_empty());
    assert_pr_96(&report.repos[0].prs[0]);
}

fn assert_pr_96(pr: &PrsEntry) {
    assert_eq!(pr.number, 96);
    assert_eq!(pr.additions, 8);
}

#[test]
fn paging_stops_at_the_page_cap() {
    let mut fetch = FakeFetch::new();
    for page in 1..=MAX_PAGES {
        let body = full_page(1000 + page * 100);
        fetch = fetch.route(pulls_page_url("storytold", "loopy", page), 200, &body);
    }
    let report = collect_prs(&fetch, &query(&["loopy"], 48 * 3600, 30, 50));
    assert!(report.errors.is_empty());
    assert!(report.repos[0].prs.is_empty());
}

#[test]
fn per_repo_cap_keeps_the_newest_merged() {
    let fetch = routes_for(FakeFetch::new(), "craft-agent");
    let report = collect_prs(&fetch, &query(&["craft-agent"], 48 * 3600, 1, 50));
    assert_eq!(report.repos[0].prs.len(), 1);
    assert_eq!(report.repos[0].prs[0].number, 101);
}

#[test]
fn global_cap_truncates_in_repo_order() {
    let mut fetch = routes_for(FakeFetch::new(), "one");
    fetch = routes_for(fetch, "two");
    let report = collect_prs(&fetch, &query(&["one", "two"], 48 * 3600, 30, 3));
    assert_eq!(report.repos[0].prs.len(), 2);
    assert_eq!(report.repos[1].prs.len(), 1);
    assert!(report.errors.is_empty());
}

fn merged_page(rows: &[(u64, &str)]) -> String {
    let mut body = String::from('[');
    for (number, merged_at) in rows {
        body.push_str(&format!(
            "{{\"number\":{number},\"title\":\"t\",\"html_url\":\"u\",\"user\":{{\"login\":\"a\"}},\"merged_at\":\"{merged_at}\"}}"
        ));
        body.push(',');
    }
    body.pop();
    body.push(']');
    body
}

#[test]
fn rate_limit_mid_entry_stops_the_repo() {
    let org = "storytold";
    let repo = "midlimit";
    let page = pulls_page_url(org, repo, 1);
    let body = merged_page(&[
        (3, "2026-10-11T06:00:00Z"),
        (2, "2026-10-11T05:00:00Z"),
        (1, "2026-10-11T04:00:00Z"),
    ]);
    let detail = |number: u64| pull_detail_url(org, repo, number);
    let files = |number: u64| pull_files_url(org, repo, number);
    let fetch = FakeFetch::new()
        .route(&page, 200, &body)
        .route(detail(3), 200, "{\"additions\":1}")
        .route(files(3), 200, "[]")
        .route_rate_limited(detail(2), "60");
    let report = collect_prs(&fetch, &query(&[repo], 48 * 3600, 30, 50));
    assert_eq!(pr_numbers(&report, 0), vec![3]);
    assert_eq!(report.errors.len(), 1);
    assert!(report.errors[0].message.contains("429"));
}

#[test]
fn offset_timestamps_sort_by_instant_not_text() {
    let org = "storytold";
    let repo = "offsets";
    let page = pulls_page_url(org, repo, 1);
    // 11:00+02:00 is 09:00Z, older than 09:30Z despite sorting after it as text.
    let body = merged_page(&[
        (2, "2026-10-11T11:00:00+02:00"),
        (1, "2026-10-11T09:30:00Z"),
    ]);
    let detail = |number: u64| pull_detail_url(org, repo, number);
    let files = |number: u64| pull_files_url(org, repo, number);
    let fetch = FakeFetch::new()
        .route(&page, 200, &body)
        .route(detail(1), 200, "{\"additions\":1}")
        .route(files(1), 200, "[]")
        .route(detail(2), 200, "{\"additions\":1}")
        .route(files(2), 200, "[]");
    let report = collect_prs(&fetch, &query(&[repo], 48 * 3600, 2, 50));
    assert!(report.errors.is_empty());
    assert_eq!(pr_numbers(&report, 0), vec![1, 2]);
}

#[test]
fn per_repo_cap_marks_truncation() {
    let fetch = routes_for(FakeFetch::new(), "craft-agent");
    let report = collect_prs(&fetch, &query(&["craft-agent"], 48 * 3600, 1, 50));
    assert_eq!(report.repos[0].prs.len(), 1);
    assert_truncated(
        &report,
        "prs-per-repo",
        1,
        &["craft-agent", "--limit-per-repo"],
    );
}

#[test]
fn total_cap_marks_truncation_for_the_clipped_repo() {
    let mut fetch = routes_for(FakeFetch::new(), "one");
    fetch = routes_for(fetch, "two");
    let report = collect_prs(&fetch, &query(&["one", "two"], 48 * 3600, 30, 3));
    assert_eq!(report.repos[0].prs.len(), 2);
    assert_eq!(report.repos[1].prs.len(), 1);
    assert_truncated(&report, "prs-total", 3, &["two", "--max-prs"]);
}

#[test]
fn unclipped_collections_carry_no_truncation_markers() {
    let mut fetch = routes_for(FakeFetch::new(), "one");
    fetch = routes_for(fetch, "two");
    let report = collect_prs(&fetch, &query(&["one", "two"], 48 * 3600, 30, 50));
    assert!(report.errors.is_empty());
    assert!(report.truncated.is_empty());
}

#[test]
fn clipped_file_list_marks_truncation() {
    const WIDE_DETAIL: &str = "{\"additions\":9,\"deletions\":2,\"changed_files\":80}";
    let org = "storytold";
    let repo = "wide";
    let body = merged_page(&[(7, "2026-10-11T06:00:00Z")]);
    let fetch = FakeFetch::new()
        .route(pulls_page_url(org, repo, 1), 200, &body)
        .route(pull_detail_url(org, repo, 7), 200, WIDE_DETAIL)
        .route(pull_files_url(org, repo, 7), 200, fixture("files_101.json"));
    let report = collect_prs(&fetch, &query(&[repo], 48 * 3600, 30, 50));
    assert!(report.errors.is_empty());
    assert_eq!(report.repos[0].prs.len(), 1);
    assert_eq!(report.repos[0].prs[0].changed_files.len(), 3);
    assert_truncated(&report, "files-per-pr", 100, &["wide#7", "80"]);
}
