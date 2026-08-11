//! Turning matched line numbers into the regions worth returning.
//!
//! A match expands to the declaration enclosing it rather than to a fixed
//! window, so a hit carries a whole function instead of six arbitrary lines.
//! Regions that overlap merge, so the same lines are never sent twice.

use grep_matcher::Matcher;
use grep_regex::RegexMatcher;

use crate::spans::{enclosing, kind_declares, Declaration};

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
    pub(super) is_declaration: bool,
    pub(super) depth: usize,
}

/// Coalesce overlapping regions of one file.
///
/// A match on an import line and a match inside the function below it produce
/// windows that overlap. Emitting both spends the budget twice on the same
/// lines and shows the reader the same code under two anchors.
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
                prev.matched.sort_unstable();
                prev.matched.dedup();
                prev.is_declaration |= region.is_declaration;
            }
            _ => merged.push(region),
        }
    }
    merged
}

/// Whether this span declares the thing that was searched for.
///
/// The match landing on a declaration's first line is not enough. In
/// `const a = store.createProject(...)` the match sits on the first line of a
/// declaration, but the declaration is `a` — a local binding that merely calls
/// the symbol. Counting that as a declaration hands the unconditional
/// declaration bonus to every caller written on one line, and a test file full
/// of them then outranks the file that actually declares the name.
///
/// So the pattern has to match the declared name itself. Applying the same
/// matcher keeps the test honest for regex patterns and for `-w`, under which
/// `createProject` no longer claims `createProjectManager`.
pub(super) fn declares_query(matcher: &RegexMatcher, name: Option<&str>) -> bool {
    name.is_some_and(|name| matcher.is_match(name.as_bytes()).unwrap_or(false))
}

pub(super) fn regions_for(
    decls: &[Declaration],
    lines: &[u64],
    total_lines: u64,
    matcher: &RegexMatcher,
    max_declaration_lines: u64,
) -> Vec<Region> {
    let mut sorted = lines.to_vec();
    sorted.sort_unstable();
    sorted.dedup();

    // Group match lines by the region that will contain them, so a function
    // matched eight times is one span rather than eight.
    let mut regions: Vec<Region> = Vec::new();
    for line in sorted {
        let (start, end, symbol, kind, depth) = match enclosing(decls, line) {
            Some(d) if d.end_line - d.start_line < max_declaration_lines => (
                d.start_line,
                d.end_line.min(total_lines),
                Some(d.name.clone()),
                Some(d.kind.clone()),
                d.depth,
            ),
            // Larger than the budget can admit, so returning it whole means
            // the packer drops it and the query answers nothing at all. A
            // window into a 400-line container is worth more than the
            // container is. The declaration's own first line keeps its name,
            // so "where is X declared" still ranks as a declaration instead
            // of losing to every file that merely mentions it.
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
        };
        match regions
            .iter_mut()
            .find(|r| r.start == start && r.end == end && r.symbol.is_some() == symbol.is_some())
        {
            Some(existing) => existing.matched.push(line),
            None => {
                let is_declaration = start == line
                    && kind.as_deref().is_some_and(kind_declares)
                    && declares_query(matcher, symbol.as_deref());
                regions.push(Region {
                    start,
                    end,
                    symbol,
                    kind,
                    matched: vec![line],
                    is_declaration,
                    depth,
                });
            }
        }
    }
    merge_regions(regions)
}
