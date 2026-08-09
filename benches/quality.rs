//! Retrieval quality of the rkgrep binary against parser-derived ground truth.
//!
//! Ground truth comes from a tree-sitter grammar for the language under test,
//! not from any of the systems being measured, so nothing here scores itself.
//! For each query symbol the truth is the file that declares it plus the files
//! that reference it.
//!
//! ```console
//! cargo bench --bench quality -- <repo>
//! cargo bench --bench quality -- <repo> --lang go --queries 300
//! ```
//!
//! The release binary is the thing measured, so build it first.

#[path = "support/mod.rs"]
mod support;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use clap::Parser as ClapParser;

use support::corpus::collect;
use support::metrics::{coverage, mrr, under_budget};
use support::stats::{mean, median, median_ci95, Rng};
use support::tokenizer::count as count_tokens;
use support::truth::{self, Language};

const DEFAULT_BINARY: &str = "target/release/rkgrep";

#[derive(ClapParser)]
#[command(
    name = "quality",
    about = "Retrieval quality against tree-sitter ground truth"
)]
struct Args {
    /// Repository to measure retrieval over
    root: PathBuf,

    /// Language whose grammar supplies the ground truth
    #[arg(long, default_value = "python")]
    lang: String,

    /// rkgrep binary under test
    #[arg(long, default_value = DEFAULT_BINARY)]
    binary: PathBuf,

    /// Number of query symbols to sample
    #[arg(long, default_value_t = 120)]
    queries: usize,

    /// Sampling seed, so runs are comparable
    #[arg(long, default_value_t = 42)]
    seed: u64,

    /// Token budget the result set must fit
    #[arg(long, default_value_t = 2000)]
    max_tokens: usize,

    /// Lines of context the rg baseline keeps around each match
    #[arg(long, default_value_t = 10)]
    context: usize,

    /// List the queries rkgrep ranks below the baseline, worst first
    #[arg(long, default_value_t = 0, value_name = "N")]
    show_failures: usize,

    /// Accepted and ignored: `cargo bench` passes it to every bench target.
    #[arg(long, hide = true)]
    bench: bool,
}

/// A symbol, the file that declares it, and the files that mention it.
struct Query {
    name: String,
    definition: String,
    neighborhood: BTreeSet<String>,
}

/// Symbols with one declaration site and at least one external reference.
///
/// One declaration site makes the answer unambiguous; an external reference
/// keeps retrieval from being trivial.
fn build_queries(t: &truth::Truth, limit: usize, seed: u64) -> Vec<Query> {
    let mut candidates: Vec<Query> = Vec::new();
    for (name, defs) in &t.definitions {
        if defs.len() != 1 || name.starts_with("__") {
            continue;
        }
        let home = defs.iter().next().expect("length checked above").clone();
        let refs = t.references.get(name).cloned().unwrap_or_default();
        if refs.iter().filter(|p| **p != home).count() < 1 {
            continue;
        }
        let mut neighborhood = refs;
        neighborhood.insert(home.clone());
        candidates.push(Query {
            name: name.clone(),
            definition: home,
            neighborhood,
        });
    }
    // `definitions` is a BTreeMap, so candidates arrive sorted by name and the
    // shuffle below is the only source of order.
    Rng::new(seed).shuffle(&mut candidates);
    candidates.truncate(limit);
    candidates
}

// ---------------------------------------------------------------------------
// Systems under test. Each returns a ranked list of (path, token_cost).
// ---------------------------------------------------------------------------

fn run_capture(cmd: &mut Command) -> String {
    let out = cmd.stderr(Stdio::null()).output();
    match out {
        Ok(out) => String::from_utf8_lossy(&out.stdout).into_owned(),
        Err(_) => String::new(),
    }
}

fn rel(root: &Path, path: &str) -> Option<String> {
    Path::new(path)
        .strip_prefix(root)
        .ok()
        .and_then(|p| p.to_str())
        .map(str::to_string)
}

/// One matched line: where it is and what it says.
struct MatchedLine {
    lineno: usize,
    text: String,
}

/// Every matching line in the tree, from a single `rg -nw` run.
///
/// One invocation, because the latency column compares this against one rkgrep
/// invocation. Spawning a second `rg` for counts and a third for declaration
/// lines would put two extra process startups on the baseline's clock and none
/// on rkgrep's, and the column would be measuring `fork`. Counts and
/// declaration lines are both recoverable from this output: a declaration line
/// contains the name as a whole word, so `-w` cannot have hidden one.
fn rg_matches(root: &Path, name: &str, glob: &str) -> BTreeMap<String, Vec<MatchedLine>> {
    let pattern = regex::escape(name);
    let text = run_capture(
        Command::new("rg")
            .args(["-n", "-w", "--no-messages", "-g", glob, "--", &pattern])
            .arg(root),
    );
    let mut hits: BTreeMap<String, Vec<MatchedLine>> = BTreeMap::new();
    for line in text.lines() {
        let Some((path, rest)) = line.split_once(':') else {
            continue;
        };
        let Some((lineno, text)) = rest.split_once(':') else {
            continue;
        };
        let (Some(path), Ok(lineno)) = (rel(root, path), lineno.parse::<usize>()) else {
            continue;
        };
        hits.entry(path).or_default().push(MatchedLine {
            lineno,
            text: text.to_string(),
        });
    }
    hits
}

/// Files whose declaration line matches — what someone who knows rg types.
fn rg_defs(
    hits: &BTreeMap<String, Vec<MatchedLine>>,
    name: &str,
    lang: &Language,
) -> BTreeSet<String> {
    let pattern = lang.def_pattern.replace("{name}", &regex::escape(name));
    let Ok(re) = regex::Regex::new(&pattern) else {
        return BTreeSet::new();
    };
    hits.iter()
        .filter(|(_, lines)| lines.iter().any(|l| re.is_match(&l.text)))
        .map(|(path, _)| path.clone())
        .collect()
}

/// Token cost of each file's matched lines +/- context, merged.
///
/// The fair packing baseline: nobody pastes a whole file into a context window
/// when grep already told them which lines matched.
fn rg_regions(
    hits: &BTreeMap<String, Vec<MatchedLine>>,
    files: &BTreeMap<String, String>,
    context: usize,
) -> BTreeMap<String, usize> {
    let mut costs = BTreeMap::new();
    for (path, matched) in hits {
        let Some(source) = files.get(path) else {
            continue;
        };
        let lines: Vec<&str> = source.lines().collect();
        let mut wanted = BTreeSet::new();
        for m in matched {
            let lo = m.lineno.saturating_sub(context).max(1);
            let hi = (m.lineno + context).min(lines.len());
            wanted.extend(lo..=hi);
        }
        if wanted.is_empty() {
            continue;
        }
        let region: Vec<&str> = wanted.iter().map(|i| lines[i - 1]).collect();
        costs.insert(path.clone(), count_tokens(&region.join("\n")));
    }
    costs
}

/// rg's declarations-first ranking, charged for the regions it returns.
fn run_rg_span(
    root: &Path,
    name: &str,
    files: &BTreeMap<String, String>,
    lang: &Language,
    context: usize,
) -> Vec<(String, usize)> {
    let hits = rg_matches(root, name, lang.glob);
    let defs = rg_defs(&hits, name, lang);
    let costs = rg_regions(&hits, files, context);
    let mut counted: Vec<(String, usize)> = hits
        .iter()
        .map(|(path, lines)| (path.clone(), lines.len()))
        .collect();
    counted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let (mut first, rest): (Vec<_>, Vec<_>) =
        counted.into_iter().partition(|(p, _)| defs.contains(p));
    first.extend(rest);
    first
        .into_iter()
        .filter_map(|(p, _)| costs.get(&p).map(|c| (p.clone(), *c)))
        .collect()
}

/// The spans rkgrep returns, in rank order, charged what rkgrep says they cost.
fn run_rkgrep(
    binary: &Path,
    root: &Path,
    name: &str,
    glob: &str,
    max_tokens: usize,
) -> Vec<(String, usize)> {
    // Escaped, as the rg side is: a JavaScript identifier may contain `$`, and
    // an unescaped one is an anchor that matches nothing on either side.
    let text = run_capture(
        Command::new(binary)
            .arg(regex::escape(name))
            .arg(root)
            .args(["--json", "-w", "-g", glob, "-t", &max_tokens.to_string()]),
    );
    if text.trim().is_empty() {
        return Vec::new();
    }
    let Ok(hits) = serde_json::from_str::<Vec<serde_json::Value>>(&text) else {
        return Vec::new();
    };
    hits.iter()
        .filter_map(|h| {
            Some((
                h.get("path")?.as_str()?.to_string(),
                h.get("tokens")?.as_u64()? as usize,
            ))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Measurement
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Scores {
    mrr: Vec<f64>,
    def: Vec<f64>,
    cov: Vec<f64>,
    latency_ms: Vec<f64>,
    /// The head of each ranked list, kept so a loss can be explained rather
    /// than only counted.
    top: Vec<Vec<String>>,
}

/// How many leading paths to remember per query for `--show-failures`.
const TOP_KEPT: usize = 3;

/// The queries rkgrep ranks worst relative to the baseline.
///
/// A mean hides which symbols moved, and the shape of a declaration the
/// extractor misreads is visible only in the individual case.
fn show_failures(queries: &[Query], rg: &Scores, rk: &Scores, limit: usize) {
    let mut losses: Vec<(f64, usize)> = (0..queries.len())
        .map(|i| (rg.mrr[i] - rk.mrr[i], i))
        .filter(|(gap, _)| *gap > 0.0)
        .collect();
    losses.sort_by(|a, b| b.0.partial_cmp(&a.0).expect("gaps are never NaN"));
    losses.truncate(limit);

    if losses.is_empty() {
        println!("\nrkgrep ranks the declaring file at least as highly as rg on every query.");
        return;
    }
    println!(
        "\n{} queries where rg ranks the declaration higher:",
        losses.len()
    );
    for (gap, i) in losses {
        let q = &queries[i];
        println!(
            "\n  {} (gap {:.2})\n    declared in: {}\n    rkgrep top:  {}\n    rg top:      {}",
            q.name,
            gap,
            q.definition,
            rk.top[i].join(", "),
            rg.top[i].join(", "),
        );
    }
}

fn measure(
    queries: &[Query],
    max_tokens: usize,
    mut system: impl FnMut(&str) -> Vec<(String, usize)>,
) -> Scores {
    let mut s = Scores::default();
    for q in queries {
        let started = Instant::now();
        let ranked = system(&q.name);
        s.latency_ms.push(started.elapsed().as_secs_f64() * 1e3);

        let kept = under_budget(&ranked, max_tokens);
        s.mrr.push(mrr(&ranked, &q.definition));
        s.def.push(if kept.contains(&q.definition) {
            1.0
        } else {
            0.0
        });
        s.cov.push(coverage(&kept, &q.neighborhood));

        let mut top: Vec<String> = Vec::new();
        for (path, _) in &ranked {
            if !top.contains(path) {
                top.push(path.clone());
            }
            if top.len() == TOP_KEPT {
                break;
            }
        }
        s.top.push(top);
    }
    s
}

fn report(rows: &[(&str, Scores)]) {
    println!(
        "{:<10}{:>10}{:>12}{:>12}{:>12}{:>18}",
        "system", "MRR(def)", "def@budget", "cov@budget", "latency", "95% CI"
    );
    println!("{}", "-".repeat(74));
    for (label, s) in rows {
        let (lo, hi) = median_ci95(&s.latency_ms);
        println!(
            "{:<10}{:>10.3}{:>11.1}%{:>11.1}%{:>10.1}ms{:>16}",
            label,
            mean(&s.mrr),
            mean(&s.def) * 100.0,
            mean(&s.cov) * 100.0,
            median(&s.latency_ms),
            format!("[{lo:.1}, {hi:.1}]ms"),
        );
    }
    println!(
        "\nMRR(def)    rank of the file that declares the symbol\n\
         def@budget  how often that file survives the token budget\n\
         cov@budget  CONFOUNDED: the neighborhood is files that textually\n\
         \x20           reference the name, which is what grep computes, so it\n\
         \x20           rewards returning more files. Read the first two.\n\
         latency     median over the queries, with a bootstrap interval"
    );
}

fn main() -> std::process::ExitCode {
    let args = Args::parse();

    let Some(lang) = truth::by_name(&args.lang) else {
        let known: Vec<&str> = truth::ALL.iter().map(|l| l.name).collect();
        eprintln!(
            "quality: unknown --lang {}; known: {}",
            args.lang,
            known.join(", ")
        );
        return std::process::ExitCode::from(2);
    };
    if !args.binary.exists() {
        eprintln!(
            "quality: no rkgrep binary at {}; run `cargo build --release` first",
            args.binary.display()
        );
        return std::process::ExitCode::from(2);
    }
    let Ok(root) = args.root.canonicalize() else {
        eprintln!("quality: cannot read {}", args.root.display());
        return std::process::ExitCode::from(2);
    };

    let files = collect(&root, lang.suffixes);
    let parsed = lang.parse_truth(&files);
    let queries = build_queries(&parsed, args.queries, args.seed);

    println!("repo:    {}", root.display());
    println!(
        "corpus:  {} {} files ({} unparsed), lang={}",
        files.len(),
        lang.glob,
        parsed.unparsed,
        lang.name
    );
    println!(
        "queries: {}, budget {} tokens\n",
        queries.len(),
        args.max_tokens
    );

    if queries.is_empty() {
        eprintln!(
            "quality: no {} symbol has one declaration site and an external \
             reference; nothing to measure",
            lang.name
        );
        return std::process::ExitCode::from(1);
    }

    let rg = measure(&queries, args.max_tokens, |name| {
        run_rg_span(&root, name, &files, lang, args.context)
    });
    let rk = measure(&queries, args.max_tokens, |name| {
        run_rkgrep(&args.binary, &root, name, lang.glob, args.max_tokens)
    });
    if args.show_failures > 0 {
        show_failures(&queries, &rg, &rk, args.show_failures);
    }
    report(&[("rg-span", rg), ("rkgrep", rk)]);
    std::process::ExitCode::SUCCESS
}
