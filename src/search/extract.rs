//! Reading candidates and turning them into spans.
//!
//! This is the serial half of a query, so it is bounded twice over: ordering
//! and reading interleave in passes, and extraction stops once the budget has
//! more material than it can return. Within a pass the files are independent,
//! so a batch is spread across one worker per core.

use std::borrow::Cow;
use std::cell::OnceCell;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use grep_matcher::Matcher;
use grep_regex::RegexMatcher;
use grep_searcher::sinks::UTF8;
use grep_searcher::{Searcher, SearcherBuilder};

use crate::spans::{comment_source, declarations};
use crate::tokenizer::count_capped;

use super::query::{Queries, Query};
use super::rank::{better_candidate, span_score, Candidate};
use super::region::regions_for;
use super::walk::MAX_MATCH_LINES_PER_FILE;
use super::{Hit, Options, Timings};

/// Roughly what one line of source costs, which is what turns a token budget
/// into the line budget a declaration is clamped against.
///
/// Deliberately generous: over-estimating clamps a declaration a little early,
/// while under-estimating hands the packer spans it can only drop.
const TOKENS_PER_LINE: usize = 12;

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

/// What a pattern is matched against: the file itself, or only its comments.
///
/// [`comment_source`] preserves byte offsets and newlines, so a line number
/// resolved against it is the line number in the original file.
fn haystack(content: &str, comments_only: bool) -> Cow<'_, str> {
    match comments_only {
        true => Cow::Owned(comment_source(content)),
        false => Cow::Borrowed(content),
    }
}

/// Matched line numbers for one candidate file, resolved late.
///
/// Only candidates reach this, so precise line numbers are paid for on the
/// handful of files that can actually be returned rather than on every file
/// that happened to match.
pub(super) fn matched_lines(
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

/// The 1-based byte column of the first match on each of `lines`.
///
/// Every pattern is tried, not only the span's owner: a span belongs to one
/// pattern but its lines can have been matched by any of them, and a column
/// that points at the start of the line is not a position to jump to.
///
/// Only `--vimgrep` needs these, and they cost a matcher call per matched
/// line, so nothing else pays for them. Resolved against what was searched, so
/// a `--comments` match reports the column it has in the file rather than in
/// the code the mask removed.
fn match_columns(queries: &[Query], searched: &[&str], lines: &[u64]) -> Vec<u64> {
    lines
        .iter()
        .map(|line| {
            let Some(text) = searched.get((*line as usize).saturating_sub(1)) else {
                return 1;
            };
            queries
                .iter()
                .filter_map(|query| query.matcher.find_at(text.as_bytes(), 0).ok().flatten())
                .map(|found| found.start() as u64 + 1)
                .min()
                .unwrap_or(1)
        })
        .collect()
}

/// What turning a candidate into spans needs, beyond the candidate itself.
///
/// Fixed for the whole run and shared by every worker. The matchers travel
/// separately because they are the one thing each worker needs its own copy
/// of.
struct Extraction {
    /// Whether the patterns are matched against comments alone.
    comments_only: bool,
    /// Whether to resolve the column of every match.
    columns: bool,
    /// Matched lines recorded per file; see [`MAX_MATCH_LINES_PER_FILE`].
    max_match_lines: usize,
    /// Lines a declaration may span before a window into it is returned
    /// instead; see [`TOKENS_PER_LINE`].
    max_declaration_lines: u64,
    /// Threads a batch is spread across.
    workers: usize,
}

impl Extraction {
    /// The settings one run's extraction works under.
    fn for_run(opts: &Options) -> Self {
        Self {
            comments_only: opts.comments_only,
            columns: opts.columns,
            // A budget caps what one file can contribute, so recording
            // thousands of match positions in it buys nothing. With no budget
            // the cap lifts: dropping the 513th match of a file is exactly
            // what `--no-budget` promises not to do.
            max_match_lines: match opts.max_tokens {
                Some(_) => MAX_MATCH_LINES_PER_FILE,
                None => usize::MAX,
            },
            // A declaration past this many lines cannot fit the budget, so
            // what the query gets is a window into it. Without a budget
            // nothing is too large.
            max_declaration_lines: opts
                .max_tokens
                .map_or(u64::MAX, |max| (max / TOKENS_PER_LINE).max(1) as u64),
            workers: match opts.threads {
                0 => std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get),
                threads => threads,
            },
        }
    }
}

/// Every span one candidate file contributes.
///
/// Pulled out of the candidate loop so a batch of candidates can be turned
/// into spans on several threads at once: each call touches only its own file
/// and returns an owned result.
fn spans_for_file(candidate: &Candidate, queries: &[Query], ex: &Extraction) -> Vec<Hit> {
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
    let searched = haystack(&content, ex.comments_only);

    // The file is read and parsed once; each pattern only re-runs the matcher
    // over text already in memory.
    let mut matched: Vec<(u64, usize)> = Vec::new();
    for (index, query) in queries.iter().enumerate() {
        let found = matched_lines(&mut searcher, &query.matcher, &searched, ex.max_match_lines);
        matched.extend(found.into_iter().map(|line| (line, index)));
    }
    if matched.is_empty() {
        return Vec::new();
    }

    let searched_lines: Vec<&str> = match ex.columns {
        true => searched.lines().collect(),
        false => Vec::new(),
    };

    let mut hits = Vec::new();
    for region in regions_for(
        &decls,
        &matched,
        total_lines,
        queries,
        ex.max_declaration_lines,
    ) {
        let from = (region.start.saturating_sub(1)) as usize;
        let to = (region.end as usize).min(lines.len());
        if from >= to {
            continue;
        }
        let owner = region.owner();
        let answered = region.answered();
        let text = lines[from..to].join("\n");
        let score = span_score(
            &text,
            &queries[owner].terms,
            region.matched.len(),
            candidate.path_scores[owner],
        );
        let columns = match ex.columns {
            true => match_columns(queries, &searched_lines, &region.matched),
            false => Vec::new(),
        };
        let is_declaration = region.is_declaration();
        hits.push(Hit {
            path: file.path.clone(),
            start_line: region.start,
            end_line: region.end,
            symbol: region.symbol,
            kind: region.kind,
            is_declaration,
            match_lines: region.matched,
            match_columns: columns,
            depth: region.depth,
            answered,
            query: queries[owner].pattern.clone(),
            query_index: owner,
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
/// batch costs what its slowest worker costs rather than the sum.
///
/// One worker per core, pulling the next candidate from a shared cursor —
/// never one thread per file. A no-budget query extracts every matching file
/// in the tree, and spawning a thread for each of several thousand small ones
/// costs more than reading them does.
fn spans_for_batch(batch: &[Candidate], queries: &[Query], ex: &Extraction) -> Vec<Vec<Hit>> {
    let mut out: Vec<Vec<Hit>> = (0..batch.len()).map(|_| Vec::new()).collect();
    let workers = ex.workers.min(batch.len());
    if workers < 2 {
        for (slot, candidate) in out.iter_mut().zip(batch) {
            *slot = spans_for_file(candidate, queries, ex);
        }
        return out;
    }

    let next = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                // A matcher per worker: see the note in `collect_matches`.
                let mine = queries.to_vec();
                let next = &next;
                scope.spawn(move || extract_share(batch, next, &mine, ex))
            })
            .collect();
        // Written back by index, so the result does not depend on which worker
        // finished first.
        for handle in handles {
            for (at, hits) in handle.join().unwrap_or_default() {
                out[at] = hits;
            }
        }
    });
    out
}

/// One worker's share of a batch: whatever the cursor hands it, each result
/// carrying the index it belongs at.
fn extract_share(
    batch: &[Candidate],
    next: &AtomicUsize,
    queries: &[Query],
    ex: &Extraction,
) -> Vec<(usize, Vec<Hit>)> {
    let mut mine: Vec<(usize, Vec<Hit>)> = Vec::new();
    loop {
        let at = next.fetch_add(1, Ordering::Relaxed);
        let Some(candidate) = batch.get(at) else {
            return mine;
        };
        mine.push((at, spans_for_file(candidate, queries, ex)));
    }
}

/// Spans gathered so far, and what they have cost against the budget.
struct Gathered {
    hits: Vec<Hit>,
    /// Charged per pattern, so one pattern cannot satisfy the gate on behalf
    /// of another that has found nothing yet.
    tokens: Vec<usize>,
    read_files: usize,
}

impl Gathered {
    fn new(queries: usize) -> Self {
        Self {
            hits: Vec::new(),
            tokens: vec![0; queries],
            read_files: 0,
        }
    }

    /// Whether every pattern has `share` tokens of material to rank.
    fn satisfied(&self, share: usize) -> bool {
        self.tokens.iter().all(|charged| *charged >= share)
    }

    /// Take one batch's spans, charging each to its own pattern's `share`.
    ///
    /// The gate that stops extraction reads `tokens` and nothing else does,
    /// so a span is counted only up to what is still missing: the sum crosses
    /// the share on exactly the span it would have crossed on. A count that
    /// came in under its cap is the real one, so the packer inherits it; the
    /// rest go uncounted until it asks.
    fn absorb(&mut self, batch: Vec<Vec<Hit>>, share: Option<usize>) {
        for file_hits in batch {
            if !file_hits.is_empty() {
                self.read_files += 1;
            }
            for hit in file_hits {
                let charged = &mut self.tokens[hit.query_index];
                let missing = share.map_or(0, |s| s.saturating_sub(*charged));
                if missing > 0 {
                    let counted = count_capped(&hit.text, missing);
                    *charged += counted;
                    if counted < missing {
                        let _ = hit.tokens.set(counted);
                    }
                }
                self.hits.push(hit);
            }
        }
    }
}

/// Read candidates best-first until the budget has enough material to rank.
///
/// Ordering and reading interleave: a common term matches tens of thousands of
/// files to return a handful, so candidates are lifted out in passes of
/// `CANDIDATE_CHUNK` and another pass runs only if the budget is still
/// unfilled. Ordering is a ranking cost rather than an extraction one, so it
/// is timed on its own and taken back off the phase total.
pub(super) fn gather_spans(
    ranked: &mut [Candidate],
    queries: &Queries,
    opts: &Options,
    timings: &mut Timings,
) -> Vec<Hit> {
    let started = Instant::now();
    let mut ordering_total = Duration::ZERO;
    let ex = Extraction::for_run(opts);

    // Without a budget every matching file is a candidate: there is no size
    // for extraction to stop at, and stopping early would silently drop
    // matches that were asked for.
    //
    // With several patterns the slack is divided between them, so a common
    // pattern reaching its share does not stop a rare one being looked for.
    let share = opts
        .max_tokens
        .map(|max| (max.saturating_mul(CANDIDATE_SLACK) / queries.len()).max(1));
    let exhausted = |examined: usize, found: &Gathered| {
        examined >= MIN_CANDIDATE_FILES && share.is_some_and(|s| found.satisfied(s))
    };
    // With a budget, a batch is the smallest run the budget check cannot cut
    // short. Without one there is nothing to stop for, so a whole pass goes at
    // once and the workers are spawned for it once rather than once per eight
    // files.
    let batch_width = if share.is_some() {
        MIN_CANDIDATE_FILES
    } else {
        CANDIDATE_CHUNK
    };

    let mut found = Gathered::new(queries.len());
    let mut examined = 0usize;
    let mut start = 0usize;
    'passes: while start < ranked.len() {
        if exhausted(examined, &found) {
            break;
        }
        let ordering = Instant::now();
        let end = (start + CANDIDATE_CHUNK).min(ranked.len());
        if end < ranked.len() {
            ranked[start..].select_nth_unstable_by(CANDIDATE_CHUNK - 1, better_candidate);
        }
        ranked[start..end].sort_by(better_candidate);
        ordering_total += ordering.elapsed();

        let mut at = start;
        while at < end {
            // The budget is consulted per batch rather than per file, so a
            // query reads at most `MIN_CANDIDATE_FILES - 1` files past the
            // point one at a time would have stopped. `CANDIDATE_SLACK`
            // already gathers four times the budget before stopping, so those
            // files widen the field ranking chooses from and cost a thread
            // each rather than a pass each.
            let batch_len = batch_width.min(end - at);
            found.absorb(
                spans_for_batch(&ranked[at..at + batch_len], &queries.list, &ex),
                share,
            );
            examined += batch_len;
            at += batch_len;
            if exhausted(examined, &found) {
                break 'passes;
            }
        }
        start = end;
    }

    timings.read_files = found.read_files;
    timings.rank += ordering_total;
    timings.extract = started.elapsed().saturating_sub(ordering_total);
    found.hits
}
