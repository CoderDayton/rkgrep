//! Turning matched line numbers into the regions worth returning.
//!
//! A match expands to the declaration enclosing it rather than to a fixed
//! window, so a hit carries a whole function instead of six arbitrary lines.
//! Regions that overlap merge, so the same lines are never sent twice.

use std::collections::HashMap;

use grep_matcher::Matcher;
use grep_regex::RegexMatcher;

use crate::spans::{enclosing, kind_declares, Declaration};

use super::query::Query;

/// Window for matches outside any declaration: imports, top-level constants,
/// configuration literals. Also what a declaration too large to return is
/// clamped to.
pub const ORPHAN_CONTEXT: u64 = 6;

/// A region of one file that will become a [`super::Hit`], before scoring.
pub(super) struct Region {
    pub(super) start: u64,
    pub(super) end: u64,
    pub(super) symbol: Option<String>,
    pub(super) kind: Option<String>,
    pub(super) matched: Vec<u64>,
    pub(super) depth: usize,
    /// Matched lines contributed by each pattern, indexed by pattern.
    per_query: Vec<usize>,
    /// The earliest pattern this region declares, if it declares one.
    declaring: Option<usize>,
}

impl Region {
    /// A match landing on the declaration line of a symbol the pattern itself
    /// matches; see [`declares_query`].
    pub(super) fn is_declaration(&self) -> bool {
        self.declaring.is_some()
    }

    /// Every pattern with a matched line in this region.
    pub(super) fn answered(&self) -> Vec<usize> {
        self.per_query
            .iter()
            .enumerate()
            .filter(|(_, count)| **count > 0)
            .map(|(index, _)| index)
            .collect()
    }

    /// The pattern this region answers.
    pub(super) fn owner(&self) -> usize {
        self.declaring.unwrap_or_else(|| {
            self.per_query
                .iter()
                .enumerate()
                .max_by_key(|(index, count)| (**count, std::cmp::Reverse(*index)))
                .map_or(0, |(index, _)| index)
        })
    }
}

/// Coalesce overlapping regions of one file.
fn merge_regions(mut regions: Vec<Region>) -> Vec<Region> {
    regions.sort_by_key(|r| (r.start, r.end));
    let mut merged: Vec<Region> = Vec::with_capacity(regions.len());
    for region in regions {
        match merged.last_mut() {
            Some(prev) if region.start <= prev.end => {
                prev.end = prev.end.max(region.end);
                if prev.symbol.is_none() {
                    prev.symbol = region.symbol;
                    prev.kind = region.kind;
                    prev.depth = region.depth;
                }
                prev.matched.extend(region.matched);
                for (total, add) in prev.per_query.iter_mut().zip(&region.per_query) {
                    *total += add;
                }
                prev.declaring = match (prev.declaring, region.declaring) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (found, None) | (None, found) => found,
                };
            }
            _ => merged.push(region),
        }
    }
    // Once, at the end: merging concatenates two sorted runs, and sorting
    // after every one of them is how a file of many small declarations turns
    // quadratic.
    for region in &mut merged {
        region.matched.sort_unstable();
        region.matched.dedup();
    }
    merged
}

/// Whether this span declares the thing that was searched for.
pub(super) fn declares_query(matcher: &RegexMatcher, name: Option<&str>) -> bool {
    name.is_some_and(|name| matcher.is_match(name.as_bytes()).unwrap_or(false))
}

/// The lines one match is worth returning, and what declares them.
fn window_for(
    decls: &[Declaration],
    line: u64,
    total_lines: u64,
    max_declaration_lines: u64,
) -> (u64, u64, Option<String>, Option<String>, usize) {
    match enclosing(decls, line) {
        Some(d) if d.end_line - d.start_line < max_declaration_lines => (
            d.start_line,
            d.end_line.min(total_lines),
            Some(d.name.clone()),
            Some(d.kind.clone()),
            d.depth,
        ),
        Some(d) => {
            let names = line == d.start_line;
            (
                line.saturating_sub(ORPHAN_CONTEXT).max(d.start_line),
                (line + ORPHAN_CONTEXT).min(d.end_line).min(total_lines),
                names.then(|| d.name.clone()),
                names.then(|| d.kind.clone()),
                if names { d.depth } else { 0 },
            )
        }
        None => (
            line.saturating_sub(ORPHAN_CONTEXT).max(1),
            (line + ORPHAN_CONTEXT).min(total_lines),
            None,
            None,
            0,
        ),
    }
}

/// Regions for one file, from lines already paired with the pattern that
/// matched them.
pub(super) fn regions_for(
    decls: &[Declaration],
    lines: &[(u64, usize)],
    total_lines: u64,
    queries: &[Query],
    max_declaration_lines: u64,
) -> Vec<Region> {
    let mut sorted = lines.to_vec();
    sorted.sort_unstable();
    sorted.dedup();

    let mut regions: Vec<Region> = Vec::new();
    // Which region each window already belongs to. A file of fifty thousand
    // one-line functions produces a region per match, and finding the right
    // one by scanning them all is quadratic in that count.
    let mut seen: HashMap<(u64, u64, bool), usize> = HashMap::new();
    for (line, query) in sorted {
        let (start, end, symbol, kind, depth) =
            window_for(decls, line, total_lines, max_declaration_lines);
        let declares = start == line
            && kind.as_deref().is_some_and(kind_declares)
            && declares_query(&queries[query].matcher, symbol.as_deref());
        let window = (start, end, symbol.is_some());
        match seen.get(&window).copied() {
            Some(at) => {
                let existing = &mut regions[at];
                existing.matched.push(line);
                existing.per_query[query] += 1;
                if declares {
                    existing.declaring = Some(existing.declaring.map_or(query, |a| a.min(query)));
                }
            }
            None => {
                seen.insert(window, regions.len());
                let mut per_query = vec![0usize; queries.len()];
                per_query[query] = 1;
                regions.push(Region {
                    start,
                    end,
                    symbol,
                    kind,
                    matched: vec![line],
                    depth,
                    per_query,
                    declaring: declares.then_some(query),
                });
            }
        }
    }
    merge_regions(regions)
}
