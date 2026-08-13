//! Returning spans by anchor, with no pattern and no search.
//!
//! Anchors are the form rkgrep already prints, so a cheap `-l` survey and a
//! paid-for read are two halves of one flow: decide what is worth reading for
//! a couple of hundred tokens, then spend the budget only on that.

use std::io::{BufRead, Read};
use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::search::Hit;

/// A `path:start-end` reference to part of a file.
struct Anchor {
    path: String,
    start: u64,
    end: u64,
}

/// Anchors are split from the right, so a path holding a colon still parses.
fn parse(text: &str) -> Result<Anchor> {
    let malformed = || format!("not an anchor: {text} (expected path:start-end)");
    let (path, range) = text.rsplit_once(':').with_context(malformed)?;
    let (start, end) = range.split_once('-').with_context(malformed)?;
    let start: u64 = start.trim().parse().with_context(malformed)?;
    let end: u64 = end.trim().parse().with_context(malformed)?;
    if path.is_empty() || start == 0 || end < start {
        bail!(malformed());
    }
    Ok(Anchor {
        path: path.to_string(),
        start,
        end,
    })
}

/// The anchors to fetch: the ones given, or the ones on stdin for `-`.
fn anchors(given: &[String]) -> Result<Vec<Anchor>> {
    let mut text: Vec<String> = Vec::new();
    for one in given {
        if one == "-" {
            let mut buffer = String::new();
            std::io::stdin()
                .read_to_string(&mut buffer)
                .context("reading anchors from stdin")?;
            text.extend(
                buffer
                    .lines()
                    .filter_map(|line| line.split_whitespace().next())
                    .map(str::to_string),
            );
        } else {
            text.push(one.clone());
        }
    }
    text.iter().map(|one| parse(one)).collect()
}

/// The file an anchor names, which has to be one under `root`.
///
/// Anchors are read from stdin as often as they are typed, so where they point
/// is not the caller's own choosing. A path that resolves outside the tree
/// being searched is refused rather than read. An anchor carrying its own root
/// or drive is refused before the filesystem is touched, because joining it
/// discards `root` entirely.
fn resolve(root: &Path, path: &str) -> Result<PathBuf> {
    let given = Path::new(path);
    if matches!(
        given.components().next(),
        Some(Component::Prefix(_) | Component::RootDir)
    ) {
        bail!("{path} is outside {}", root.display());
    }
    let full = root.join(path);
    let resolved = full
        .canonicalize()
        .with_context(|| format!("reading {}", full.display()))?;
    if !resolved.starts_with(root) {
        bail!("{path} is outside {}", root.display());
    }
    Ok(resolved)
}

/// The lines of `path` from `start` to `end`, and how many lines it has.
///
/// Only the named range is held, so fetching six lines costs six lines however
/// large the file they came from.
fn lines_in(path: &Path, start: u64, end: u64) -> Result<(Vec<String>, u64)> {
    let file = std::fs::File::open(path).with_context(|| format!("reading {}", path.display()))?;
    let mut kept: Vec<String> = Vec::new();
    let mut total = 0u64;
    for line in std::io::BufReader::new(file).lines() {
        let line = line.with_context(|| format!("reading {}", path.display()))?;
        total += 1;
        if total >= start && total <= end {
            kept.push(line);
        }
    }
    Ok((kept, total))
}

/// The lines each anchor names, in the order given, under `max_tokens`.
pub fn fetch(given: &[String], root: &Path, max_tokens: Option<usize>) -> Result<Vec<Hit>> {
    let mut hits = Vec::new();
    let mut used = 0usize;
    for anchor in anchors(given)? {
        let full = resolve(root, &anchor.path)?;
        let (lines, total) = lines_in(&full, anchor.start, anchor.end)?;
        if lines.is_empty() {
            bail!(
                "{}:{}-{} is past the end of a {}-line file",
                anchor.path,
                anchor.start,
                anchor.end,
                total
            );
        }
        let to = anchor.end.min(total);
        let hit = Hit::plain(
            anchor.path.clone(),
            anchor.start,
            to,
            format!("{}:{}-{}", anchor.path, anchor.start, to),
            lines.join("\n"),
        );
        if max_tokens.is_some_and(|max| used + hit.tokens() > max) {
            continue;
        }
        used += hit.tokens();
        hits.push(hit);
    }
    Ok(hits)
}
