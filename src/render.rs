//! Output formatting.
//!
//! Every renderer writes as it goes rather than returning one finished string.
//! A result set with no budget on it is the whole of what a query matched, and
//! holding that twice — once as spans, once as the text of them — is what
//! decides whether a large repository is searchable at all.

use std::io::{self, Write};

use crate::search::{Hit, Reasons};

const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";

/// Width the line-number gutter is padded to, so a span reads as a block
/// rather than as a ragged left edge.
const GUTTER: usize = 6;

/// How a result set is written out.
#[derive(Debug, Clone, Copy, Default)]
pub struct Style {
    /// Print the source, not only the anchors.
    pub text: bool,
    pub color: bool,
    /// Number every line, marking the ones that matched.
    pub line_numbers: bool,
    /// Name the pattern each span answers, for a run that carried several.
    pub queries: bool,
    /// Show what each span's score is made of.
    pub why: bool,
}

/// Anchored, model-readable rendering of a result set.
pub fn render_text(out: &mut impl Write, hits: &[Hit], style: Style) -> io::Result<()> {
    let (dim, bold, reset) = match style.color {
        true => (DIM, BOLD, RESET),
        false => ("", "", ""),
    };
    let mut chunk = String::new();
    for (i, hit) in hits.iter().enumerate() {
        chunk.clear();
        if i > 0 && style.text {
            chunk.push('\n');
        }
        chunk.push_str(bold);
        chunk.push_str(&hit.anchor());
        chunk.push_str(reset);
        if let (Some(symbol), Some(kind)) = (&hit.symbol, &hit.kind) {
            chunk.push_str(&format!(" {dim}({kind} {symbol}){reset}"));
        }
        chunk.push_str(&format!(" {dim}[{} tok]{reset}", hit.tokens()));
        if style.queries {
            chunk.push_str(&format!(" {dim}for {}{reset}", hit.query));
        }
        chunk.push('\n');
        if let (true, Some(reasons)) = (style.why, &hit.reasons) {
            push_reasons(&mut chunk, hit, reasons, style);
        }
        if style.text {
            push_source(&mut chunk, hit, style);
        }
        // However many blank lines the last span ends on, the run ends on one.
        if i + 1 == hits.len() {
            while chunk.ends_with('\n') {
                chunk.pop();
            }
            chunk.push('\n');
        }
        out.write_all(chunk.as_bytes())?;
    }
    Ok(())
}

/// Why one span scored what it did, written as the sum it is.
///
/// Declarations sort ahead of everything else whatever they score, so a span
/// that got there that way says so: the number alone would not explain it.
fn push_reasons(out: &mut String, hit: &Hit, reasons: &Reasons, style: Style) {
    let (dim, reset) = match style.color {
        true => (DIM, RESET),
        false => ("", ""),
    };
    let first = match hit.is_declaration {
        true => ", declaration",
        false => "",
    };
    out.push_str(&format!(
        "{dim}  why matches {:.2} + terms {:.2} + path {:.2} - depth {:.2} = {:.2}{first}{reset}\n",
        reasons.matches,
        reasons.terms,
        reasons.path,
        reasons.depth_penalty,
        reasons.total(),
    ));
}

/// A span's source, with its matched lines told apart from the rest.
fn push_source(out: &mut String, hit: &Hit, style: Style) {
    let plain = !style.line_numbers && !style.color;
    if plain {
        out.push_str(&hit.text);
        out.push('\n');
        return;
    }
    for (offset, line) in hit.text.lines().enumerate() {
        let number = hit.start_line + offset as u64;
        let matched = hit.match_lines.binary_search(&number).is_ok();
        if style.line_numbers {
            let separator = if matched { ':' } else { '-' };
            match style.color {
                true => out.push_str(&format!("{DIM}{number:>GUTTER$}{separator}{RESET}")),
                false => out.push_str(&format!("{number:>GUTTER$}{separator}")),
            }
        }
        match style.color && matched {
            true => out.push_str(&format!("{BOLD}{line}{RESET}")),
            false => out.push_str(line),
        }
        out.push('\n');
    }
}

/// The path as the caller would have to type it to open the file.
fn located(root: &str, path: &str) -> String {
    match (root, path) {
        (_, "") => root.to_string(),
        ("." | "", _) => path.to_string(),
        _ => format!("{}/{path}", root.trim_end_matches('/')),
    }
}

/// One line per match, as `path:line:col:text`.
pub fn render_vimgrep(out: &mut impl Write, hits: &[Hit], root: &str) -> io::Result<()> {
    let mut matches: Vec<(&str, u64, u64, &str)> = Vec::new();
    for hit in hits {
        // The lines of a span in one pass, rather than a fresh scan from its
        // start for each match landing in it.
        let mut lines = hit.text.lines().enumerate();
        let mut at_line = |wanted: u64| {
            let offset = wanted.checked_sub(hit.start_line)? as usize;
            lines
                .find(|(seen, _)| *seen == offset)
                .map(|(_, text)| text)
        };
        for (at, line) in hit.match_lines.iter().enumerate() {
            let column = hit.match_columns.get(at).copied().unwrap_or(1);
            matches.push((
                hit.path.as_str(),
                *line,
                column,
                at_line(*line).unwrap_or(""),
            ));
        }
    }

    matches.sort_unstable_by_key(|(path, line, ..)| (*path, *line));

    for (path, line, column, text) in matches {
        writeln!(out, "{}:{line}:{column}:{text}", located(root, path))?;
    }
    Ok(())
}

pub fn render_json(out: &mut impl Write, hits: &[Hit]) -> io::Result<()> {
    if let Err(err) = serde_json::to_writer_pretty(&mut *out, hits) {
        // Keep the kind of a write that failed, so a closed pipe stays
        // tellable from a real error.
        return Err(match err.io_error_kind() {
            Some(kind) => io::Error::from(kind),
            None => io::Error::other(err),
        });
    }
    out.write_all(b"\n")
}
