//! The last phase: fill the token budget from the ranked list.

use std::collections::HashMap;

use super::{Hit, Options};

/// Fill the token budget from the ranked list, capping any one file's share.
pub(super) fn pack(hits: Vec<Hit>, opts: &Options) -> Vec<Hit> {
    let mut used = 0usize;
    let mut per_file: HashMap<String, usize> = HashMap::new();
    let mut packed = Vec::new();
    for hit in hits {
        let count = per_file.get(&hit.path).copied().unwrap_or(0);
        if opts.max_per_file > 0 && count >= opts.max_per_file {
            continue;
        }
        // A span too large for what is left is skipped and the walk continues,
        // so one oversized declaration cannot truncate the result set.
        if opts.max_tokens.is_some_and(|max| used + hit.tokens() > max) {
            continue;
        }
        used += hit.tokens();
        per_file.insert(hit.path.clone(), count + 1);
        packed.push(hit);
        if opts.max_tokens.is_some_and(|max| used >= max) {
            break;
        }
    }
    packed
}
