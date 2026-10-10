//! discover_file corpus: the generic closest-wins walk behind
//! `.craft-agent/mcp.json`, mirroring the agents-directory walk.

use std::fs;
use std::path::{Path, PathBuf};

use std::sync::atomic::{AtomicU32, Ordering};

use craft_core::agents::discover_file;

fn scratch(name: &str) -> std::io::Result<PathBuf> {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let unique = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "craft-mcpdisc-{}-{name}-{unique}",
        std::process::id()
    ));
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn mcp_dir(start: &Path) -> PathBuf {
    start.join(".craft-agent")
}

#[test]
fn the_closest_file_wins_and_missing_is_none() {
    let root = scratch("closest").unwrap();
    let outer = root.join("outer");
    let inner = outer.join("inner");
    fs::create_dir_all(mcp_dir(&outer)).unwrap();
    fs::create_dir_all(&inner).unwrap();
    let file = mcp_dir(&outer).join("mcp.json");
    fs::write(&file, "{}").unwrap();
    assert_eq!(
        discover_file(&inner, ".craft-agent/mcp.json"),
        Some(file.clone())
    );
    assert_eq!(discover_file(&outer, ".craft-agent/mcp.json"), Some(file));
    assert_eq!(
        discover_file(&root, ".craft-agent/mcp.json"),
        None,
        "no candidate anywhere is None, not an error"
    );
}

#[test]
fn the_git_root_hosts_the_last_candidate() {
    let base = scratch("gitroot").unwrap();
    let root = base.join("repo");
    let inner = root.join("a").join("b");
    fs::create_dir_all(&inner).unwrap();
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::create_dir_all(mcp_dir(&root)).unwrap();
    let file = mcp_dir(&root).join("mcp.json");
    fs::write(&file, "{}").unwrap();
    assert_eq!(
        discover_file(&inner, ".craft-agent/mcp.json"),
        Some(file.clone()),
        "a file at the git root is the last candidate hit"
    );
    assert_eq!(discover_file(&root, ".craft-agent/mcp.json"), Some(file));
}

#[test]
fn the_walk_stops_at_the_git_root() {
    let base = scratch("rootstop").unwrap();
    fs::create_dir_all(mcp_dir(&base)).unwrap();
    fs::write(mcp_dir(&base).join("mcp.json"), "{}").unwrap();
    let repo = base.join("repo");
    fs::create_dir_all(repo.join(".git")).unwrap();
    let deep = repo.join("a").join("b");
    fs::create_dir_all(&deep).unwrap();
    assert_eq!(
        discover_file(&deep, ".craft-agent/mcp.json"),
        None,
        "a file above the git root is never reached"
    );
}

#[test]
fn a_directory_at_the_leaf_path_is_not_a_file_hit() {
    let root = scratch("dirhit").unwrap();
    fs::create_dir_all(mcp_dir(&root).join("mcp.json")).unwrap();
    assert_eq!(
        discover_file(&root, ".craft-agent/mcp.json"),
        None,
        "the file walk never reports a directory"
    );
}
