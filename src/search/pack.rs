//! The last phase: fill the token budget from the ranked list.

use std::collections::{HashMap, HashSet};

use super::{Hit, Options};

/// Interleave the ranked spans so every pattern is answered before any pattern
/// is answered twice.
fn interleave(hits: Vec<Hit>, queries: usize) -> Vec<Hit> {
    if queries < 2 {
        return hits;
    }
    let mut lanes: Vec<Vec<Hit>> = (0..queries).map(|_| Vec::new()).collect();
    for hit in hits {
        lanes[hit.query_index].push(hit);
    }
    let mut lanes: Vec<_> = lanes.into_iter().map(Vec::into_iter).collect();
    let mut ordered = Vec::new();
    loop {
        let mut served = false;
        for lane in &mut lanes {
            if let Some(hit) = lane.next() {
                ordered.push(hit);
                served = true;
            }
        }
        if !served {
            return ordered;
        }
    }
}

/// Which spans fit the budget, best first, skipping anything in `barred`.
fn fill(ordered: &[Hit], opts: &Options, barred: &HashSet<usize>) -> Vec<usize> {
    let mut used = 0usize;
    let mut per_file: HashMap<&str, usize> = HashMap::new();
    let mut chosen = Vec::new();
    for (at, hit) in ordered.iter().enumerate() {
        if barred.contains(&at) {
            continue;
        }
        let count = per_file.get(hit.path.as_str()).copied().unwrap_or(0);
        if opts.max_per_file > 0 && count >= opts.max_per_file {
            continue;
        }
        if let Some(max) = opts.max_tokens {
            let cost = hit.tokens();
            if used + cost > max {
                continue;
            }
            used += cost;
        }
        per_file.insert(hit.path.as_str(), count + 1);
        chosen.push(at);
        if opts.max_tokens.is_some_and(|max| used >= max) {
            break;
        }
    }
    chosen
}

fn references(ordered: &[Hit], chosen: &[usize]) -> usize {
    chosen
        .iter()
        .filter(|at| !ordered[**at].is_declaration)
        .count()
}

/// Give up declarations, lowest-ranked first, until enough references fit.
fn promote_references(ordered: &[Hit], opts: &Options, plain: Vec<usize>) -> Vec<usize> {
    let mut barred: HashSet<usize> = HashSet::new();
    let mut trial = plain.clone();
    loop {
        let Some(give_up) = trial
            .iter()
            .rev()
            .copied()
            .find(|at| ordered[*at].is_declaration)
        else {
            return plain;
        };
        barred.insert(give_up);
        trial = fill(ordered, opts, &barred);
        if references(ordered, &trial) >= opts.min_references {
            return trial;
        }
    }
}

/// Fill the token budget from the ranked list, capping any one file's share.
pub(super) fn pack(hits: Vec<Hit>, opts: &Options, queries: usize) -> Vec<Hit> {
    let ordered = interleave(hits, queries);
    let plain = fill(&ordered, opts, &HashSet::new());
    let chosen = match references(&ordered, &plain) < opts.min_references {
        true => promote_references(&ordered, opts, plain),
        false => plain,
    };

    let keep: HashSet<usize> = chosen.into_iter().collect();
    ordered
        .into_iter()
        .enumerate()
        .filter(|(at, _)| keep.contains(at))
        .map(|(_, hit)| hit)
        .collect()
}
