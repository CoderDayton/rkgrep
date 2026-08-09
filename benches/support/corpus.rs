//! The file set a benchmark runs over, and what it costs to put in a context
//! window.

use std::collections::BTreeMap;
use std::path::Path;

use ignore::WalkBuilder;

/// Payload size in the unit `--max-tokens` is denominated in.
///
/// Mirrors `estimate_tokens` in `src/spans.rs`, which benchmarks cannot call
/// because rkgrep builds only a binary target. Both sides of every comparison
/// have to be charged by the same rule, so the two must agree; if the one in
/// `src/spans.rs` changes, change this with it.
pub fn estimate_tokens(text: &str) -> usize {
    let mut count = 0usize;
    let mut in_run = false;
    for &b in text.as_bytes() {
        let is_word = b.is_ascii_alphanumeric() || b == b'_';
        if is_word && !in_run {
            count += 1;
        }
        in_run = is_word;
    }
    count.max(1)
}

/// Relative path -> source text for every file under `root` with one of
/// `suffixes`.
///
/// Walks with the same crate rkgrep does, so the benchmark corpus is the set
/// of files rkgrep would actually consider: ignore rules included, and
/// vendored trees excluded exactly where the repository says they are.
/// Unreadable and non-UTF-8 files are skipped, not counted.
pub fn collect(root: &Path, suffixes: &[&str]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for entry in WalkBuilder::new(root).build().flatten() {
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        let matches = path
            .to_str()
            .is_some_and(|p| suffixes.iter().any(|s| p.ends_with(s)));
        if !matches {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let Ok(rel) = path.strip_prefix(root) else {
            continue;
        };
        if let Some(rel) = rel.to_str() {
            out.insert(rel.to_string(), text);
        }
    }
    out
}
