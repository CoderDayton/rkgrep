//! rkgrep — ranked, span-scoped, budget-packed code search.

mod fetch;
mod pathset;
mod render;
mod search;
mod spans;
mod tokenizer;

use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use clap::{Parser, ValueEnum};

use crate::render::{render_json, render_text, render_vimgrep, Style};
use crate::search::{search, Hit, Options, Select};

#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum ColorChoice {
    Auto,
    Always,
    Never,
}

/// Spans from any one file, when a token budget is in force.
const DEFAULT_MAX_PER_FILE: usize = 3;

const AFTER_HELP: &str = "\
EXAMPLES:
  rkgrep validate_token              rank every hit, pack 2000 tokens
  rkgrep -w handle_request src/      whole words only, under src/
  rkgrep -e Claims -e refresh        two symbols, one budget, answers alternating
  rkgrep -d validate_token           only where it is declared
  rkgrep -r validate_token           only where it is used
  rkgrep --since main -C TODO        comments in what this branch changed
  rkgrep -l TODO | rkgrep --fetch -  survey cheaply, then read what matters
  rkgrep --vimgrep parse_url         one jumpable line per match
  rkgrep -A validate_token           every ranked span, no budget

The pattern is ripgrep's, unchanged, so ripgrep regex syntax applies. What
rkgrep adds is ranking the hits, expanding each to the declaration around it,
and fitting the result into a token budget.";

#[derive(Parser)]
#[command(
    name = "rkgrep",
    version,
    about = "Ranked, span-scoped, budget-packed code search for a context window",
    after_help = AFTER_HELP
)]
struct Cli {
    /// Pattern to search for (ripgrep regex syntax)
    pattern: Option<String>,

    /// Directory or file to search
    path: Option<PathBuf>,

    /// Pattern to search for; repeat to ask about several symbols at once
    #[arg(short = 'e', long = "regexp", value_name = "PATTERN")]
    regexp: Vec<String>,

    /// Treat the pattern as a literal string
    #[arg(short = 'F', long, help_heading = "Search")]
    fixed_strings: bool,

    /// Match whole words only
    #[arg(short = 'w', long, help_heading = "Search")]
    word_regexp: bool,

    /// Case-insensitive matching
    #[arg(short = 'i', long, help_heading = "Search")]
    ignore_case: bool,

    /// Match only inside comments, in any language
    #[arg(short = 'C', long, help_heading = "Search")]
    comments: bool,

    /// Restrict to matching files (repeatable, e.g. -g '*.rs')
    #[arg(
        short = 'g',
        long = "glob",
        value_name = "GLOB",
        help_heading = "Scoping"
    )]
    globs: Vec<String>,

    /// Search only the files listed in FILE, or on stdin for -
    #[arg(long, value_name = "FILE", help_heading = "Scoping")]
    files_from: Option<String>,

    /// Separate --files-from entries with NUL instead of newline
    #[arg(short = '0', long, requires = "files_from", help_heading = "Scoping")]
    null: bool,

    /// Search only what changed since REF, uncommitted work included
    #[arg(
        long,
        value_name = "REF",
        conflicts_with = "files_from",
        help_heading = "Scoping"
    )]
    since: Option<String>,

    /// Search hidden files and directories
    #[arg(long, help_heading = "Scoping")]
    hidden: bool,

    /// Do not respect .gitignore and friends
    #[arg(long, help_heading = "Scoping")]
    no_ignore: bool,

    /// Only spans that declare the symbol: where is this defined
    #[arg(short = 'd', long, help_heading = "Selection")]
    declarations: bool,

    /// Only spans that do not declare it: who uses this
    #[arg(
        short = 'r',
        long,
        conflicts_with = "declarations",
        help_heading = "Selection"
    )]
    references: bool,

    /// Keep budget for at least N spans that are not declarations
    #[arg(
        long,
        value_name = "N",
        default_value_t = 0,
        conflicts_with_all = ["declarations", "references"],
        help_heading = "Selection"
    )]
    min_references: usize,

    /// Only these declaring kinds, e.g. --kind fn,class (implies -d)
    ///
    /// The kind is the declaring keyword, not a type system: a function
    /// recognized from its signature alone reports `function` whatever the
    /// language calls it.
    #[arg(
        long,
        value_name = "KIND",
        value_delimiter = ',',
        conflicts_with = "references",
        help_heading = "Selection"
    )]
    kind: Vec<String>,

    /// Token budget for the whole result set
    #[arg(
        short = 't',
        long,
        default_value_t = 2000,
        value_name = "N",
        help_heading = "Budget"
    )]
    max_tokens: usize,

    /// Return every ranked span: no token budget, and no per-file cap unless
    /// --max-per-file says otherwise
    #[arg(
        short = 'A',
        long,
        conflicts_with = "max_tokens",
        help_heading = "Budget"
    )]
    no_budget: bool,

    /// Cap spans returned from any one file, 0 for no cap [default: 3]
    #[arg(long, value_name = "N", help_heading = "Budget")]
    max_per_file: Option<usize>,

    /// Print anchors without source text
    #[arg(short = 'l', long, help_heading = "Output")]
    anchors_only: bool,

    /// Number every line, marking the ones that matched
    #[arg(short = 'n', long, help_heading = "Output")]
    line_numbers: bool,

    /// One line per match as path:line:col:text, for editors
    #[arg(long, conflicts_with = "json", help_heading = "Output")]
    vimgrep: bool,

    /// Emit JSON
    #[arg(long, help_heading = "Output")]
    json: bool,

    /// Colorize headers and matched lines
    #[arg(long, value_enum, default_value = "auto", help_heading = "Output")]
    color: ColorChoice,

    /// Return the lines named by a path:start-end anchor, or by - for stdin
    /// (repeatable)
    #[arg(long, value_name = "ANCHOR", help_heading = "Output")]
    fetch: Vec<String>,

    /// Print timing and budget use to stderr
    #[arg(long, help_heading = "Performance")]
    stats: bool,

    /// Worker threads (0 chooses automatically)
    #[arg(
        long,
        default_value_t = 0,
        value_name = "N",
        help_heading = "Performance"
    )]
    threads: usize,
}

impl Cli {
    /// The patterns to search for, and the path to search.
    ///
    /// With `-e` there is no positional pattern, so the first positional is
    /// the path. Resolving it here keeps `<PATTERN> [PATH]` in the help text
    /// while letting `rkgrep -e Claims src/` mean what it looks like.
    fn patterns_and_path(&self) -> Result<(Vec<String>, PathBuf), String> {
        if self.regexp.is_empty() {
            let Some(pattern) = self.pattern.clone() else {
                return Err("no pattern given; try --help".to_string());
            };
            let path = self.path.clone().unwrap_or_else(|| PathBuf::from("."));
            return Ok((vec![pattern], path));
        }
        if self.path.is_some() {
            return Err("with -e, only a path may follow the flags".to_string());
        }
        let path = self
            .pattern
            .clone()
            .map_or_else(|| PathBuf::from("."), PathBuf::from);
        Ok((self.regexp.clone(), path))
    }

    /// The root `--fetch` resolves anchors against.
    ///
    /// Anchors are relative to the root they were surveyed under, so the same
    /// path follows them back: `rkgrep -l TODO src | rkgrep --fetch - src`.
    /// With no pattern to occupy it, the first positional is that path.
    fn fetch_root(&self) -> PathBuf {
        self.path
            .clone()
            .or_else(|| self.pattern.clone().map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("."))
    }

    fn select(&self) -> Select {
        match (self.declarations || !self.kind.is_empty(), self.references) {
            (true, _) => Select::Declarations,
            (_, true) => Select::References,
            _ => Select::All,
        }
    }
}

/// The files a run is restricted to, or `None` to walk the whole root.
fn path_set(cli: &Cli, root: &std::path::Path) -> anyhow::Result<Option<Vec<PathBuf>>> {
    if let Some(source) = &cli.files_from {
        return pathset::from_list(source, cli.null, root).map(Some);
    }
    if let Some(reference) = &cli.since {
        return pathset::from_git(reference, root).map(Some);
    }
    Ok(None)
}

fn write_out(rendered: &str) {
    let out = io::stdout();
    let mut out = io::BufWriter::new(out.lock());
    if !rendered.is_empty() {
        let _ = writeln!(out, "{rendered}");
    }
    let _ = out.flush();
}

fn exit_code(hits: &[Hit]) -> ExitCode {
    // 0 when something matched, 1 when nothing did, as grep does.
    match hits.is_empty() {
        true => ExitCode::from(1),
        false => ExitCode::SUCCESS,
    }
}

/// `--fetch`: no pattern, no walk, just the lines the anchors name.
fn run_fetch(cli: &Cli, style: Style, budget: Option<usize>) -> ExitCode {
    let root = match search::validate_root(&cli.fetch_root()) {
        Ok(root) => root,
        Err(err) => {
            eprintln!("rkgrep: {err}");
            return ExitCode::from(2);
        }
    };
    match fetch::fetch(&cli.fetch, &root, budget) {
        Ok(hits) => {
            write_out(&match cli.json {
                true => render_json(&hits),
                false => render_text(&hits, style),
            });
            exit_code(&hits)
        }
        Err(err) => {
            eprintln!("rkgrep: {err:#}");
            ExitCode::from(2)
        }
    }
}

/// What a run found but could not show, said once, on stderr.
///
/// With one pattern an empty result says this already. With several, a typo in
/// one of them is otherwise invisible behind the answers to the others.
fn report_gaps(patterns: &[String], found: &search::Results, kinds: &[String]) {
    if patterns.len() > 1 && !found.unmatched.is_empty() {
        eprintln!("rkgrep: no matches for {}", found.unmatched.join(", "));
    }
    if found.hits.is_empty() && !kinds.is_empty() {
        eprintln!("rkgrep: nothing of that kind; kinds come from the declaring keyword");
    }
}

fn options(cli: &Cli, paths: Option<Vec<PathBuf>>, budget: Option<usize>) -> Options {
    Options {
        max_tokens: budget,
        // A budget is what makes a per-file cap necessary: it stops one
        // crowded module taking the whole of it. With no budget to protect,
        // capping only hides matches the user asked for.
        max_per_file: cli.max_per_file.unwrap_or(match cli.no_budget {
            true => 0,
            false => DEFAULT_MAX_PER_FILE,
        }),
        globs: cli.globs.clone(),
        paths,
        comments_only: cli.comments,
        select: cli.select(),
        kinds: cli.kind.clone(),
        min_references: cli.min_references,
        columns: cli.vimgrep,
        literal: cli.fixed_strings,
        word: cli.word_regexp,
        ignore_case: cli.ignore_case,
        hidden: cli.hidden,
        no_ignore: cli.no_ignore,
        threads: cli.threads,
    }
}

fn print_stats(cli: &Cli, found: &search::Results, started: Instant) {
    let used: usize = found.hits.iter().map(Hit::tokens).sum();
    let ms = |d: std::time::Duration| d.as_secs_f64() * 1e3;
    let budget = match cli.no_budget {
        true => "unlimited".to_string(),
        false => cli.max_tokens.to_string(),
    };
    eprintln!(
        "rkgrep: {} spans, {}/{} tokens, {:.1}ms total",
        found.hits.len(),
        used,
        budget,
        started.elapsed().as_secs_f64() * 1e3,
    );
    eprintln!(
        "rkgrep: walk {:.1}ms ({} files matched), rank {:.1}ms, extract {:.1}ms ({} files read)",
        ms(found.timings.walk),
        found.timings.matching_files,
        ms(found.timings.rank),
        ms(found.timings.extract),
        found.timings.read_files,
    );
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let started = Instant::now();
    // Parsing 200k BPE ranks takes longer than a small query's whole search.
    // Started here, it builds while the parallel walk runs and is ready by the
    // time the serial extract phase counts its first span.
    tokenizer::prewarm();

    let use_color = match cli.color {
        ColorChoice::Always => true,
        ColorChoice::Never => false,
        ColorChoice::Auto => io::stdout().is_terminal(),
    };
    let budget = (!cli.no_budget).then_some(cli.max_tokens);
    let mut style = Style {
        text: !cli.anchors_only,
        color: use_color,
        line_numbers: cli.line_numbers,
        queries: false,
    };

    if !cli.fetch.is_empty() {
        return run_fetch(&cli, style, budget);
    }

    let (patterns, path) = match cli.patterns_and_path() {
        Ok(resolved) => resolved,
        Err(message) => {
            eprintln!("rkgrep: {message}");
            return ExitCode::from(2);
        }
    };

    let root = match search::validate_root(&path) {
        Ok(root) => root,
        Err(err) => {
            eprintln!("rkgrep: {err}");
            return ExitCode::from(2);
        }
    };

    let paths = match path_set(&cli, &root) {
        Ok(paths) => paths,
        Err(err) => {
            eprintln!("rkgrep: {err:#}");
            return ExitCode::from(2);
        }
    };

    let opts = options(&cli, paths, budget);

    let found = match search(&patterns, &root, &opts) {
        Ok(found) => found,
        Err(err) => {
            eprintln!("rkgrep: {err:#}");
            return ExitCode::from(2);
        }
    };

    style.queries = patterns.len() > 1;
    write_out(&match (cli.json, cli.vimgrep) {
        (true, _) => render_json(&found.hits),
        (_, true) => render_vimgrep(&found.hits),
        _ => render_text(&found.hits, style),
    });

    report_gaps(&patterns, &found, &opts.kinds);

    if cli.stats {
        print_stats(&cli, &found, started);
    }

    exit_code(&found.hits)
}
