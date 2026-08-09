//! Retrieval metrics, computed over a ranked list of `(path, token_cost)`.

use std::collections::BTreeSet;

/// Greedily fill the token budget from a ranked list, deduped by path.
///
/// A span too large for the remaining budget is skipped and the walk
/// continues, so one oversized file cannot truncate the result set.
pub fn under_budget(ranked: &[(String, usize)], max_tokens: usize) -> Vec<String> {
    let mut used = 0usize;
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for (path, cost) in ranked {
        if seen.contains(path) || used + cost > max_tokens {
            continue;
        }
        seen.insert(path.clone());
        used += cost;
        out.push(path.clone());
    }
    out
}

/// Reciprocal rank of `target` among the distinct paths, or 0 if absent.
pub fn mrr(ranked: &[(String, usize)], target: &str) -> f64 {
    let mut seen = BTreeSet::new();
    let mut rank = 0usize;
    for (path, _) in ranked {
        if !seen.insert(path.clone()) {
            continue;
        }
        rank += 1;
        if path == target {
            return 1.0 / rank as f64;
        }
    }
    0.0
}

/// Fraction of `relevant` present in `kept`.
pub fn coverage(kept: &[String], relevant: &BTreeSet<String>) -> f64 {
    if relevant.is_empty() {
        return 1.0;
    }
    let hit = kept.iter().filter(|p| relevant.contains(*p)).count();
    hit as f64 / relevant.len() as f64
}
