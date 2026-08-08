//! End-to-end behaviour of the search pipeline.

use std::fs;
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

const AUTH: &str = r#""""Auth helpers, mentioning def decoy() in the docstring."""
from enum import Enum


def validate_token(token):
    return bool(token)


def refresh(token):
    return validate_token(token)
"#;

const API: &str = r#"from auth import validate_token


def handler(request):
    if not validate_token(request.token):
        return 401
    return 200
"#;

const VENDOR: &str = "def validate_token(x):\n    return True\n";

fn tree() -> TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    for (rel, body) in [
        ("src/auth.py", AUTH),
        ("src/api.py", API),
        ("vendor/copy.py", VENDOR),
    ] {
        let path = dir.path().join(rel);
        fs::create_dir_all(path.parent().expect("has a parent")).expect("mkdir");
        fs::write(path, body).expect("write");
    }
    dir
}

fn rkgrep(root: &Path, args: &[&str]) -> (String, i32) {
    let exe = env!("CARGO_BIN_EXE_rkgrep");
    let out = Command::new(exe)
        .args(args)
        .arg(root)
        .arg("--color")
        .arg("never")
        .output()
        .expect("rkgrep runs");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

fn json_hits(root: &Path, args: &[&str]) -> serde_json::Value {
    let mut args = args.to_vec();
    args.push("--json");
    let (stdout, _) = rkgrep(root, &args);
    serde_json::from_str(&stdout).unwrap_or(serde_json::Value::Array(vec![]))
}

#[test]
fn declarations_rank_above_references() {
    let dir = tree();
    let hits = json_hits(dir.path(), &["validate_token", "-w", "-t", "1000"]);
    let first = &hits[0];
    assert_eq!(first["is_declaration"], true, "hits: {hits}");
    assert_eq!(first["symbol"], "validate_token");
}

#[test]
fn a_match_expands_to_its_enclosing_declaration() {
    let dir = tree();
    let hits = json_hits(dir.path(), &["validate_token", "-w", "-t", "1000"]);
    let api = hits
        .as_array()
        .expect("array")
        .iter()
        .find(|h| h["path"].as_str().is_some_and(|p| p.ends_with("api.py")))
        .expect("api.py is returned");
    assert_eq!(api["symbol"], "handler");
    let text = api["text"].as_str().expect("text");
    assert!(text.contains("def handler(request):"), "{text}");
    // The whole function, not a fixed window of N lines.
    assert!(text.contains("return 200"), "{text}");
}

#[test]
fn overlapping_regions_merge() {
    // api.py matches on its import line and inside handler(); the orphan
    // window around the import overlaps the function's span.
    let dir = tree();
    let hits = json_hits(dir.path(), &["validate_token", "-w", "-t", "1000"]);
    let from_api: Vec<_> = hits
        .as_array()
        .expect("array")
        .iter()
        .filter(|h| h["path"].as_str().is_some_and(|p| p.ends_with("api.py")))
        .collect();
    assert_eq!(from_api.len(), 1, "one merged span expected: {from_api:?}");
    let text = from_api[0]["text"].as_str().expect("text");
    assert!(text.contains("from auth import validate_token"), "{text}");
    assert!(text.contains("def handler"), "{text}");
}

#[test]
fn docstring_mentions_never_become_declarations() {
    let dir = tree();
    let hits = json_hits(dir.path(), &["decoy", "-t", "1000"]);
    for hit in hits.as_array().expect("array") {
        assert_eq!(hit["is_declaration"], false, "{hit}");
    }
}

#[test]
fn budget_is_respected() {
    let dir = tree();
    let hits = json_hits(dir.path(), &["validate_token", "-w", "-t", "12"]);
    let total: u64 = hits
        .as_array()
        .expect("array")
        .iter()
        .map(|h| h["tokens"].as_u64().unwrap_or(0))
        .sum();
    assert!(total <= 12, "budget exceeded: {total}");
}

#[test]
fn max_per_file_limits_crowding() {
    let dir = tree();
    let hits = json_hits(dir.path(), &["def", "-t", "4000", "--max-per-file", "1"]);
    let mut paths: Vec<&str> = hits
        .as_array()
        .expect("array")
        .iter()
        .filter_map(|h| h["path"].as_str())
        .collect();
    let before = paths.len();
    paths.sort_unstable();
    paths.dedup();
    assert_eq!(before, paths.len(), "a file appeared twice: {paths:?}");
}

#[test]
fn anchors_point_at_real_lines() {
    let dir = tree();
    let hits = json_hits(dir.path(), &["validate_token", "-w", "-t", "1000"]);
    for hit in hits.as_array().expect("array") {
        let path = dir.path().join(hit["path"].as_str().expect("path"));
        let content = fs::read_to_string(&path).expect("readable");
        let lines: Vec<&str> = content.lines().collect();
        let start = hit["start_line"].as_u64().expect("start") as usize;
        let end = hit["end_line"].as_u64().expect("end") as usize;
        assert!(start >= 1 && end <= lines.len(), "{hit}");
        let expected = lines[start - 1..end].join("\n");
        assert_eq!(hit["text"].as_str().expect("text"), expected, "{hit}");
        for line in hit["match_lines"].as_array().expect("match_lines") {
            let n = line.as_u64().expect("line number") as usize;
            assert!(n >= start && n <= end, "match outside its span: {hit}");
        }
    }
}

#[test]
fn globs_restrict_the_search() {
    let dir = tree();
    fs::write(dir.path().join("notes.txt"), "validate_token\n").expect("write");
    let hits = json_hits(dir.path(), &["validate_token", "-g", "*.txt", "-t", "500"]);
    let paths: Vec<&str> = hits
        .as_array()
        .expect("array")
        .iter()
        .filter_map(|h| h["path"].as_str())
        .collect();
    assert_eq!(paths, vec!["notes.txt"], "{paths:?}");
}

#[test]
fn no_matches_exits_one_and_prints_nothing() {
    let dir = tree();
    let (stdout, code) = rkgrep(dir.path(), &["no_such_symbol_anywhere"]);
    assert!(stdout.trim().is_empty(), "{stdout}");
    assert_eq!(code, 1);
}

#[test]
fn a_match_exits_zero() {
    let dir = tree();
    let (_, code) = rkgrep(dir.path(), &["validate_token", "-w"]);
    assert_eq!(code, 0);
}

#[test]
fn a_bad_pattern_is_reported_not_ignored() {
    let dir = tree();
    let (_, code) = rkgrep(dir.path(), &["(unclosed"]);
    assert_eq!(
        code, 2,
        "an invalid pattern must not look like 'no matches'"
    );
}

#[test]
fn a_missing_path_is_reported() {
    let exe = env!("CARGO_BIN_EXE_rkgrep");
    let out = Command::new(exe)
        .args(["pattern", "/definitely/not/here"])
        .output()
        .expect("runs");
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn literal_mode_disables_regex_syntax() {
    let dir = tree();
    fs::write(dir.path().join("lit.py"), "x = a.b(c)\n").expect("write");
    let hits = json_hits(dir.path(), &["a.b(c)", "-F", "-g", "lit.py", "-t", "500"]);
    assert_eq!(hits.as_array().expect("array").len(), 1, "{hits}");
}
