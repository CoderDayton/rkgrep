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
//! A run may carry several patterns. [`query`] compiles one matcher per
//! pattern plus one for the walk, so the parallel phase stays a single pass
//! and per-pattern work happens where the file is already open.
//!
//! [`search`] is the only entry point; this module owns the types that cross
//! every phase.

mod extract;
mod filter;
mod pack;
mod query;
mod rank;
mod region;
mod walk;

use std::cell::OnceCell;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::Serialize;

use crate::tokenizer::count as count_tokens;

pub use filter::{extensions_for, language_names, Tests};
pub use rank::Reasons;
pub use region::DEFAULT_ORPHAN_CONTEXT;

#[derive(Debug, Clone)]
pub struct Hit {
    pub path: String,
    pub start_line: u64,
    pub end_line: u64,
    pub symbol: Option<String>,
    pub kind: Option<String>,
    pub match_lines: Vec<u64>,
    /// 1-based byte column of the first match on each line of `match_lines`,
    /// resolved only when [`Options::columns`] asks for it.
    pub match_columns: Vec<u64>,
    pub is_declaration: bool,
    /// Nesting depth of the declaration this span came from; 0 for top level.
    pub depth: usize,
    /// The pattern this span answers, as the caller wrote it.
    pub query: String,
    /// Which pattern that is, for round-robin packing.
    pub(in crate::search) query_index: usize,
    /// Every pattern with a matched line in this span. A span has one owner
    /// but can answer several patterns, so this is what says a pattern
    /// matched, and `query_index` only says who gets to speak for it.
    pub(in crate::search) answered: Vec<usize>,
    /// Filled on demand by [`Hit::tokens`], never at construction.
    pub(in crate::search) tokens: OnceCell<usize>,
    pub score: f64,
    /// What that score is made of, resolved only when `--why` asks for it.
    pub reasons: Option<Reasons>,
    pub text: String,
}

impl Hit {
    pub fn anchor(&self) -> String {
        format!("{}:{}-{}", self.path, self.start_line, self.end_line)
    }

    /// What this span costs against the budget.
    pub fn tokens(&self) -> usize {
        *self.tokens.get_or_init(|| count_tokens(&self.text))
    }

    /// A span with no search behind it, which is what `--fetch` returns.
    pub fn plain(
        path: String,
        start_line: u64,
        end_line: u64,
        query: String,
        text: String,
    ) -> Self {
        Self {
            path,
            start_line,
            end_line,
            symbol: None,
            kind: None,
            match_lines: Vec::new(),
            match_columns: Vec::new(),
            is_declaration: false,
            depth: 0,
            query,
            query_index: 0,
            answered: Vec::new(),
            tokens: OnceCell::new(),
            score: 0.0,
            reasons: None,
            text,
        }
    }
}

/// Written out rather than derived, so `tokens` is the count and not the cell
/// that may or may not be holding it yet, and so columns are absent rather
/// than empty when nothing asked for them.
impl Serialize for Hit {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let columns = !self.match_columns.is_empty();
        let fields = 12 + usize::from(columns) + usize::from(self.reasons.is_some());
        let mut hit = serializer.serialize_struct("Hit", fields)?;
        hit.serialize_field("path", &self.path)?;
        hit.serialize_field("start_line", &self.start_line)?;
        hit.serialize_field("end_line", &self.end_line)?;
        hit.serialize_field("symbol", &self.symbol)?;
        hit.serialize_field("kind", &self.kind)?;
        hit.serialize_field("match_lines", &self.match_lines)?;
        if columns {
            hit.serialize_field("match_columns", &self.match_columns)?;
        }
        hit.serialize_field("is_declaration", &self.is_declaration)?;
        hit.serialize_field("depth", &self.depth)?;
        hit.serialize_field("query", &self.query)?;
        hit.serialize_field("tokens", &self.tokens())?;
        hit.serialize_field("score", &self.score)?;
        if let Some(reasons) = &self.reasons {
            hit.serialize_field("why", reasons)?;
        }
        hit.serialize_field("text", &self.text)?;
        hit.end()
    }
}

/// Which spans a run is asking for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Select {
    /// Declarations and references both, declarations ranked first.
    #[default]
    All,
    /// Only spans that declare a queried symbol — "where is this defined".
    Declarations,
    /// Only spans that do not — "who uses this".
    References,
}

/// Where the time went, for `--stats`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Timings {
    /// Parallel: walk the tree, search every file, collect the matching ones.
    pub walk: Duration,
    /// Serial: every ordering decision — candidates by the signals the walk
    /// produced, then the spans those candidates yielded.
    pub rank: Duration,
    /// Serial: read candidates, extract declarations, resolve line numbers.
    pub extract: Duration,
    /// Serial: choose which spans fit the budget, counting what each costs.
    pub pack: Duration,
    /// Serial: write the result set out, including the token counts the output
    /// itself asks for. Filled in by the caller, which is what does the
    /// writing.
    pub render: Duration,
    /// Files the walk found matching, before any candidate cut.
    pub matching_files: usize,
    /// Files actually opened and turned into spans.
    pub read_files: usize,
}

/// What one run produced.
pub struct Results {
    pub hits: Vec<Hit>,
    pub timings: Timings,
    /// Patterns that yielded no span at all, before any filter or budget. With
    /// several patterns a typo in one is otherwise invisible.
    pub unmatched: Vec<String>,
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
    /// Search only these files. `None` walks the root; an empty list searches
    /// nothing, which is what a `--since` with no changes means.
    pub paths: Option<Vec<PathBuf>>,
    /// Match only inside comments. The span returned is still the declaration
    /// the comment sits in, so a hit carries the code it describes.
    pub comments_only: bool,
    /// Which spans to return.
    pub select: Select,
    /// Declaring kinds to keep; empty keeps every kind.
    pub kinds: Vec<String>,
    /// Budget reserved for at least this many non-declaration spans.
    pub min_references: usize,
    /// Resolve the column of each match. Costs a matcher call per matched
    /// line, so it is paid for only when `--vimgrep` asks.
    pub columns: bool,
    /// Keep the score breakdown of every span, for `--why`.
    pub explain: bool,
    /// Extensions the walk keeps; empty keeps every file `--glob` allowed.
    pub extensions: Vec<&'static str>,
    /// What to do with the test files the walk finds.
    pub tests: Tests,
    /// Lines returned either side of a match that no declaration encloses,
    /// and the window a declaration too large to return is clamped to.
    pub orphan_context: u64,
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
            paths: None,
            comments_only: false,
            select: Select::All,
            kinds: Vec::new(),
            min_references: 0,
            columns: false,
            explain: false,
            extensions: Vec::new(),
            tests: Tests::default(),
            orphan_context: DEFAULT_ORPHAN_CONTEXT,
            literal: false,
            word: false,
            ignore_case: false,
            hidden: false,
            no_ignore: false,
            threads: 0,
        }
    }
}

/// Whether a span survives `--declarations`, `--references` and `--kind`.
fn selected(hit: &Hit, opts: &Options) -> bool {
    let by_select = match opts.select {
        Select::All => true,
        Select::Declarations => hit.is_declaration,
        Select::References => !hit.is_declaration,
    };
    let by_kind = opts.kinds.is_empty()
        || hit
            .kind
            .as_deref()
            .is_some_and(|kind| opts.kinds.iter().any(|want| want == kind));
    by_select && by_kind
}

/// Ranked spans matching `patterns`, packed under `opts.max_tokens`, and a
/// breakdown of where the time went.
pub fn search(patterns: &[String], root: &Path, opts: &Options) -> Result<Results> {
    let mut timings = Timings::default();
    let queries = query::compile(patterns, opts)?;

    let started = Instant::now();
    let mut files = walk::collect_matches(&queries.scout, root, opts)?;
    timings.walk = started.elapsed();
    timings.matching_files = files.len();

    let started = Instant::now();

    let mut ranked: Vec<rank::Candidate> = files
        .drain(..)
        .map(|file| rank::Candidate::new(file, &queries.list))
        .collect();
    timings.rank = started.elapsed();

    let mut hits = extract::gather_spans(&mut ranked, &queries, opts, &mut timings);

    let ordering = Instant::now();
    rank::penalize_shadowed_declarations(&mut hits);

    hits.sort_by(rank::better_hit);
    timings.rank += ordering.elapsed();

    let unmatched = queries
        .list
        .iter()
        .enumerate()
        .filter(|(index, _)| !hits.iter().any(|hit| hit.answered.contains(index)))
        .map(|(_, query)| query.pattern.clone())
        .collect();

    hits.retain(|hit| selected(hit, opts));

    let packing = Instant::now();
    let hits = pack::pack(hits, opts, queries.len());
    timings.pack = packing.elapsed();

    Ok(Results {
        hits,
        timings,
        unmatched,
    })
}

/// Search roots that do not exist are the caller's error, not a walk failure.
pub fn validate_root(root: &Path) -> Result<PathBuf> {
    root.canonicalize()
        .with_context(|| format!("no such path: {}", root.display()))
}
