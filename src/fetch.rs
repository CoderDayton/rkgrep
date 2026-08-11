//! Returning spans by anchor, with no pattern and no search.
//!
//! Anchors are the form rkgrep already prints, so a cheap `-l` survey and a
//! paid-for read are two halves of one flow: decide what is worth reading for
//! a couple of hundred tokens, then spend the budget only on that.

use std::io::Read;
use std::path::Path;

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

/// The lines each anchor names, in the order given, under `max_tokens`.
pub fn fetch(given: &[String], root: &Path, max_tokens: Option<usize>) -> Result<Vec<Hit>> {
    let mut hits = Vec::new();
    let mut used = 0usize;
    for anchor in anchors(given)? {
        let full = root.join(&anchor.path);
        let content = std::fs::read_to_string(&full)
            .with_context(|| format!("reading {}", full.display()))?;
        let lines: Vec<&str> = content.lines().collect();
        let from = (anchor.start - 1) as usize;
        let to = (anchor.end as usize).min(lines.len());
        if from >= to {
            bail!(
                "{}:{}-{} is past the end of a {}-line file",
                anchor.path,
                anchor.start,
                anchor.end,
                lines.len()
            );
        }
        let hit = Hit::plain(
            anchor.path.clone(),
            anchor.start,
            to as u64,
            format!("{}:{}-{}", anchor.path, anchor.start, to),
            lines[from..to].join("\n"),
        );
        if max_tokens.is_some_and(|max| used + hit.tokens() > max) {
            continue;
        }
        used += hit.tokens();
        hits.push(hit);
    }
    Ok(hits)
}
