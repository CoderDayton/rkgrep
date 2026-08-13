//! Several patterns at once, and the flags that shape what comes back.

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use tempfile::TempDir;

const AUTH: &str = r#"def validate_token(token):
    return bool(token)


def refresh(token):
    return validate_token(token)


class Claims:
    subject = ""
"#;

const API: &str = r#"from auth import validate_token, Claims


def handler(request):
    if not validate_token(request.token):
        return 401
    return Claims()


def reissue(request):
    return refresh(request.token)
"#;

const NOTES: &str = "# refresh the Claims cache nightly\nCACHE = {}\n";

fn tree() -> TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    for (rel, body) in [
        ("src/auth.py", AUTH),
        ("src/api.py", API),
        ("src/notes.py", NOTES),
    ] {
        let path = dir.path().join(rel);
        fs::create_dir_all(path.parent().expect("has a parent")).expect("mkdir");
        fs::write(path, body).expect("write");
    }
    dir
}

/// stdout, stderr and the exit code, with the root appended as the CLI takes
/// it and colour off so the bytes are the same everywhere.
fn run(root: &Path, args: &[&str], stdin: Option<&str>) -> (String, String, i32) {
    let exe = env!("CARGO_BIN_EXE_rkgrep");
    let mut command = Command::new(exe);
    command
        .args(args)
        .arg(root)
        .arg("--color")
        .arg("never")
        .stdin(match stdin {
            Some(_) => Stdio::piped(),
            None => Stdio::null(),
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("rkgrep runs");
    if let Some(text) = stdin {
        use std::io::Write;
        child
            .stdin
            .as_mut()
            .expect("stdin is piped")
            .write_all(text.as_bytes())
            .expect("write stdin");
    }
    let out = child.wait_with_output().expect("rkgrep finishes");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

fn stdout(root: &Path, args: &[&str]) -> String {
    run(root, args, None).0
}

fn json(root: &Path, args: &[&str]) -> Vec<serde_json::Value> {
    let mut args = args.to_vec();
    args.push("--json");
    serde_json::from_str(&stdout(root, &args)).unwrap_or_default()
}

fn queries_of(hits: &[serde_json::Value]) -> Vec<String> {
    hits.iter()
        .map(|hit| hit["query"].as_str().unwrap_or("").to_string())
        .collect()
}

#[test]
fn several_patterns_are_all_answered() {
    let dir = tree();
    let hits = json(dir.path(), &["-e", "validate_token", "-e", "Claims"]);
    let asked = queries_of(&hits);
    assert!(asked.contains(&"validate_token".to_string()), "{asked:?}");
    assert!(asked.contains(&"Claims".to_string()), "{asked:?}");
}

#[test]
fn every_pattern_is_answered_before_any_is_answered_twice() {
    let dir = tree();
    let hits = json(dir.path(), &["-e", "validate_token", "-e", "Claims"]);
    let asked = queries_of(&hits);
    assert!(asked.len() >= 2, "need two spans to interleave: {asked:?}");
    assert_ne!(
        asked[0], asked[1],
        "the rotation repeated a pattern: {asked:?}"
    );
}

#[test]
fn a_span_two_patterns_both_hit_is_returned_once() {
    let dir = tree();
    // `handler` mentions both, so both patterns produce the same span.
    let hits = json(dir.path(), &["-e", "validate_token", "-e", "Claims"]);
    let mut anchors: Vec<String> = hits
        .iter()
        .map(|hit| {
            format!(
                "{}:{}-{}",
                hit["path"].as_str().unwrap_or(""),
                hit["start_line"],
                hit["end_line"]
            )
        })
        .collect();
    let before = anchors.len();
    anchors.sort();
    anchors.dedup();
    assert_eq!(before, anchors.len(), "a span came back twice: {anchors:?}");
}

#[test]
fn a_pattern_that_matched_nothing_is_named() {
    let dir = tree();
    let (_, err, _) = run(dir.path(), &["-e", "Claims", "-e", "no_such_symbol"], None);
    assert!(err.contains("no_such_symbol"), "{err:?}");
    // The one that did match is not reported as missing.
    assert!(!err.contains("Claims"), "{err:?}");
}

#[test]
fn one_pattern_says_nothing_about_unmatched_patterns() {
    let dir = tree();
    let (_, err, code) = run(dir.path(), &["no_such_symbol"], None);
    assert_eq!(code, 1);
    assert!(err.is_empty(), "{err:?}");
}

#[test]
fn declarations_only_drops_every_mention() {
    let dir = tree();
    let hits = json(dir.path(), &["-d", "validate_token"]);
    assert!(!hits.is_empty());
    assert!(
        hits.iter().all(|hit| hit["is_declaration"] == true),
        "{hits:?}"
    );
}

#[test]
fn references_only_drops_every_declaration() {
    let dir = tree();
    let hits = json(dir.path(), &["-r", "validate_token"]);
    assert!(!hits.is_empty());
    assert!(
        hits.iter().all(|hit| hit["is_declaration"] == false),
        "{hits:?}"
    );
}

#[test]
fn kind_filters_and_asks_only_for_declarations() {
    let dir = tree();
    let hits = json(dir.path(), &["--kind", "class", "Claims"]);
    assert!(!hits.is_empty());
    for hit in &hits {
        assert_eq!(hit["kind"], "class", "{hit:?}");
        assert_eq!(hit["is_declaration"], true, "{hit:?}");
    }
    // A kind nothing declares returns nothing rather than falling back.
    assert!(json(dir.path(), &["--kind", "struct", "Claims"]).is_empty());
}

/// Three big declarations of one name and one big use of it, sized so a
/// 450-token budget fits two declarations and has no room left for the use.
fn crowded() -> TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    let filler = |verb: &str| {
        (0..12)
            .map(|i| format!("    row_{i} = {verb}(item_{i}, extra_{i})"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    for n in 1..=3 {
        fs::write(
            dir.path().join(format!("decl{n}.py")),
            format!(
                "def validate_token(token):\n{}\n    return True\n",
                filler("compute")
            ),
        )
        .expect("write");
    }
    fs::write(
        dir.path().join("use.py"),
        format!(
            "def handler(request):\n{}\n    return validate_token(request)\n",
            filler("collect")
        ),
    )
    .expect("write");
    dir
}

fn reference_count(hits: &[serde_json::Value]) -> usize {
    hits.iter()
        .filter(|hit| hit["is_declaration"] == false)
        .count()
}

#[test]
fn min_references_keeps_room_for_usage() {
    let dir = crowded();
    let plain = json(
        dir.path(),
        &["-b", "450", "--max-per-file", "0", "validate_token"],
    );
    assert_eq!(
        reference_count(&plain),
        0,
        "the budget already fit a reference: {plain:?}"
    );

    let held = json(
        dir.path(),
        &[
            "-b",
            "450",
            "--max-per-file",
            "0",
            "--min-references",
            "1",
            "validate_token",
        ],
    );
    assert!(
        reference_count(&held) >= 1,
        "no reference survived: {held:?}"
    );
    // The best answer is the last thing given up, so it is still there.
    assert_eq!(held[0]["is_declaration"], true, "{held:?}");
}

#[test]
fn min_references_keeps_declarations_when_there_is_nothing_to_promote() {
    let dir = tree();
    // Nothing references `subject`, so the guarantee cannot be met and the
    // declaration it would have given up is kept instead.
    let hits = json(dir.path(), &["--min-references", "3", "subject"]);
    assert!(
        !hits.is_empty(),
        "the result was emptied chasing references"
    );
}

#[test]
fn files_from_restricts_the_search() {
    let dir = tree();
    let list = dir.path().join("list.txt");
    fs::write(
        &list,
        format!("{}\n", dir.path().join("src/api.py").display()),
    )
    .expect("write");
    let hits = json(
        dir.path(),
        &[
            "--files-from",
            list.to_str().expect("utf-8"),
            "validate_token",
        ],
    );
    assert!(!hits.is_empty());
    assert!(
        hits.iter().all(|hit| hit["path"] == "src/api.py"),
        "{hits:?}"
    );
}

#[test]
fn files_from_reads_stdin_and_takes_nul_separators() {
    let dir = tree();
    let listing = format!("{}\0", dir.path().join("src/auth.py").display());
    let (out, _, _) = run(
        dir.path(),
        &["--files-from", "-", "-0", "-l", "validate_token"],
        Some(&listing),
    );
    assert!(out.contains("src/auth.py"), "{out:?}");
    assert!(!out.contains("src/api.py"), "{out:?}");
}

#[test]
fn an_empty_file_list_searches_nothing() {
    let dir = tree();
    let (out, _, code) = run(
        dir.path(),
        &["--files-from", "-", "-l", "validate_token"],
        Some(""),
    );
    assert!(out.is_empty(), "{out:?}");
    assert_eq!(code, 1);
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("git runs");
    assert!(status.success(), "git {args:?}");
}

#[test]
fn since_searches_only_what_changed() {
    let dir = tree();
    git(dir.path(), &["init", "-q"]);
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-qm", "base"]);

    // Untouched, so nothing differs from HEAD.
    let (out, _, code) = stdout_and_code(dir.path(), &["--since", "HEAD", "-l", "validate_token"]);
    assert!(out.is_empty(), "{out:?}");
    assert_eq!(code, 1);

    fs::write(
        dir.path().join("src/api.py"),
        format!("{API}\n\ndef extra():\n    return validate_token(1)\n"),
    )
    .expect("write");
    let (out, _, _) = stdout_and_code(dir.path(), &["--since", "HEAD", "-l", "validate_token"]);
    assert!(out.contains("src/api.py"), "{out:?}");
    assert!(!out.contains("src/auth.py"), "{out:?}");
}

#[test]
fn since_ignores_commits_made_on_the_reference_after_the_split() {
    let dir = tree();
    git(dir.path(), &["init", "-q"]);
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-qm", "base"]);
    git(dir.path(), &["branch", "-m", "base"]);

    git(dir.path(), &["checkout", "-q", "-b", "feature"]);
    fs::write(
        dir.path().join("src/api.py"),
        format!("{API}\n\ndef extra():\n    return validate_token(1)\n"),
    )
    .expect("write");
    git(dir.path(), &["commit", "-qam", "mine"]);

    git(dir.path(), &["checkout", "-q", "base"]);
    fs::write(
        dir.path().join("src/auth.py"),
        format!("{AUTH}\n\ndef theirs():\n    return validate_token(2)\n"),
    )
    .expect("write");
    git(dir.path(), &["commit", "-qam", "theirs"]);
    git(dir.path(), &["checkout", "-q", "feature"]);

    let (out, _, _) = stdout_and_code(dir.path(), &["--since", "base", "-l", "validate_token"]);
    assert!(out.contains("src/api.py"), "{out:?}");
    assert!(!out.contains("src/auth.py"), "{out:?}");
}

#[test]
fn since_outside_a_repository_says_so() {
    let dir = tree();
    let (_, err, code) = run(dir.path(), &["--since", "HEAD", "validate_token"], None);
    assert_eq!(code, 2);
    assert!(err.contains("--files-from"), "{err:?}");
}

fn stdout_and_code(root: &Path, args: &[&str]) -> (String, String, i32) {
    run(root, args, None)
}

#[test]
fn fetch_returns_the_lines_an_anchor_names() {
    let dir = tree();
    let (out, _, code) = run(dir.path(), &["--fetch", "src/auth.py:1-2"], None);
    assert_eq!(code, 0);
    assert!(out.contains("src/auth.py:1-2"), "{out:?}");
    assert!(out.contains("def validate_token(token):"), "{out:?}");
    assert!(!out.contains("class Claims"), "{out:?}");
}

#[test]
fn fetch_reads_the_anchors_a_survey_printed() {
    let dir = tree();
    let survey = stdout(dir.path(), &["-l", "-d", "validate_token"]);
    assert!(!survey.is_empty());
    let (out, _, code) = run(dir.path(), &["--fetch", "-"], Some(&survey));
    assert_eq!(code, 0);
    assert!(out.contains("def validate_token(token):"), "{out:?}");
}

#[test]
fn a_malformed_anchor_is_reported_not_ignored() {
    let dir = tree();
    let (_, err, code) = run(dir.path(), &["--fetch", "src/auth.py"], None);
    assert_eq!(code, 2);
    assert!(err.contains("not an anchor"), "{err:?}");
}

/// The `path:line:column:text` fields of one `--vimgrep` line.
///
/// A Windows path opens with a drive letter, so the first colon of the line is
/// part of the path rather than a field separator.
fn vimgrep_fields(line: &str) -> (&str, u64, u64, &str) {
    let bytes = line.as_bytes();
    let drive = if bytes.get(1) == Some(&b':') && bytes[0].is_ascii_alphabetic() {
        2
    } else {
        0
    };
    let parts: Vec<&str> = line[drive..].splitn(4, ':').collect();
    assert_eq!(parts.len(), 4, "{line:?}");
    (
        &line[..drive + parts[0].len()],
        parts[1].parse().expect("a line number"),
        parts[2].parse().expect("a column"),
        parts[3],
    )
}

#[test]
fn vimgrep_prints_one_jumpable_line_per_matched_line() {
    let dir = tree();
    let out = stdout(dir.path(), &["--vimgrep", "-w", "validate_token"]);
    assert!(!out.is_empty());
    for line in out.lines() {
        let (_, line_number, column, text) = vimgrep_fields(line);
        assert!(line_number >= 1 && column >= 1, "{line:?}");
        // The column points at the match, not at the start of the line.
        assert!(
            text[(column as usize - 1)..].starts_with("validate_token"),
            "{line:?}"
        );
    }
}

/// The line unit answers "every match", so a budget meant for a context
/// window would drop most of them without saying so.
#[test]
fn vimgrep_enumerates_every_match_rather_than_filling_a_budget() {
    let dir = crowded();
    let every = stdout(dir.path(), &["--vimgrep", "-w", "validate_token"]);
    let budgeted = stdout(
        dir.path(),
        &["--vimgrep", "-b", "120", "-w", "validate_token"],
    );
    assert_eq!(every.lines().count(), 4, "{every:?}");
    assert!(
        budgeted.lines().count() < every.lines().count(),
        "an explicit -b still binds: {budgeted:?}"
    );
}

/// Quickfix knows nothing of a search root, so a path it is handed has to
/// open from the shell that ran the search.
#[test]
fn vimgrep_paths_carry_the_directory_searched() {
    let dir = tree();
    let out = stdout(dir.path(), &["--vimgrep", "-w", "validate_token"]);
    assert!(!out.is_empty());
    for line in out.lines() {
        let (path, ..) = vimgrep_fields(line);
        assert!(Path::new(path).is_file(), "{line:?}");
    }
}

#[test]
fn vimgrep_orders_by_path_then_line() {
    let dir = tree();
    let out = stdout(dir.path(), &["--vimgrep", "token"]);
    let places: Vec<(&str, u64)> = out
        .lines()
        .map(|line| {
            let (path, number, ..) = vimgrep_fields(line);
            (path, number)
        })
        .collect();
    let mut ascending = places.clone();
    ascending.sort_unstable();
    assert_eq!(places, ascending, "{out:?}");
}

#[test]
fn line_numbers_mark_matched_lines() {
    let dir = tree();
    let out = stdout(dir.path(), &["-n", "-d", "-w", "validate_token"]);
    let matched: Vec<&str> = out
        .lines()
        .filter(|line| line.trim_start().starts_with("1:"))
        .collect();
    assert_eq!(matched.len(), 1, "{out:?}");
    assert!(
        matched[0].ends_with("def validate_token(token):"),
        "{out:?}"
    );
    // Its body is context, separated by a dash the way grep writes it.
    assert!(
        out.lines().any(|line| line.trim_start().starts_with("2-")),
        "{out:?}"
    );
}

#[test]
fn without_line_numbers_the_source_is_the_files_own_bytes() {
    let dir = tree();
    let out = stdout(dir.path(), &["-d", "-w", "validate_token"]);
    assert!(out.contains("def validate_token(token):\n    return bool(token)"));
}

#[test]
fn one_pattern_reads_the_same_written_either_way() {
    let dir = tree();
    let positional = stdout(dir.path(), &["validate_token"]);
    let flagged = stdout(dir.path(), &["-e", "validate_token"]);
    assert_eq!(positional, flagged);
    // One pattern names no queries in the header; several do.
    assert!(!positional.contains(" for validate_token"));
}

/// One symbol written in four languages, one of which is its test.
fn mixed() -> TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    for (rel, body) in [
        (
            "src/auth.py",
            "def validate_token(token):\n    return bool(token)\n",
        ),
        (
            "src/auth.rs",
            "fn validate_token(token: &str) -> bool {\n    !token.is_empty()\n}\n",
        ),
        (
            "src/auth.js",
            "function validate_token(token) {\n    return Boolean(token);\n}\n",
        ),
        (
            "tests/auth_test.py",
            "def test_it():\n    assert validate_token(\"x\")\n",
        ),
    ] {
        let path = dir.path().join(rel);
        fs::create_dir_all(path.parent().expect("has a parent")).expect("mkdir");
        fs::write(path, body).expect("write");
    }
    dir
}

fn paths_of(hits: &[serde_json::Value]) -> Vec<String> {
    hits.iter()
        .map(|hit| hit["path"].as_str().unwrap_or("").to_string())
        .collect()
}

#[test]
fn lang_keeps_only_the_files_that_language_is_written_in() {
    let dir = mixed();
    let python = paths_of(&json(dir.path(), &["-t", "py", "-w", "validate_token"]));
    assert!(!python.is_empty());
    assert!(
        python.iter().all(|path| path.ends_with(".py")),
        "{python:?}"
    );

    let both = paths_of(&json(
        dir.path(),
        &["-t", "py,rust", "-w", "validate_token"],
    ));
    assert!(both.contains(&"src/auth.rs".to_string()), "{both:?}");
    assert!(!both.iter().any(|path| path.ends_with(".js")), "{both:?}");
}

/// A language nobody wrote the tree in and a language that does not exist look
/// the same from the caller's side unless one of them is an error.
#[test]
fn an_unknown_language_is_refused_rather_than_answered_empty() {
    let dir = mixed();
    let (out, err, code) = run(dir.path(), &["-t", "cobol", "validate_token"], None);
    assert_eq!(code, 2, "{out:?}");
    assert!(err.contains("unknown language"), "{err:?}");
    assert!(out.is_empty(), "{out:?}");
}

#[test]
fn tests_are_dropped_or_returned_on_their_own() {
    let dir = mixed();
    let without = paths_of(&json(dir.path(), &["--no-tests", "-w", "validate_token"]));
    assert!(!without.is_empty());
    assert!(
        !without.iter().any(|path| path.contains("auth_test")),
        "{without:?}"
    );

    let only = paths_of(&json(dir.path(), &["--tests-only", "-w", "validate_token"]));
    assert_eq!(only, vec!["tests/auth_test.py".to_string()]);
}

/// Twenty flat lines with the match in the middle: no declaration encloses it,
/// so the window is the whole of what decides the span.
fn flat() -> TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    let body: String = (1..=21)
        .map(|line| match line {
            11 => "check(TIMEOUT)\n",
            _ => "pass\n",
        })
        .collect();
    fs::write(dir.path().join("flat.py"), body).expect("write");
    dir
}

#[test]
fn orphan_context_sets_the_window_around_a_match_nothing_declares() {
    let dir = flat();
    for context in [1u64, 4] {
        let hits = json(dir.path(), &["-O", &context.to_string(), "TIMEOUT"]);
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0]["start_line"].as_u64(), Some(11 - context));
        assert_eq!(hits[0]["end_line"].as_u64(), Some(11 + context));
    }
}

#[test]
fn why_shows_what_a_score_is_made_of() {
    let dir = tree();
    let out = stdout(dir.path(), &["--why", "-w", "validate_token"]);
    assert!(out.contains("why matches "), "{out:?}");

    let hits = json(dir.path(), &["--why", "-w", "validate_token"]);
    let why = &hits[0]["why"];
    let part = |name: &str| why[name].as_f64().expect("a number");
    let total = part("matches") + part("terms") + part("path") - part("depth_penalty");
    let score = hits[0]["score"].as_f64().expect("a score");
    assert!((total - score).abs() < 1e-9, "{why:?} against {score}");

    // Nothing asked, nothing carried.
    let quiet = json(dir.path(), &["-w", "validate_token"]);
    assert!(quiet[0]["why"].is_null(), "{:?}", quiet[0]);
}
