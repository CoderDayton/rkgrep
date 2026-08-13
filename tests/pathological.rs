//! Legal inputs that are hostile to the implementation rather than to the
//! reader: one line holding a megabyte, a file of a hundred thousand
//! declarations, an anchor pointing out of the tree, bytes that are not text.
//!
//! Each of these was a real defect. The timing assertions are loose on purpose
//! — they are here to catch a return to quadratic, which costs minutes, not to
//! measure anything.

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

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

fn tree_with(name: &str, body: &[u8]) -> TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    fs::write(dir.path().join(name), body).expect("write");
    dir
}

/// A minified bundle, a base64 blob or a generated data file is one very long
/// piece to the tokenizer, and it is counted for the `[N tok]` header on the
/// default path. Merging it pair by pair used to cost minutes.
#[test]
fn a_file_that_is_one_long_line_does_not_stall() {
    let body = format!("def needle():\n    x = \"{}\"\n", "A".repeat(2_000_000));
    let dir = tree_with("long.py", body.as_bytes());

    let started = Instant::now();
    let (out, _, code) = run(dir.path(), &["-a", "needle"], None);
    let took = started.elapsed();

    assert_eq!(code, 0, "{out:?}");
    assert!(out.contains("long.py"), "{out:?}");
    assert!(
        took < Duration::from_secs(60),
        "one long line took {took:?}"
    );
}

/// Generated bindings and vendored files reach this shape: every match lands
/// in its own declaration, so the run builds one region per match.
#[test]
fn a_file_of_many_declarations_does_not_stall() {
    let body: String = (0..60_000)
        .map(|i| format!("def f_{i}():\n    return needle\n\n"))
        .collect();
    let dir = tree_with("many.py", body.as_bytes());

    let started = Instant::now();
    let (out, _, code) = run(dir.path(), &["--vimgrep", "needle"], None);
    let took = started.elapsed();

    assert_eq!(code, 0);
    assert_eq!(out.lines().count(), 60_000, "one line per match");
    assert!(
        took < Duration::from_secs(60),
        "many declarations took {took:?}"
    );
}

/// Anchors arrive on stdin as often as they are typed, so where they point is
/// not the caller's own choosing.
#[test]
fn an_anchor_outside_the_root_is_refused() {
    let dir = tree_with("a.py", b"def f():\n    return 1\n");
    let outside = "/etc/passwd:1-1";

    let (out, err, code) = run(dir.path(), &["--fetch", outside], None);
    assert_eq!(code, 2, "{out:?}");
    assert!(err.contains("outside"), "{err:?}");
    assert!(out.is_empty(), "{out:?}");

    let climbing = "../../../../../../../../etc/passwd:1-1";
    let (out, err, code) = run(dir.path(), &["--fetch", climbing], None);
    assert_eq!(code, 2, "{out:?}");
    assert!(
        err.contains("outside") || err.contains("reading"),
        "{err:?}"
    );

    let (out, _, code) = run(dir.path(), &["--fetch", "-"], Some(outside));
    assert_eq!(code, 2, "{out:?}");
    assert!(out.is_empty(), "{out:?}");
}

#[test]
fn an_anchor_inside_the_root_is_still_served() {
    let dir = tree_with("a.py", b"def f():\n    return 1\n");
    let (out, _, code) = run(dir.path(), &["--fetch", "a.py:1-2"], None);
    assert_eq!(code, 0, "{out:?}");
    assert!(out.contains("return 1"), "{out:?}");
}

/// git reads a leading dash as an option, and `git diff` has options that
/// write files.
#[test]
fn a_reference_that_looks_like_a_flag_is_refused() {
    let dir = tree_with("a.py", b"def f():\n    return 1\n");
    let (out, err, code) = run(
        dir.path(),
        &["--since=--output=/tmp/rkgrep-test", "f"],
        None,
    );
    assert_eq!(code, 2, "{out:?}");
    assert!(err.contains("not a revision"), "{err:?}");
    assert!(
        !Path::new("/tmp/rkgrep-test").exists(),
        "git wrote a file it was asked for on the command line"
    );
}

/// Reading a file rkgrep will not extract from costs memory and buys nothing,
/// so it is skipped — and a search that returns less than it found says so.
#[test]
fn a_file_over_the_size_cap_is_skipped_and_said_so() {
    let body = format!(
        "def needle():\n    x = \"{}\"\n",
        "a b c ".repeat(1_600_000)
    );
    assert!(
        body.len() > 8 * 1024 * 1024,
        "the fixture must exceed the cap"
    );
    let dir = tree_with("huge.py", body.as_bytes());

    let (out, err, _) = run(dir.path(), &["-a", "needle"], None);
    assert!(out.is_empty(), "{out:?}");
    assert!(err.contains("skipped 1 files over 8 MiB"), "{err:?}");
}

/// None of these should be readable as source, and none of them should end the
/// process any way but cleanly.
#[test]
fn hostile_bytes_do_not_crash() {
    let dir = tempfile::tempdir().expect("temp dir");
    let deep: String = (1..150)
        .map(|i| format!("{}def n{i}():\n", "    ".repeat(i)))
        .collect();
    for (name, body) in [
        (
            "invalid.py",
            b"def needle():\n    return \"\xff\xfe\x80\"\n".to_vec(),
        ),
        (
            "nul.py",
            b"def needle():\n\x00\x00\n    return 1\n".to_vec(),
        ),
        ("crlf.py", b"def needle():\r\n    return 1\r\n".to_vec()),
        (
            "bom.py",
            b"\xef\xbb\xbfdef needle():\n    return 1\n".to_vec(),
        ),
        ("noeol.py", b"def needle(): return 1".to_vec()),
        (
            "unicode.py",
            "def needle():\n    s = \"\u{e9}\u{4e2d}\u{1f600}\u{301}\"\n"
                .as_bytes()
                .to_vec(),
        ),
        (
            "blank.py",
            [b"\n".repeat(20_000), b"def needle():\n    pass\n".to_vec()].concat(),
        ),
        ("deep.py", format!("def needle():\n{deep}").into_bytes()),
    ] {
        fs::write(dir.path().join(name), body).expect("write");
    }

    for args in [
        vec!["needle"],
        vec!["-a", "needle"],
        vec!["-n", "-a", "needle"],
        vec!["--json", "-a", "needle"],
        vec!["--vimgrep", "needle"],
        vec!["-C", "-a", "needle"],
        vec!["-d", "-a", "needle"],
        vec!["-l", "needle"],
    ] {
        let (_, err, code) = run(dir.path(), &args, None);
        assert!(
            code == 0 || code == 1,
            "rkgrep {args:?} exited {code}: {err:?}"
        );
        assert!(!err.contains("panicked"), "rkgrep {args:?}: {err:?}");
    }
}
