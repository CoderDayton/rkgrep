//! The parallel phase: run the engine over the tree and keep the files that
//! matched, with just enough about each one to rank it.
//!
//! Nothing here reads a file twice or resolves a line number. Most matching
//! files are never returned, so everything that costs per file is deferred to
//! the candidates that survive ranking.

use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use anyhow::{Context, Result};
use grep_matcher::Matcher;
use grep_regex::RegexMatcher;
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkMatch};
use ignore::overrides::OverrideBuilder;
use ignore::WalkBuilder;

use crate::spans::{declaration_name, open_source, Masker, Mode};

use super::region::declares_query;
use super::Options;

/// Matched lines recorded per file before the searcher moves on.
pub(super) const MAX_MATCH_LINES_PER_FILE: usize = 512;

/// Matched lines checked for a declaration of the query before the walk gives
/// up on the hint for that file.
const HINT_SCAN_LINES: usize = 16;

/// Counts matches without requiring line numbers.
///
/// `sinks::UTF8` returns an error when line numbers are disabled, so a sink
/// that genuinely does not need them has to be written out. This phase only
/// decides which files are worth opening; exact positions are resolved later,
/// per candidate, by [`super::extract::matched_lines`].
struct ScoutSink<'a> {
    matcher: &'a RegexMatcher,
    matches: usize,
    declaration_hint: bool,
}

impl Sink for ScoutSink<'_> {
    type Error = std::io::Error;

    fn matched(&mut self, _searcher: &Searcher, mat: &SinkMatch<'_>) -> Result<bool, Self::Error> {
        if !self.declaration_hint && self.matches < HINT_SCAN_LINES {
            if let Ok(line) = std::str::from_utf8(mat.bytes()) {
                self.declaration_hint = declares_query(self.matcher, declaration_name(line));
            }
        }
        self.matches += 1;
        Ok(self.matches < MAX_MATCH_LINES_PER_FILE)
    }
}

/// What the walk learns about one matching file.
pub(super) struct FileMatches {
    pub(super) path: String,
    pub(super) absolute: PathBuf,
    pub(super) match_count: usize,
    /// Any matched line that declares the query on its own.
    pub(super) declaration_hint: bool,
}

/// The walker one run needs, with the ignore rules the flags asked for.
fn walker(root: &Path, opts: &Options) -> Result<Option<WalkBuilder>> {
    let mut overrides = OverrideBuilder::new(root);
    for glob in &opts.globs {
        overrides
            .add(glob)
            .with_context(|| format!("invalid glob: {glob}"))?;
    }
    let overrides = overrides.build().context("could not build glob set")?;

    let explicit = opts.paths.is_some();
    let mut builder = match opts.paths.as_deref() {
        Some([]) => return Ok(None),
        Some([first, rest @ ..]) => {
            let mut builder = WalkBuilder::new(first);
            for path in rest {
                builder.add(path);
            }
            builder
        }
        None => WalkBuilder::new(root),
    };
    builder
        .overrides(overrides)
        .hidden(!(opts.hidden || explicit))
        .git_ignore(!(opts.no_ignore || explicit))
        .git_global(!(opts.no_ignore || explicit))
        .git_exclude(!(opts.no_ignore || explicit));
    if opts.threads > 0 {
        builder.threads(opts.threads);
    }
    Ok(Some(builder))
}

/// One searcher per worker thread; it is never shared across them.
fn scout_searcher() -> Searcher {
    SearcherBuilder::new()
        // Line numbers are the single most expensive option here, and this
        // phase only decides which files are worth opening. They are resolved
        // later, per candidate, by `matched_lines`.
        .line_number(false)
        // Stop at the first NUL rather than scanning a binary to the end.
        .binary_detection(BinaryDetection::quit(0))
        // Memory mapping is deliberately left off: measured, it doubled search
        // time, because per-file mmap setup dominates when most files are
        // small.
        .build()
}

/// Match count and declaration hint from the comments of one file.
///
/// Comments have to be masked before the pattern sees them, which the engine
/// cannot do for us. Masking carries from one line to the next, so the file
/// still streams: this reads a line at a time and holds none of them.
fn scout_comments(matcher: &RegexMatcher, path: &Path) -> Option<(usize, bool)> {
    let reader = open_source(path)?;
    let mut masker = Masker::default();
    let mut comments = String::new();
    let mut matches = 0usize;
    let mut declaration_hint = false;

    for line in reader.lines() {
        let line = line.ok()?;
        masker.scan_line(&line);
        masker.render(&line, Mode::Comments, &mut comments);
        if !matcher.is_match(comments.as_bytes()).unwrap_or(false) {
            continue;
        }
        if !declaration_hint && matches < HINT_SCAN_LINES {
            declaration_hint = declares_query(matcher, declaration_name(&comments));
        }
        matches += 1;
        if matches >= MAX_MATCH_LINES_PER_FILE {
            break;
        }
    }
    (matches > 0).then_some((matches, declaration_hint))
}

/// Match count and declaration hint for one file, or `None` if it is not a
/// file, could not be read, or did not match.
fn scout_file(
    searcher: &mut Searcher,
    matcher: &RegexMatcher,
    path: &Path,
    comments_only: bool,
) -> Option<(usize, bool)> {
    if comments_only {
        return scout_comments(matcher, path);
    }
    let mut sink = ScoutSink {
        matcher,
        matches: 0,
        declaration_hint: false,
    };
    match searcher.search_path(matcher, path, &mut sink).is_err() || sink.matches == 0 {
        true => None,
        false => Some((sink.matches, sink.declaration_hint)),
    }
}

pub(super) fn collect_matches(
    matcher: &RegexMatcher,
    root: &Path,
    opts: &Options,
) -> Result<Vec<FileMatches>> {
    let Some(builder) = walker(root, opts)? else {
        return Ok(Vec::new());
    };

    let (tx, rx) = mpsc::channel::<FileMatches>();
    let root = root.to_path_buf();
    let comments_only = opts.comments_only;

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
        let mut searcher = scout_searcher();

        Box::new(move |entry| {
            let Ok(entry) = entry else {
                return ignore::WalkState::Continue;
            };
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                return ignore::WalkState::Continue;
            }
            let Some((match_count, declaration_hint)) =
                scout_file(&mut searcher, &matcher, entry.path(), comments_only)
            else {
                return ignore::WalkState::Continue;
            };
            let _ = tx.send(FileMatches {
                path: relative_path(&root, entry.path()),
                absolute: entry.path().to_path_buf(),
                match_count,
                declaration_hint,
            });
            ignore::WalkState::Continue
        })
    });

    // Every worker's sender must go before the receiver will finish.
    drop(tx);
    Ok(rx.into_iter().collect())
}

fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}
