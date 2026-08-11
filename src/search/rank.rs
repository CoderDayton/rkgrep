//! Every ordering decision, and the weights behind them.
//!
//! Ranking runs twice. Files are ordered first, from the signals the walk
//! already produced, so extraction is paid for only on files that could
//! plausibly be returned; the spans those files yield are then ordered
//! against each other.

use std::collections::HashMap;

use crate::spans::identifier_tokens;

use super::query::Query;
use super::walk::FileMatches;
use super::Hit;

/// A match on a declaration line is the strongest signal available: it is the
/// difference between where something is defined and one of forty places it is
/// mentioned. Declarations sort first outright; these weights order the rest.
const W_MATCHES: f64 = 1.0;
const W_TERMS: f64 = 1.5;
const W_PATH: f64 = 1.0;

/// Charged per level of nesting, and only against a declaration that a
/// shallower declaration of the same name is competing with.
const W_DEPTH: f64 = 1.0;

/// Ranking terms recovered from a search pattern.
pub fn query_terms(pattern: &str) -> Vec<String> {
    let mut terms: Vec<String> = Vec::new();
    let mut current = String::new();
    for c in pattern.chars() {
        if c.is_alphanumeric() || c == '_' {
            current.push(c);
        } else if !current.is_empty() {
            terms.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        terms.push(current);
    }
    let mut out: Vec<String> = Vec::new();
    for term in terms {
        for part in identifier_tokens(&term) {
            if !out.contains(&part) {
                out.push(part);
            }
        }
        let lowered = term.to_lowercase();
        if !out.contains(&lowered) {
            out.push(lowered);
        }
    }
    out
}

/// How much a file's own name looks like what was asked for.
fn path_score(path: &str, terms: &[String]) -> f64 {
    if terms.is_empty() {
        return 0.0;
    }
    let name = path.rsplit('/').next().unwrap_or(path);
    let stem = name.split('.').next().unwrap_or(name).to_lowercase();
    if terms.contains(&stem) {
        return 1.0;
    }
    let parts = identifier_tokens(&stem);
    if parts.is_empty() {
        return 0.0;
    }
    let overlap = parts.iter().filter(|p| terms.contains(p)).count();
    overlap as f64 / parts.len() as f64
}

fn term_overlap(text: &str, terms: &[String]) -> f64 {
    if terms.is_empty() {
        return 0.0;
    }
    let lowered = text.to_lowercase();
    let present = terms
        .iter()
        .filter(|t| lowered.contains(t.as_str()))
        .count();
    present as f64 / terms.len() as f64
}

/// What one span scores before the declaration bonus and the depth penalty.
pub(super) fn span_score(text: &str, terms: &[String], matches: usize, path_score: f64) -> f64 {
    W_MATCHES * ((matches + 1) as f64).ln()
        + W_TERMS * term_overlap(text, terms)
        + W_PATH * path_score
}

/// A matching file with its ranking signals resolved.
pub(super) struct Candidate {
    pub(super) file: FileMatches,
    /// How much the file's name looks like each pattern, indexed by pattern.
    pub(super) path_scores: Vec<f64>,
    /// The best of those, which is what orders candidates: a file is worth
    /// reading if it looks like the answer to any of the patterns.
    best_path_score: f64,
}

impl Candidate {
    pub(super) fn new(file: FileMatches, queries: &[Query]) -> Self {
        let path_scores: Vec<f64> = queries
            .iter()
            .map(|query| path_score(&file.path, &query.terms))
            .collect();
        let best_path_score = path_scores.iter().copied().fold(0.0, f64::max);
        Self {
            file,
            path_scores,
            best_path_score,
        }
    }
}

/// Best first. Total, because paths are unique, so the order does not depend
/// on the nondeterministic order the parallel walk produced.
pub(super) fn better_candidate(a: &Candidate, b: &Candidate) -> std::cmp::Ordering {
    b.file
        .declaration_hint
        .cmp(&a.file.declaration_hint)
        .then(b.file.match_count.cmp(&a.file.match_count))
        .then(
            b.best_path_score
                .partial_cmp(&a.best_path_score)
                .unwrap_or(std::cmp::Ordering::Equal),
        )
        .then(a.file.path.cmp(&b.file.path))
}

/// Declarations first, unconditionally. A span whose match lands on the
/// declaration line is what "where is X" asks for; letting a file that
/// mentions X twenty times outrank it by match count is how a ranker loses to
/// a one-line grep.
pub(super) fn better_hit(a: &Hit, b: &Hit) -> std::cmp::Ordering {
    b.is_declaration
        .cmp(&a.is_declaration)
        .then(
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal),
        )
        .then(a.path.cmp(&b.path))
        .then(a.start_line.cmp(&b.start_line))
}

/// Demote a declaration that a shallower declaration of the same name shadows.
pub(super) fn penalize_shadowed_declarations(hits: &mut [Hit]) {
    let mut shallowest: HashMap<&str, usize> = HashMap::new();
    for hit in hits.iter() {
        if !hit.is_declaration {
            continue;
        }
        if let Some(symbol) = &hit.symbol {
            let entry = shallowest.entry(symbol).or_insert(hit.depth);
            *entry = (*entry).min(hit.depth);
        }
    }

    let penalties: Vec<f64> = hits
        .iter()
        .map(|hit| match (hit.is_declaration, &hit.symbol) {
            (true, Some(symbol)) => shallowest
                .get(symbol.as_str())
                .map_or(0.0, |best| W_DEPTH * hit.depth.saturating_sub(*best) as f64),
            _ => 0.0,
        })
        .collect();

    for (hit, penalty) in hits.iter_mut().zip(penalties) {
        hit.score -= penalty;
    }
}
