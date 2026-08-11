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
fn comments_scope_matches_to_comments() {
    let dir = tempfile::tempdir().expect("temp dir");
    // `stale` is named in a comment inside `evict`, and declared below it.
    fs::write(
        dir.path().join("cache.py"),
        "def evict(key):\n    # drop the stale entry\n    return key\n\n\ndef stale(key):\n    return key\n",
    )
    .expect("write");

    let hits = json_hits(dir.path(), &["stale", "--comments", "-t", "1000"]);
    assert_eq!(hits.as_array().map(Vec::len), Some(1), "hits: {hits}");
    // Filter only: the span is still the declaration the comment sits in.
    assert_eq!(hits[0]["symbol"], "evict");
}

#[test]
fn comments_ignore_attributes() {
    let dir = tempfile::tempdir().expect("temp dir");
    fs::write(
        dir.path().join("lib.rs"),
        "#[derive(Debug)]\nstruct Marker;\n",
    )
    .expect("write");
    let (stdout, code) = rkgrep(dir.path(), &["derive", "-C"]);
    assert_eq!(code, 1, "stdout: {stdout}");
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

/// A caller written as a one-line binding is not a declaration of the callee.
///
/// `const a = store.createProject(...)` puts the match on the first line of a
/// declaration — but of `a`, not of `createProject`. Treating that as a
/// declaration gives the unconditional declaration bonus to every such caller,
/// and a file full of them buries the file that declares the name.
#[test]
fn a_binding_that_calls_the_symbol_does_not_rank_as_its_declaration() {
    let dir = tempfile::tempdir().expect("temp dir");
    fs::write(
        dir.path().join("api.ts"),
        "export function createProject(name) {\n  return { name };\n}\n",
    )
    .expect("write");
    fs::write(
        dir.path().join("uses.test.ts"),
        "test('one', () => {\n\
         const a = store.createProject({ name: 'A' });\n\
         const b = store.createProject({ name: 'B' });\n\
         });\n",
    )
    .expect("write");

    let hits = json_hits(dir.path(), &["createProject", "-w", "-t", "1000"]);
    let first = &hits[0];
    assert_eq!(first["symbol"], "createProject", "hits: {hits}");
    assert_eq!(first["is_declaration"], true, "hits: {hits}");
    assert!(
        first["path"]
            .as_str()
            .unwrap_or_default()
            .ends_with("api.ts"),
        "the declaring file should rank first, hits: {hits}"
    );
}

/// Under `-w`, a longer name that merely contains the pattern is a different
/// symbol and must not claim the declaration bonus.
#[test]
fn a_longer_name_containing_the_pattern_is_not_its_declaration() {
    let dir = tempfile::tempdir().expect("temp dir");
    fs::write(
        dir.path().join("mgr.ts"),
        "export function createProjectManager(opts) {\n  return opts;\n}\n",
    )
    .expect("write");
    fs::write(
        dir.path().join("api.ts"),
        "export function createProject(name) {\n  return { name };\n}\n",
    )
    .expect("write");

    let hits = json_hits(dir.path(), &["createProject", "-w", "-t", "1000"]);
    let declaring: Vec<&str> = hits
        .as_array()
        .map(|hits| {
            hits.iter()
                .filter(|h| h["is_declaration"] == true)
                .filter_map(|h| h["symbol"].as_str())
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(declaring, vec!["createProject"], "hits: {hits}");
}

/// Without a budget nothing is dropped for size, which is what standing in
/// for `rg` requires.
#[test]
fn no_budget_returns_more_than_a_tight_budget() {
    let dir = tree();
    let tight = json_hits(dir.path(), &["def", "-t", "40"]);
    let all = json_hits(dir.path(), &["def", "--no-budget"]);

    let tokens = |hits: &serde_json::Value| -> u64 {
        hits.as_array()
            .map(|hits| hits.iter().filter_map(|h| h["tokens"].as_u64()).sum())
            .unwrap_or(0)
    };
    assert!(
        tokens(&all) > 40,
        "an unbudgeted run should exceed a 40-token budget: {all}"
    );
    assert!(
        all.as_array().map(Vec::len).unwrap_or(0) > tight.as_array().map(Vec::len).unwrap_or(0),
        "tight: {tight}\nall: {all}"
    );
}

/// The per-file cap exists to stop one module taking a budget. With no budget
/// to protect, it only hides matches — so it lifts with it.
#[test]
fn no_budget_lifts_the_per_file_cap_but_an_explicit_one_still_wins() {
    let dir = tempfile::tempdir().expect("temp dir");
    let mut crowded = String::new();
    for i in 0..8 {
        crowded.push_str(&format!("def handler_{i}(req):\n    return {i}\n\n"));
    }
    fs::write(dir.path().join("crowded.py"), &crowded).expect("write");

    let uncapped = json_hits(dir.path(), &["handler_", "--no-budget"]);
    assert!(
        uncapped.as_array().map(Vec::len).unwrap_or(0) > 3,
        "the default cap of 3 should not apply: {uncapped}"
    );

    let capped = json_hits(
        dir.path(),
        &["handler_", "--no-budget", "--max-per-file", "2"],
    );
    assert_eq!(
        capped.as_array().map(Vec::len).unwrap_or(0),
        2,
        "an explicit cap outranks the flag: {capped}"
    );
}

/// `impl Repo` re-opens a name; `struct Repo` declares it, and `impl Store for
/// Repo` names a trait declared somewhere else again. All three match the
/// name, so letting an impl block claim the declaration bonus puts a block of
/// methods above the definition those methods belong to.
#[test]
fn an_impl_block_is_not_where_the_type_is_declared() {
    let dir = tempfile::tempdir().expect("temp dir");
    // The impl block names the type more often than the declaration does,
    // which is the ordinary shape of a Rust file and is what puts it ahead on
    // match count alone.
    fs::write(
        dir.path().join("repo.rs"),
        "pub struct Repo<T> {\n    item: T,\n}\n\n\
         impl<T> Repo<T> {\n\
         \x20   pub fn duplicate(&self) -> Repo<T> {\n\
         \x20       Repo { item: self.item }\n\
         \x20   }\n\
         }\n",
    )
    .expect("write");

    let hits = json_hits(dir.path(), &["Repo", "-w"]);
    assert_eq!(hits[0]["kind"], "struct", "{hits}");
    assert_eq!(hits[0]["start_line"], 1, "{hits}");
}

/// A declaration whose body runs to 400 lines, with the match inside it.
fn container(marker: &str) -> String {
    let mut source = format!("class Registry:\n    seed({marker})\n");
    for i in 0..400 {
        source.push_str(&format!("    values.append({i})\n"));
    }
    source
}

/// A declaration too large for the budget is a container, not an answer. The
/// match comes back as a window into it: no budget could admit the whole
/// block, so returning it means the packer drops it and the query reports
/// nothing at all.
#[test]
fn an_oversized_declaration_comes_back_as_a_window() {
    let dir = tempfile::tempdir().expect("temp dir");
    fs::write(dir.path().join("big.py"), container("MARKER_NAME")).expect("write");

    let hits = json_hits(dir.path(), &["MARKER_NAME"]);
    assert!(
        !hits.as_array().expect("array").is_empty(),
        "the match must be reported: {hits}"
    );
    let start = hits[0]["start_line"].as_u64().expect("start");
    let end = hits[0]["end_line"].as_u64().expect("end");
    assert!(
        end - start <= 12,
        "a window, not the whole block: {start}-{end}"
    );
}

/// Clamping an oversized declaration must not cost it the declaration bonus:
/// a match on its own first line is still the answer to "where is this
/// declared", and the span simply starts there instead of covering the body.
#[test]
fn an_oversized_declaration_still_answers_where_it_is_declared() {
    let dir = tempfile::tempdir().expect("temp dir");
    fs::write(dir.path().join("big.py"), container("seed")).expect("write");

    let hits = json_hits(dir.path(), &["Registry", "-w"]);
    assert_eq!(hits[0]["symbol"], "Registry", "{hits}");
    assert_eq!(hits[0]["is_declaration"], true, "{hits}");
}

/// Candidates are extracted several at a time, on a thread each, and the walk
/// that produced them finishes in no particular order. Neither may reach the
/// result: the same query over the same tree is the same answer every time.
#[test]
fn repeated_runs_return_identical_results() {
    let dir = tempfile::tempdir().expect("temp dir");
    for i in 0..40 {
        fs::write(
            dir.path().join(format!("mod_{i}.py")),
            format!(
                "def handler_{i}(req):\n    return validate_token(req)\n\n\ndef helper_{i}(x):\n    return x\n"
            ),
        )
        .expect("write");
    }
    let first = json_hits(dir.path(), &["validate_token", "-t", "300"]);
    assert!(!first.as_array().expect("array").is_empty(), "{first}");
    for _ in 0..4 {
        assert_eq!(
            json_hits(dir.path(), &["validate_token", "-t", "300"]),
            first
        );
    }
}

/// A declaration whose file ends in data used to run to end of file, so the
/// only span answering the query did not fit any budget, the packer dropped
/// it, and the run reported nothing — indistinguishable from the pattern
/// being absent from the tree.
#[test]
fn a_declaration_above_a_data_literal_still_fits_a_budget() {
    let dir = tempfile::tempdir().expect("temp dir");
    let mut source = String::from("def find_user(id):\n    return id\n\nDATA = [\n");
    for i in 0..1500 {
        source.push_str(&format!("    \"row {i}\",\n"));
    }
    source.push_str("]\n");
    fs::write(dir.path().join("rows.py"), &source).expect("write");

    let hits = json_hits(dir.path(), &["find_user", "-w"]);
    assert_eq!(
        hits[0]["symbol"], "find_user",
        "the declaration must survive the budget: {hits}"
    );
}

/// The per-file match-line cap is a budget optimization, so it lifts with the
/// budget. A file with more matches than the cap must still report all of them.
#[test]
fn no_budget_reports_matches_past_the_per_file_match_line_cap() {
    const DECLARATIONS: usize = 700;

    let dir = tempfile::tempdir().expect("temp dir");
    let mut source = String::new();
    for i in 0..DECLARATIONS {
        source.push_str(&format!("def handler_{i}(req):\n    return {i}\n\n"));
    }
    fs::write(dir.path().join("many.py"), &source).expect("write");

    let hits = json_hits(dir.path(), &["handler_", "--no-budget"]);
    assert_eq!(
        hits.as_array().map(Vec::len).unwrap_or(0),
        DECLARATIONS,
        "every declaration should come back"
    );
}
