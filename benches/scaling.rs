//! Thread scaling of rkgrep against ripgrep on a warm page cache.
//!
//! Both tools are driven as subprocesses on identical input, so the numbers
//! include process startup and are directly comparable to what a user times
//! at a shell. Each cell is a median over repeated trials with a bootstrap
//! interval, because process timings are right-skewed by scheduling noise and
//! a single best-of-N hides that.
//!
//! ```console
//! cargo bench --bench scaling -- <tree>
//! cargo bench --bench scaling -- <tree> --threads 1 4 16 --repeats 9
//! ```

#[path = "support/mod.rs"]
mod support;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use clap::Parser as ClapParser;

use support::stats::{median, median_ci95, stddev};

const DEFAULT_BINARY: &str = "target/release/rkgrep";

/// Passes that pull the tree into the page cache before anything is timed.
const WARM_PASSES: usize = 2;

/// A token no corpus contains, so warming walks every file and matches none.
const WARM_PATTERN: &str = "zzz_no_such_token_zzz";

#[derive(ClapParser)]
#[command(name = "scaling", about = "Thread scaling of rkgrep against ripgrep")]
struct Args {
    /// Tree to search
    tree: PathBuf,

    /// rkgrep binary under test
    #[arg(long, default_value = DEFAULT_BINARY)]
    binary: PathBuf,

    /// Patterns to time, each reported separately
    #[arg(long, num_args = 1.., default_values = ["function", "Result", "config"])]
    patterns: Vec<String>,

    /// Thread counts to sweep; the first is the scaling baseline
    #[arg(long, num_args = 1.., default_values_t = [1usize, 2, 4, 8, 16, 32])]
    threads: Vec<usize>,

    /// Timed trials per cell
    #[arg(long, default_value_t = 7)]
    repeats: usize,

    /// Accepted and ignored: `cargo bench` passes it to every bench target.
    #[arg(long, hide = true)]
    bench: bool,
}

fn time_once(cmd: &mut Command) -> f64 {
    let started = Instant::now();
    let _ = cmd.stdout(Stdio::null()).stderr(Stdio::null()).status();
    started.elapsed().as_secs_f64() * 1e3
}

fn rkgrep_cmd(binary: &Path, pattern: &str, tree: &Path, threads: usize) -> Command {
    let mut cmd = Command::new(binary);
    cmd.arg(pattern)
        .arg(tree)
        .args(["--threads", &threads.to_string()])
        .args(["--no-ignore", "--hidden"]);
    cmd
}

fn rg_cmd(pattern: &str, tree: &Path, threads: usize) -> Command {
    let mut cmd = Command::new("rg");
    cmd.args(["-j", &threads.to_string()])
        .args(["--no-ignore", "--hidden", "-c", "--", pattern])
        .arg(tree);
    cmd
}

/// One cell of the table: the timings for a tool at a thread count.
struct Cell {
    median: f64,
    stddev: f64,
    ci: (f64, f64),
}

fn measure(mut cmd: impl FnMut() -> Command, repeats: usize) -> Cell {
    let samples: Vec<f64> = (0..repeats).map(|_| time_once(&mut cmd())).collect();
    Cell {
        median: median(&samples),
        stddev: stddev(&samples),
        ci: median_ci95(&samples),
    }
}

fn warm(tree: &Path) {
    for _ in 0..WARM_PASSES {
        let _ = rg_cmd(WARM_PATTERN, tree, 32)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn main() -> std::process::ExitCode {
    let args = Args::parse();

    if !args.binary.exists() {
        eprintln!(
            "scaling: no rkgrep binary at {}; run `cargo build --release` first",
            args.binary.display()
        );
        return std::process::ExitCode::from(2);
    }
    let Ok(tree) = args.tree.canonicalize() else {
        eprintln!("scaling: cannot read {}", args.tree.display());
        return std::process::ExitCode::from(2);
    };
    if args.threads.is_empty() {
        eprintln!("scaling: --threads needs at least one value");
        return std::process::ExitCode::from(2);
    }
    // A cell with no trials has no median, and the speedup column divides by
    // the baseline cell's, so an empty one prints `inf` rather than failing.
    if args.repeats == 0 {
        eprintln!("scaling: --repeats needs at least one trial");
        return std::process::ExitCode::from(2);
    }

    eprintln!("warming page cache on {} ...", tree.display());
    warm(&tree);
    println!(
        "{} trials per cell, median with a 95% bootstrap interval\n",
        args.repeats
    );

    for pattern in &args.patterns {
        let rk: Vec<Cell> = args
            .threads
            .iter()
            .map(|&t| measure(|| rkgrep_cmd(&args.binary, pattern, &tree, t), args.repeats))
            .collect();
        let rg: Vec<Cell> = args
            .threads
            .iter()
            .map(|&t| measure(|| rg_cmd(pattern, &tree, t), args.repeats))
            .collect();
        let (base_rk, base_rg) = (rk[0].median, rg[0].median);

        println!("pattern={pattern:?}");
        println!(
            "{:>7} {:>9} {:>8} {:>18} {:>9} {:>8} {:>7}",
            "threads", "rkgrep", "speedup", "95% CI", "rg", "speedup", "ratio"
        );
        for (i, &t) in args.threads.iter().enumerate() {
            let (a, b) = (&rk[i], &rg[i]);
            println!(
                "{:>7} {:>7.1}ms {:>7.2}x {:>18} {:>7.1}ms {:>7.2}x {:>6.2}x",
                t,
                a.median,
                base_rk / a.median,
                format!("[{:.1}, {:.1}]", a.ci.0, a.ci.1),
                b.median,
                base_rg / b.median,
                a.median / b.median,
            );
        }
        let noisiest = rk
            .iter()
            .map(|c| c.stddev / c.median)
            .fold(0.0f64, f64::max);
        println!(
            "worst relative spread across rkgrep cells: {:.1}%\n",
            noisiest * 100.0
        );
    }
    std::process::ExitCode::SUCCESS
}
