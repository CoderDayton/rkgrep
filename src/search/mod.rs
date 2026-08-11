//! Ranked, span-scoped, budget-packed search over ripgrep's engine.
//!
//! ripgrep answers "which lines match" better than anything else, and this
//! links its engine directly rather than shelling out: no process spawn, no
//! serialization round-trip, and line numbers arrive as integers.
//!
//! What ripgrep does not do is decide which of two hundred hits are worth
//! reading, how much of the surrounding code to include, or when to stop.
//! Each step below is one module:
//!
//! 1. [`walk`] runs the engine over the tree and collects the matching files
//! 2. [`region`] expands each match to the declaration enclosing it, not a
//!    fixed window, and merges the overlaps
//! 3. [`extract`] reads candidates best-first and turns regions into spans
//! 4. [`rank`] orders files and then spans, declarations first
//! 5. [`pack`] fills the token budget from the ranked list
//!
//! [`search`] is the only entry point; this module owns the types that cross
//! every phase.

mod extract;
mod pack;
mod rank;
mod region;
mod walk;

use std::cell::OnceCell;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use serde::Serialize;

use crate::tokenizer::count as count_tokens;

pub use rank::query_terms;

#[derive(Debug, Clone)]
pub struct Hit {
    pub path: String,
    pub start_line: u64,
    pub end_line: u64,
    pub symbol: Option<String>,
    pub kind: Option<String>,
    pub match_lines: Vec<u64>,
    pub is_declaration: bool,
    /// Nesting depth of the declaration this span came from; 0 for top level.
    pub depth: usize,
    /// Filled on demand by [`Hit::tokens`], never at construction.
    pub(in crate::search) tokens: OnceCell<usize>,
    pub score: f64,
    pub text: String,
}

impl Hit {
    pub fn anchor(&self) -> String {
        format!("{}:{}-{}", self.path, self.start_line, self.end_line)
    }

    /// What this span costs against the budget.
    ///
    /// Extraction produces spans by the hundred and the budget admits a
    /// handful, so counting one at construction spends most of the query on
    /// spans that are then discarded. The packer asks in score order and stops
    /// when the budget is full, which is the only order in which the question
    /// is worth answering.
    pub fn tokens(&self) -> usize {
        *self.tokens.get_or_init(|| count_tokens(&self.text))
    }
}

/// Written out rather than derived, so `tokens` is the count and not the cell
/// that may or may not be holding it yet.
impl Serialize for Hit {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut hit = serializer.serialize_struct("Hit", 11)?;
        hit.serialize_field("path", &self.path)?;
        hit.serialize_field("start_line", &self.start_line)?;
        hit.serialize_field("end_line", &self.end_line)?;
        hit.serialize_field("symbol", &self.symbol)?;
        hit.serialize_field("kind", &self.kind)?;
        hit.serialize_field("match_lines", &self.match_lines)?;
        hit.serialize_field("is_declaration", &self.is_declaration)?;
        hit.serialize_field("depth", &self.depth)?;
        hit.serialize_field("tokens", &self.tokens())?;
        hit.serialize_field("score", &self.score)?;
        hit.serialize_field("text", &self.text)?;
        hit.end()
    }
}

/// Where the time went, for `--stats`.
///
/// The walk is the parallel phase and everything after it is serial, so this
/// is what says whether a slow query is short of cores or short of a better
/// ranking cut -- the two have opposite fixes.
#[derive(Debug, Clone, Copy, Default)]
pub struct Timings {
    /// Parallel: walk the tree, search every file, collect the matching ones.
    pub walk: Duration,
    /// Serial: every ordering decision — candidates by the signals the walk
    /// produced, then the spans those candidates yielded.
    pub rank: Duration,
    /// Serial: read candidates, extract declarations, resolve line numbers.
    pub extract: Duration,
    /// Files the walk found matching, before any candidate cut.
    pub matching_files: usize,
    /// Files actually opened and turned into spans.
    pub read_files: usize,
}

#[derive(Debug, Clone)]
pub struct Options {
    /// Token budget the whole result set must fit, or `None` to return every
    /// ranked span. Without a budget rkgrep stands in for `rg`: nothing is
    /// dropped for size, and extraction runs on every matching file rather
    /// than only on the ones that could plausibly be returned.
    pub max_tokens: Option<usize>,
    /// Cap on spans from any one file; 0 means no cap.
    pub max_per_file: usize,
    pub globs: Vec<String>,
    /// Match only inside comments. The span returned is still the declaration
    /// the comment sits in, so a hit carries the code it describes.
    pub comments_only: bool,
    pub literal: bool,
    pub word: bool,
    pub ignore_case: bool,
    pub hidden: bool,
    pub no_ignore: bool,
    pub threads: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            max_tokens: Some(2000),
            max_per_file: 3,
            globs: Vec::new(),
            comments_only: false,
            literal: false,
            word: false,
            ignore_case: false,
            hidden: false,
            no_ignore: false,
            threads: 0,
        }
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

/// Ranked spans matching `pattern`, packed under `opts.max_tokens`, and a
/// breakdown of where the time went.
pub fn search(pattern: &str, root: &Path, opts: &Options) -> Result<(Vec<Hit>, Timings)> {
    let mut timings = Timings::default();
    let matcher = build_matcher(pattern, opts)?;

    let started = Instant::now();
    let mut files = walk::collect_matches(&matcher, root, opts)?;
    timings.walk = started.elapsed();
    timings.matching_files = files.len();

    let started = Instant::now();
    let terms = query_terms(pattern);

    // Rank files before reading any of them, so span extraction is paid for
    // only on files that could plausibly be returned. The signals here are
    // whatever the walk already produced: whether a matched line looked like
    // a declaration, how many matches there were, and the file's own name.
    let mut ranked: Vec<rank::Candidate> = files
        .drain(..)
        .map(|file| rank::Candidate {
            path_score: rank::path_score(&file.path, &terms),
            file,
        })
        .collect();
    timings.rank = started.elapsed();

    let mut hits = extract::gather_spans(&mut ranked, &matcher, &terms, opts, &mut timings);

    let ordering = Instant::now();
    rank::penalize_shadowed_declarations(&mut hits);

    // Declarations first, unconditionally. A span whose match lands on the
    // declaration line is what "where is X" asks for; letting a file that
    // mentions X twenty times outrank it by match count is how a ranker loses
    // to a one-line grep.
    hits.sort_by(rank::better_hit);
    timings.rank += ordering.elapsed();

    Ok((pack::pack(hits, opts), timings))
}

/// Search roots that do not exist are the caller's error, not a walk failure.
pub fn validate_root(root: &Path) -> Result<PathBuf> {
    root.canonicalize()
        .with_context(|| format!("no such path: {}", root.display()))
}
