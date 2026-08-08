//! rkgrep — ranked, span-scoped, budget-packed code search.

mod render;
mod search;
mod spans;

use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use clap::{Parser, ValueEnum};

use crate::render::{render_json, render_text};
use crate::search::{search, Options};

#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum ColorChoice {
    Auto,
    Always,
    Never,
}

const AFTER_HELP: &str = "\
EXAMPLES:
  rkgrep validate_token              rank every hit, pack 2000 tokens
  rkgrep -w handle_request src/      whole words only, under src/
  rkgrep -t 8000 -g '*.py' Config    bigger budget, Python files only
  rkgrep --json parse_url | jq .     machine-readable output
  rkgrep -l TODO                     anchors only, no source text

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
    pattern: String,

    /// Directory or file to search
    #[arg(default_value = ".")]
    path: PathBuf,

    /// Token budget for the whole result set
    #[arg(short = 't', long, default_value_t = 2000, value_name = "N")]
    max_tokens: usize,

    /// Cap spans returned from any one file
    #[arg(long, default_value_t = 3, value_name = "N")]
    max_per_file: usize,

    /// Restrict to matching files (repeatable, e.g. -g '*.rs')
    #[arg(short = 'g', long = "glob", value_name = "GLOB")]
    globs: Vec<String>,

    /// Treat the pattern as a literal string
    #[arg(short = 'F', long)]
    fixed_strings: bool,

    /// Match whole words only
    #[arg(short = 'w', long)]
    word_regexp: bool,

    /// Case-insensitive matching
    #[arg(short = 'i', long)]
    ignore_case: bool,

    /// Search hidden files and directories
    #[arg(long)]
    hidden: bool,

    /// Do not respect .gitignore and friends
    #[arg(long)]
    no_ignore: bool,

    /// Print anchors without source text
    #[arg(short = 'l', long)]
    anchors_only: bool,

    /// Emit JSON
    #[arg(long)]
    json: bool,

    /// Colorize headers
    #[arg(long, value_enum, default_value = "auto")]
    color: ColorChoice,

    /// Print timing and budget use to stderr
    #[arg(long)]
    stats: bool,

    /// Worker threads (0 chooses automatically)
    #[arg(long, default_value_t = 0, value_name = "N")]
    threads: usize,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let started = Instant::now();

    let root = match search::validate_root(&cli.path) {
        Ok(root) => root,
        Err(err) => {
            eprintln!("rkgrep: {err}");
            return ExitCode::from(2);
        }
    };

    let opts = Options {
        max_tokens: cli.max_tokens,
        max_per_file: cli.max_per_file,
        globs: cli.globs.clone(),
        literal: cli.fixed_strings,
        word: cli.word_regexp,
        ignore_case: cli.ignore_case,
        hidden: cli.hidden,
        no_ignore: cli.no_ignore,
        threads: cli.threads,
    };

    let (hits, timings) = match search(&cli.pattern, &root, &opts) {
        Ok(result) => result,
        Err(err) => {
            eprintln!("rkgrep: {err:#}");
            return ExitCode::from(2);
        }
    };

    let use_color = match cli.color {
        ColorChoice::Always => true,
        ColorChoice::Never => false,
        ColorChoice::Auto => io::stdout().is_terminal(),
    };

    let out = io::stdout();
    let mut out = io::BufWriter::new(out.lock());
    let rendered = if cli.json {
        render_json(&hits)
    } else {
        render_text(&hits, !cli.anchors_only, use_color)
    };
    if !rendered.is_empty() {
        let _ = writeln!(out, "{rendered}");
    }
    let _ = out.flush();

    if cli.stats {
        let used: usize = hits.iter().map(|h| h.tokens).sum();
        let ms = |d: std::time::Duration| d.as_secs_f64() * 1e3;
        eprintln!(
            "rkgrep: {} spans, {}/{} tokens, {:.1}ms total",
            hits.len(),
            used,
            cli.max_tokens,
            started.elapsed().as_secs_f64() * 1e3,
        );
        eprintln!(
            "rkgrep: walk {:.1}ms ({} files matched), rank {:.1}ms, extract {:.1}ms ({} files read)",
            ms(timings.walk),
            timings.matching_files,
            ms(timings.rank),
            ms(timings.extract),
            timings.read_files,
        );
    }

    // 0 when something matched, 1 when nothing did, as grep does.
    if hits.is_empty() {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}
