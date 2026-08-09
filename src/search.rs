//! Ranked, span-scoped, budget-packed search over ripgrep's engine.
//!
//! ripgrep answers "which lines match" better than anything else, and this
//! links its engine directly rather than shelling out: no process spawn, no
//! serialization round-trip, and line numbers arrive as integers.
//!
//! What ripgrep does not do is decide which of two hundred hits are worth
//! reading, how much of the surrounding code to include, or when to stop:
//!
//! 1. the engine finds matching lines (unchanged, its own strength)
//! 2. each match expands to the declaration enclosing it, not a fixed window
//! 3. overlapping regions merge, so the same lines are never sent twice
//! 4. spans rank across the whole result set, declarations first
//! 5. results pack under a token budget and come back with anchors

use std::cell::OnceCell;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use grep_matcher::Matcher;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::sinks::UTF8;
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkMatch};
use ignore::overrides::OverrideBuilder;
use ignore::WalkBuilder;
use serde::Serialize;

use crate::spans::{declaration_name, declarations, enclosing, identifier_tokens, Declaration};
use crate::tokenizer::{count as count_tokens, count_capped};

/// A match on a declaration line is the strongest signal available: it is the
/// difference between where something is defined and one of forty places it is
/// mentioned. Declarations sort first outright; these weights order the rest.
const W_MATCHES: f64 = 1.0;
const W_TERMS: f64 = 1.5;
const W_PATH: f64 = 1.0;

/// Charged per level of nesting, and only against a declaration that a
/// shallower declaration of the same name is competing with.
///
/// Asking where `save` is defined means the `save` a module declares, not the
/// `save` method some unrelated class happens to have. Penalising depth
/// unconditionally would also demote methods that nothing competes with, and
/// since a top-level span is the larger of the two, that spends budget without
/// answering the question any better.
const W_DEPTH: f64 = 1.0;

/// Window for matches outside any declaration: imports, top-level constants,
/// configuration literals.
pub const ORPHAN_CONTEXT: u64 = 6;

/// Spans are extracted only until this multiple of the budget is on hand.
/// Ranking needs more material than it returns, but not all of it: a term
/// appearing in a thousand files should not cost a thousand file reads to
/// answer with twenty spans.
const CANDIDATE_SLACK: usize = 4;

/// Always consider at least this many files, however small the budget.
const MIN_CANDIDATE_FILES: usize = 8;

/// Candidates ordered per pass.
///
/// A common term matches tens of thousands of files and a handful of them are
/// returned, so fully ordering the rest is work thrown away. Each pass lifts
/// the best `CANDIDATE_CHUNK` out of what remains in linear time and orders
/// only those; another pass runs only if the budget is still unfilled.
const CANDIDATE_CHUNK: usize = 256;

/// Matched lines recorded per file before the searcher moves on.
///
/// A budget caps what one file can contribute, so recording thousands of match
/// positions in a single file buys nothing. A common term in a large tree
/// matches hundreds of thousands of lines, and the per-line callback is the
/// walk's hot path. With no budget the cap is lifted instead: dropping the
/// 513th match of a file is exactly what `--no-budget` promises not to do.
const MAX_MATCH_LINES_PER_FILE: usize = 512;

/// Matched lines checked for a declaration of the query before the walk gives
/// up on the hint for that file.
///
/// A file that declares the query and mentions it later declares it in one of
/// its first few matches. Scanning every match instead costs the walk a fifth
/// of its time on a term like `Result`, which appears on a declaration line in
/// almost every file and declares almost none of them.
const HINT_SCAN_LINES: usize = 16;

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
    tokens: OnceCell<usize>,
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
    /// Serial: order candidates by the signals the walk produced.
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
            literal: false,
            word: false,
            ignore_case: false,
            hidden: false,
            no_ignore: false,
            threads: 0,
        }
    }
}

/// Ranking terms recovered from a search pattern.
///
/// The pattern may be a regex, so metacharacters are dropped and what is left
/// is tokenized the way an identifier would be: a search for `validateToken`
/// should still rank a `validate_token` span.
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

/// Counts matches without requiring line numbers.
///
/// `sinks::UTF8` returns an error when line numbers are disabled, so a sink
/// that genuinely does not need them has to be written out. This phase only
/// decides which files are worth opening; exact positions are resolved later,
/// per candidate, by [`matched_lines`].
struct ScoutSink<'a> {
    matcher: &'a RegexMatcher,
    matches: usize,
    declaration_hint: bool,
}

impl Sink for ScoutSink<'_> {
    type Error = std::io::Error;

    fn matched(&mut self, _searcher: &Searcher, mat: &SinkMatch<'_>) -> Result<bool, Self::Error> {
        // Only until one is found, and only over the first
        // `HINT_SCAN_LINES` matches: the declaration scan runs on every
        // matched line in the tree.
        //
        // The name has to match the query, exactly as it does once the file is
        // read; see `declares_query` for why a match landing on a
        // declaration line is not enough on its own.
        if !self.declaration_hint && self.matches < HINT_SCAN_LINES {
            if let Ok(line) = std::str::from_utf8(mat.bytes()) {
                self.declaration_hint = declares_query(self.matcher, declaration_name(line));
            }
        }
        self.matches += 1;
        // Returning false stops the searcher on this file.
        Ok(self.matches < MAX_MATCH_LINES_PER_FILE)
    }
}

/// A matching file with its ranking signals resolved.
///
/// `path_score` is computed once here rather than inside the ordering
/// comparator: it lowercases and splits the file name into a `Vec<String>`,
/// and a comparator runs O(n log n) times.
struct Candidate {
    file: FileMatches,
    path_score: f64,
}

/// Best first. Total, because paths are unique, so the order does not depend
/// on the nondeterministic order the parallel walk produced.
fn better_candidate(a: &Candidate, b: &Candidate) -> std::cmp::Ordering {
    b.file
        .declaration_hint
        .cmp(&a.file.declaration_hint)
        .then(b.file.match_count.cmp(&a.file.match_count))
        .then(
            b.path_score
                .partial_cmp(&a.path_score)
                .unwrap_or(std::cmp::Ordering::Equal),
        )
        .then(a.file.path.cmp(&b.file.path))
}

/// What the walk learns about one matching file.
///
/// Deliberately minimal. It holds neither the file's text nor its matched
/// line numbers, because most matching files are never returned: on a large
/// tree a common term matches thousands of files to produce twenty spans, and
/// resolving line numbers for all of them costs several times the search.
struct FileMatches {
    path: String,
    absolute: PathBuf,
    match_count: usize,
    /// Any matched line that declares the query on its own.
    declaration_hint: bool,
}

/// A region of one file that will become a [`Hit`], before scoring.
struct Region {
    start: u64,
    end: u64,
    symbol: Option<String>,
    kind: Option<String>,
    matched: Vec<u64>,
    is_declaration: bool,
    depth: usize,
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
fn declares_query(matcher: &RegexMatcher, name: Option<&str>) -> bool {
    name.is_some_and(|name| matcher.is_match(name.as_bytes()).unwrap_or(false))
}

fn regions_for(
    decls: &[Declaration],
    lines: &[u64],
    total_lines: u64,
    matcher: &RegexMatcher,
) -> Vec<Region> {
    let mut sorted = lines.to_vec();
    sorted.sort_unstable();
    sorted.dedup();

    // Group match lines by the region that will contain them, so a function
    // matched eight times is one span rather than eight.
    let mut regions: Vec<Region> = Vec::new();
    for line in sorted {
        let (start, end, symbol, kind, depth) = match enclosing(decls, line) {
            Some(d) => (
                d.start_line,
                d.end_line.min(total_lines),
                Some(d.name.clone()),
                Some(d.kind.clone()),
                d.depth,
            ),
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
                let is_declaration = start == line && declares_query(matcher, symbol.as_deref());
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

fn build_matcher(pattern: &str, opts: &Options) -> Result<RegexMatcher> {
    RegexMatcherBuilder::new()
        .case_insensitive(opts.ignore_case)
        .word(opts.word)
        .fixed_strings(opts.literal)
        .build(pattern)
        .with_context(|| format!("invalid pattern: {pattern}"))
}

/// Matched line numbers for one candidate file, resolved late.
///
/// Only candidates reach this, so precise line numbers are paid for on the
/// handful of files that can actually be returned rather than on every file
/// that happened to match.
fn matched_lines(
    searcher: &mut Searcher,
    matcher: &RegexMatcher,
    content: &str,
    limit: usize,
) -> Vec<u64> {
    let mut lines = Vec::new();
    let sink = UTF8(|line_number, _| {
        lines.push(line_number);
        Ok(lines.len() < limit)
    });
    let _ = searcher.search_slice(matcher, content.as_bytes(), sink);
    lines
}

fn collect_matches(
    matcher: &RegexMatcher,
    root: &Path,
    opts: &Options,
) -> Result<Vec<FileMatches>> {
    let mut overrides = OverrideBuilder::new(root);
    for glob in &opts.globs {
        overrides
            .add(glob)
            .with_context(|| format!("invalid glob: {glob}"))?;
    }
    let overrides = overrides.build().context("could not build glob set")?;

    let mut builder = WalkBuilder::new(root);
    builder
        .overrides(overrides)
        .hidden(!opts.hidden)
        .git_ignore(!opts.no_ignore)
        .git_global(!opts.no_ignore)
        .git_exclude(!opts.no_ignore);
    if opts.threads > 0 {
        builder.threads(opts.threads);
    }

    // A channel rather than a shared Vec behind a mutex: on a large tree,
    // tens of thousands of files match and every one of them would contend
    // for the same lock, which measured as poor scaling across cores.
    let (tx, rx) = mpsc::channel::<FileMatches>();
    let root = root.to_path_buf();

    builder.build_parallel().run(|| {
        let tx = tx.clone();
        let root = root.clone();
        // A matcher per worker, never one shared between them. The regex
        // engine draws a scratch cache from a pool on every call, and that
        // pool is lock-free only for the thread that created it; every other
        // thread takes a mutex (rust-lang/regex#934). Cloning is cheap -- the
        // compiled program is behind an `Arc` -- and gives each worker its own
        // pool.
        let matcher = matcher.clone();
        // One searcher per worker thread; it is not shared across them.
        let mut searcher = SearcherBuilder::new()
            // Line numbers are the single most expensive option here -- on a
            // 92k-file tree they cost four times the rest of the search -- and
            // this phase only decides which files are worth opening. They are
            // resolved later, per candidate, by `matched_lines`.
            .line_number(false)
            // Stop at the first NUL rather than scanning a binary to the end.
            .binary_detection(BinaryDetection::quit(0))
            // Memory mapping is deliberately left off: measured against that
            // same tree it doubled search time, because per-file mmap setup
            // dominates when most files are small.
            .build();

        Box::new(move |entry| {
            let Ok(entry) = entry else {
                return ignore::WalkState::Continue;
            };
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                return ignore::WalkState::Continue;
            }
            // Search via the path so the engine can memory-map, detect binary
            // files and stop early. Reading every file up front to reuse one
            // buffer costs more than re-reading the few that match: most
            // files in a tree do not match, and paying full I/O plus UTF-8
            // validation for all of them dwarfs a second read of a handful.
            let mut sink = ScoutSink {
                matcher: &matcher,
                matches: 0,
                declaration_hint: false,
            };
            if searcher
                .search_path(&matcher, entry.path(), &mut sink)
                .is_err()
                || sink.matches == 0
            {
                return ignore::WalkState::Continue;
            }
            let (match_count, declaration_hint) = (sink.matches, sink.declaration_hint);

            let path = relative_path(&root, entry.path());
            let _ = tx.send(FileMatches {
                path,
                absolute: entry.path().to_path_buf(),
                match_count,
                declaration_hint,
            });
            ignore::WalkState::Continue
        })
    });

    // Every worker's sender must go before the receiver will finish.
    drop(tx);
    // Left in whatever order the parallel walk produced: `better_candidate`
    // breaks ties on the path, so the result is deterministic without paying
    // to sort tens of thousands of entries that will never be read.
    Ok(rx.into_iter().collect())
}

fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Every span one candidate file contributes.
///
/// Pulled out of the candidate loop so a batch of candidates can be turned
/// into spans on several threads at once: each call touches only its own file
/// and returns an owned result.
fn spans_for_file(
    candidate: &Candidate,
    matcher: &RegexMatcher,
    terms: &[String],
    max_match_lines: usize,
) -> Vec<Hit> {
    let file = &candidate.file;
    let Ok(content) = std::fs::read_to_string(&file.absolute) else {
        return Vec::new();
    };
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        return Vec::new();
    }
    let total_lines = lines.len() as u64;
    let decls = declarations(&content);
    let mut searcher = SearcherBuilder::new().line_number(true).build();
    let match_lines = matched_lines(&mut searcher, matcher, &content, max_match_lines);
    if match_lines.is_empty() {
        return Vec::new();
    }

    let mut hits = Vec::new();
    for region in regions_for(&decls, &match_lines, total_lines, matcher) {
        let from = (region.start.saturating_sub(1)) as usize;
        let to = (region.end as usize).min(lines.len());
        if from >= to {
            continue;
        }
        let text = lines[from..to].join("\n");
        let score = W_MATCHES * ((region.matched.len() + 1) as f64).ln()
            + W_TERMS * term_overlap(&text, terms)
            + W_PATH * candidate.path_score;
        hits.push(Hit {
            path: file.path.clone(),
            start_line: region.start,
            end_line: region.end,
            symbol: region.symbol,
            kind: region.kind,
            match_lines: region.matched,
            is_declaration: region.is_declaration,
            depth: region.depth,
            tokens: OnceCell::new(),
            score,
            text,
        });
    }
    hits
}

/// Turn a batch of candidates into spans, in parallel, preserving their order.
///
/// The files in a batch are independent, and one of them is routinely a
/// megabyte of bundled JavaScript while the rest are ordinary source, so the
/// batch costs what its largest member costs rather than the sum.
fn spans_for_batch(
    batch: &[Candidate],
    matcher: &RegexMatcher,
    terms: &[String],
    max_match_lines: usize,
) -> Vec<Vec<Hit>> {
    if batch.len() < 2 {
        return batch
            .iter()
            .map(|c| spans_for_file(c, matcher, terms, max_match_lines))
            .collect();
    }
    // A batch is at most `MIN_CANDIDATE_FILES`, so one thread per file needs
    // no pool and no work queue.
    std::thread::scope(|scope| {
        let workers: Vec<_> = batch
            .iter()
            .map(|candidate| {
                // A matcher per worker: see the note in `collect_matches`.
                let matcher = matcher.clone();
                scope.spawn(move || spans_for_file(candidate, &matcher, terms, max_match_lines))
            })
            .collect();
        workers
            .into_iter()
            .map(|w| w.join().unwrap_or_default())
            .collect()
    })
}

/// Ranked spans matching `pattern`, packed under `opts.max_tokens`, and a
/// breakdown of where the time went.
pub fn search(pattern: &str, root: &Path, opts: &Options) -> Result<(Vec<Hit>, Timings)> {
    let mut timings = Timings::default();
    let matcher = build_matcher(pattern, opts)?;

    let started = Instant::now();
    let mut files = collect_matches(&matcher, root, opts)?;
    timings.walk = started.elapsed();
    timings.matching_files = files.len();

    let started = Instant::now();
    let terms = query_terms(pattern);

    // Rank files before reading any of them, so span extraction is paid for
    // only on files that could plausibly be returned. The signals here are
    // whatever the walk already produced: whether a matched line looked like
    // a declaration, how many matches there were, and the file's own name.
    let mut ranked: Vec<Candidate> = files
        .drain(..)
        .map(|file| Candidate {
            path_score: path_score(&file.path, &terms),
            file,
        })
        .collect();
    timings.rank = started.elapsed();

    let started = Instant::now();
    // Without a budget every matching file is a candidate: there is no size
    // for extraction to stop at, and stopping early would silently drop
    // matches that were asked for.
    let budget = opts
        .max_tokens
        .map(|max| max.saturating_mul(CANDIDATE_SLACK));
    let exhausted = |examined: usize, gathered: usize| {
        examined >= MIN_CANDIDATE_FILES && budget.is_some_and(|b| gathered >= b)
    };
    let max_match_lines = match opts.max_tokens {
        Some(_) => MAX_MATCH_LINES_PER_FILE,
        None => usize::MAX,
    };
    let mut gathered = 0usize;
    let mut hits: Vec<Hit> = Vec::new();

    let mut examined = 0usize;
    let mut start = 0usize;
    'passes: while start < ranked.len() {
        if exhausted(examined, gathered) {
            break;
        }
        let end = (start + CANDIDATE_CHUNK).min(ranked.len());
        if end < ranked.len() {
            ranked[start..].select_nth_unstable_by(CANDIDATE_CHUNK - 1, better_candidate);
        }
        ranked[start..end].sort_by(better_candidate);

        let mut at = start;
        while at < end {
            // The first batch is the run the budget check cannot cut short --
            // `MIN_CANDIDATE_FILES` are always examined -- so extracting them
            // together returns exactly what examining them one at a time
            // would. After that the budget is consulted per file, as before.
            let batch_len = if examined == 0 {
                MIN_CANDIDATE_FILES.min(end - at)
            } else {
                1
            };
            let batch = &ranked[at..at + batch_len];
            for file_hits in spans_for_batch(batch, &matcher, &terms, max_match_lines) {
                if !file_hits.is_empty() {
                    timings.read_files += 1;
                }
                for hit in file_hits {
                    // The gate below is `gathered >= budget` and nothing else
                    // reads the total, so a span is counted only up to what is
                    // still missing: the sum crosses the budget on exactly the
                    // span it would have crossed on. A count that came in
                    // under its cap is the real one, so the packer inherits
                    // it; the rest go uncounted until it asks.
                    let missing = budget.map_or(0, |b| b.saturating_sub(gathered));
                    if missing > 0 {
                        let counted = count_capped(&hit.text, missing);
                        gathered += counted;
                        if counted < missing {
                            let _ = hit.tokens.set(counted);
                        }
                    }
                    hits.push(hit);
                }
            }
            examined += batch_len;
            at += batch_len;
            if exhausted(examined, gathered) {
                break 'passes;
            }
        }
        start = end;
    }

    penalize_shadowed_declarations(&mut hits);

    // Declarations first, unconditionally. A span whose match lands on the
    // declaration line is what "where is X" asks for; letting a file that
    // mentions X twenty times outrank it by match count is how a ranker loses
    // to a one-line grep.
    hits.sort_by(|a, b| {
        b.is_declaration
            .cmp(&a.is_declaration)
            .then(
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
            .then(a.path.cmp(&b.path))
            .then(a.start_line.cmp(&b.start_line))
    });
    timings.extract = started.elapsed();

    Ok((pack(hits, opts), timings))
}

/// Demote a declaration that a shallower declaration of the same name shadows.
///
/// Two files declaring `save` -- one at module level, one as a method of an
/// unrelated class -- are both declarations of the name, and nesting is the
/// only thing that separates the one being asked about from the one that
/// merely shares its spelling.
fn penalize_shadowed_declarations(hits: &mut [Hit]) {
    let mut shallowest: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for hit in hits.iter() {
        if !hit.is_declaration {
            continue;
        }
        if let Some(symbol) = &hit.symbol {
            let entry = shallowest.entry(symbol).or_insert(hit.depth);
            *entry = (*entry).min(hit.depth);
        }
    }
    let shallowest: std::collections::HashMap<String, usize> = shallowest
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();

    for hit in hits.iter_mut() {
        if !hit.is_declaration {
            continue;
        }
        let Some(symbol) = &hit.symbol else { continue };
        let Some(&best) = shallowest.get(symbol) else {
            continue;
        };
        hit.score -= W_DEPTH * hit.depth.saturating_sub(best) as f64;
    }
}

/// Fill the token budget from the ranked list, capping any one file's share.
fn pack(hits: Vec<Hit>, opts: &Options) -> Vec<Hit> {
    let mut used = 0usize;
    let mut per_file: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
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

/// Search roots that do not exist are the caller's error, not a walk failure.
pub fn validate_root(root: &Path) -> Result<PathBuf> {
    root.canonicalize()
        .with_context(|| format!("no such path: {}", root.display()))
}
