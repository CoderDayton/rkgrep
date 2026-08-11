//! One compiled pattern, and the matcher the walk uses for all of them.
//!
//! Each pattern keeps its own matcher and its own ranking terms, because a
//! span has to be attributed to the pattern it answers and `claims.rs` should
//! score high for `Claims` and not for `refresh`. The walk does not need any
//! of that: it only decides which files are worth opening, so it runs one
//! matcher that accepts any of the patterns.

use anyhow::{Context, Result};
use grep_regex::{RegexMatcher, RegexMatcherBuilder};

use super::rank::query_terms;
use super::Options;

#[derive(Clone)]
pub(super) struct Query {
    /// The pattern as the caller wrote it, for output.
    pub(super) pattern: String,
    pub(super) matcher: RegexMatcher,
    pub(super) terms: Vec<String>,
}

/// Every pattern of one run, plus the matcher the walk searches with.
pub(super) struct Queries {
    pub(super) list: Vec<Query>,
    pub(super) scout: RegexMatcher,
}

impl Queries {
    pub(super) fn len(&self) -> usize {
        self.list.len()
    }
}

fn build_matcher(pattern: &str, opts: &Options) -> Result<RegexMatcher> {
    RegexMatcherBuilder::new()
        .case_insensitive(opts.ignore_case)
        .word(opts.word)
        .fixed_strings(opts.literal)
        .build(pattern)
        .with_context(|| format!("invalid pattern: {pattern}"))
}

/// The matcher the walk searches with: any one of the patterns.
fn build_scout(patterns: &[String], opts: &Options) -> Result<RegexMatcher> {
    if let [only] = patterns {
        return build_matcher(only, opts);
    }
    let alternation = patterns
        .iter()
        .map(|pattern| {
            let body = match opts.literal {
                true => regex::escape(pattern),
                false => pattern.clone(),
            };
            format!("(?:{body})")
        })
        .collect::<Vec<_>>()
        .join("|");
    RegexMatcherBuilder::new()
        .case_insensitive(opts.ignore_case)
        .word(opts.word)
        .build(&alternation)
        .with_context(|| format!("invalid pattern set: {}", patterns.join(", ")))
}

/// Compile every pattern of one run.
pub(super) fn compile(patterns: &[String], opts: &Options) -> Result<Queries> {
    let list = patterns
        .iter()
        .map(|pattern| {
            Ok(Query {
                pattern: pattern.clone(),
                matcher: build_matcher(pattern, opts)?,
                terms: query_terms(pattern),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Queries {
        scout: build_scout(patterns, opts)?,
        list,
    })
}
