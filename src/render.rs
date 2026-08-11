//! Output formatting.

use crate::search::Hit;

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
}

/// Anchored, model-readable rendering of a result set.
///
/// Every span leads with `path:start-end` so a reader — human or model — can
/// go straight to the source rather than searching for the snippet again.
pub fn render_text(hits: &[Hit], style: Style) -> String {
    let (dim, bold, reset) = match style.color {
        true => (DIM, BOLD, RESET),
        false => ("", "", ""),
    };
    let mut out = String::new();
    for (i, hit) in hits.iter().enumerate() {
        if i > 0 && style.text {
            out.push('\n');
        }
        out.push_str(bold);
        out.push_str(&hit.anchor());
        out.push_str(reset);
        if let (Some(symbol), Some(kind)) = (&hit.symbol, &hit.kind) {
            out.push_str(&format!(" {dim}({kind} {symbol}){reset}"));
        }
        out.push_str(&format!(" {dim}[{} tok]{reset}", hit.tokens()));
        if style.queries {
            out.push_str(&format!(" {dim}for {}{reset}", hit.query));
        }
        out.push('\n');
        if style.text {
            push_source(&mut out, hit, style);
        }
    }
    // Callers add their own trailing newline.
    while out.ends_with('\n') {
        out.pop();
    }
    out
}

/// A span's source, with its matched lines told apart from the rest.
///
/// Without `-n` this is the file's own bytes and nothing else, because rkgrep
/// output gets pasted into editors and into context windows: a marker
/// character in the margin would corrupt every paste to solve a display
/// problem. Colour says which lines matched and changes no bytes at all.
///
/// `-n` numbers the lines and separates a matched line with `:` and any other
/// with `-`, which is what ripgrep does and what every reader of grep output
/// already knows.
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
///
/// Span headers are relative to the search root, which is what lets `-l`
/// output feed `--fetch`. Quickfix knows no such root, so `--vimgrep` puts it
/// back. A single-file search has nothing below the root, and the root is then
/// the whole path.
fn located(root: &str, path: &str) -> String {
    match (root, path) {
        (_, "") => root.to_string(),
        ("." | "", _) => path.to_string(),
        _ => format!("{}/{path}", root.trim_end_matches('/')),
    }
}

/// One line per match, as `path:line:col:text`.
///
/// A different unit from a span on purpose: this is what quickfix, fzf and
/// every editor's grep integration read, and they expect one jumpable
/// position per line.
pub fn render_vimgrep(hits: &[Hit], root: &str) -> String {
    let mut matches: Vec<(&str, u64, u64, &str)> = Vec::new();
    for hit in hits {
        for (at, line) in hit.match_lines.iter().enumerate() {
            let column = hit.match_columns.get(at).copied().unwrap_or(1);
            matches.push((
                hit.path.as_str(),
                *line,
                column,
                hit.line(*line).unwrap_or(""),
            ));
        }
    }

    // Rank order scatters one file's matches across several blocks. Quickfix
    // and fzf read a file as one ascending run, so the line unit is ordered
    // like lines rather than like spans.
    matches.sort_unstable_by_key(|(path, line, ..)| (*path, *line));

    let mut out = String::new();
    for (path, line, column, text) in matches {
        out.push_str(&format!("{}:{line}:{column}:{text}\n", located(root, path)));
    }
    while out.ends_with('\n') {
        out.pop();
    }
    out
}

pub fn render_json(hits: &[Hit]) -> String {
    serde_json::to_string_pretty(hits).unwrap_or_else(|_| "[]".to_string())
}
