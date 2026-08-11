//! The parallel phase: run the engine over the tree and keep the files that
//! matched, with just enough about each one to rank it.
//!
//! Nothing here reads a file twice or resolves a line number. Most matching
//! files are never returned, so everything that costs per file is deferred to
//! the candidates that survive ranking.

use std::path::{Path, PathBuf};
use std::sync::mpsc;

use anyhow::{Context, Result};
use grep_regex::RegexMatcher;
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkMatch};
use ignore::overrides::OverrideBuilder;
use ignore::WalkBuilder;

use crate::spans::{comment_source, declaration_name};

use super::region::declares_query;
use super::Options;

/// Matched lines recorded per file before the searcher moves on.
///
/// A budget caps what one file can contribute, so recording thousands of match
/// positions in a single file buys nothing. A common term in a large tree
/// matches hundreds of thousands of lines, and the per-line callback is the
/// walk's hot path. With no budget the cap is lifted instead: dropping the
/// 513th match of a file is exactly what `--no-budget` promises not to do.
pub(super) const MAX_MATCH_LINES_PER_FILE: usize = 512;

/// Matched lines checked for a declaration of the query before the walk gives
/// up on the hint for that file.
///
/// A file that declares the query and mentions it later declares it in one of
/// its first few matches. Scanning every match instead costs the walk a fifth
/// of its time on a term like `Result`, which appears on a declaration line in
/// almost every file and declares almost none of them.
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

/// What the walk learns about one matching file.
///
/// Deliberately minimal. It holds neither the file's text nor its matched
/// line numbers, because most matching files are never returned: on a large
/// tree a common term matches thousands of files to produce twenty spans, and
/// resolving line numbers for all of them costs several times the search.
pub(super) struct FileMatches {
    pub(super) path: String,
    pub(super) absolute: PathBuf,
    pub(super) match_count: usize,
    /// Any matched line that declares the query on its own.
    pub(super) declaration_hint: bool,
}

/// The walker one run needs, with the ignore rules the flags asked for.
///
/// An explicit path list is the caller's own choice of files, so ignore rules
/// do not get to remove any of them. Globs still apply: those are part of the
/// same request.
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
        // Asked for nothing, so nothing is searched. Distinct from no list at
        // all, which walks the root.
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
        // Line numbers are the single most expensive option here -- on a
        // 92k-file tree they cost four times the rest of the search -- and
        // this phase only decides which files are worth opening. They are
        // resolved later, per candidate, by `matched_lines`.
        .line_number(false)
        // Stop at the first NUL rather than scanning a binary to the end.
        .binary_detection(BinaryDetection::quit(0))
        // Memory mapping is deliberately left off: measured against that same
        // tree it doubled search time, because per-file mmap setup dominates
        // when most files are small.
        .build()
}

/// Match count and declaration hint for one file, or `None` if it is not a
/// file, could not be read, or did not match.
///
/// Search via the path so the engine can memory-map, detect binary files and
/// stop early. Reading every file up front to reuse one buffer costs more than
/// re-reading the few that match: most files in a tree do not match, and
/// paying full I/O plus UTF-8 validation for all of them dwarfs a second read
/// of a handful.
///
/// Comment scoping is the exception: the comments have to be cut out of the
/// file before the pattern sees it, so the file is read here and the mask is
/// searched in its place. The counts this phase produces then rank files by
/// their comment matches rather than by matches the query will never be shown.
fn scout_file(
    searcher: &mut Searcher,
    matcher: &RegexMatcher,
    path: &Path,
    comments_only: bool,
) -> Option<(usize, bool)> {
    let mut sink = ScoutSink {
        matcher,
        matches: 0,
        declaration_hint: false,
    };
    let searched = if comments_only {
        match std::fs::read_to_string(path) {
            Ok(content) => {
                searcher.search_slice(matcher, comment_source(&content).as_bytes(), &mut sink)
            }
            Err(err) => Err(err),
        }
    } else {
        searcher.search_path(matcher, path, &mut sink)
    };
    match searched.is_err() || sink.matches == 0 {
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

    // A channel rather than a shared Vec behind a mutex: on a large tree,
    // tens of thousands of files match and every one of them would contend
    // for the same lock, which measured as poor scaling across cores.
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
