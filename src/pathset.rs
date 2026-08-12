//! Restricting a search to a list of files rather than a whole tree.
//!
//! `--files-from` and `--since` are one mechanism with two sources: a list the
//! caller supplies, and a list git supplies. Both end as the same set of
//! absolute paths, so the two flags cannot drift apart.

use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

/// Existing files under `root`, in the order given and without repeats.
fn keep_files(paths: Vec<PathBuf>, root: &Path) -> Vec<PathBuf> {
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut kept: Vec<PathBuf> = Vec::new();
    for path in paths {
        let Ok(resolved) = path.canonicalize() else {
            continue;
        };
        if !resolved.is_file() || !resolved.starts_with(root) {
            continue;
        }
        if seen.insert(resolved.clone()) {
            kept.push(resolved);
        }
    }
    kept
}

fn split_list(text: &str, null_separated: bool) -> Vec<PathBuf> {
    let separator = if null_separated { '\0' } else { '\n' };
    text.split(separator)
        .map(|line| line.trim_end_matches('\r'))
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// Paths listed in `source`, or on stdin when it is `-`.
pub fn from_list(source: &str, null_separated: bool, root: &Path) -> Result<Vec<PathBuf>> {
    let text = match source {
        "-" => {
            let mut buffer = String::new();
            std::io::stdin()
                .read_to_string(&mut buffer)
                .context("reading the path list from stdin")?;
            buffer
        }
        path => std::fs::read_to_string(path)
            .with_context(|| format!("reading the path list from {path}"))?,
    };
    Ok(keep_files(split_list(&text, null_separated), root))
}

fn git(args: &[&str], at: &Path) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(at)
        .output()
        .context("running git; is it installed?")?;
    if !output.status.success() {
        bail!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    String::from_utf8(output.stdout).context("git printed something that is not UTF-8")
}

/// Where `HEAD` and `reference` last shared history, or `reference` itself when
/// they share none. Diffing against this rather than `reference` keeps commits
/// made on `reference` after the split out of the result.
fn fork_point(reference: &str, top: &Path) -> String {
    match git(&["merge-base", reference, "HEAD"], top) {
        Ok(base) => base.trim().to_string(),
        Err(_) => reference.to_string(),
    }
}

/// Files this branch changed since it left `reference`, plus files git does not
/// track yet.
pub fn from_git(reference: &str, root: &Path) -> Result<Vec<PathBuf>> {
    // git reads a leading dash as an option, and `git diff` has options that
    // write files. A revision never starts with one.
    if reference.starts_with('-') {
        bail!("not a revision: {reference}");
    }
    let top = git(&["rev-parse", "--show-toplevel"], root)
        .context("--since needs a git repository; use --files-from otherwise")?;
    let top = PathBuf::from(top.trim());

    let base = fork_point(reference, &top);
    let changed = git(&["diff", "--name-only", base.as_str()], &top)
        .with_context(|| format!("no such revision: {reference}"))?;
    let untracked = git(&["ls-files", "--others", "--exclude-standard"], &top)?;

    let paths = split_list(&changed, false)
        .into_iter()
        .chain(split_list(&untracked, false))
        .map(|path| top.join(path))
        .collect();
    Ok(keep_files(paths, root))
}
