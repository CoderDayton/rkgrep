//! Where a declaration's body ends, read from indentation alone.
//!
//! Indentation rather than a parser, because the whole extractor is
//! language-agnostic; a language that does not indent its bodies simply
//! reports every declaration at depth 0 and ends each one at its own line.

/// Delimiters a body can close with, and the punctuation that may trail them:
/// `}`, `};`, `]),`.
const BODY_CLOSERS: &[u8] = b")]};,";

/// Where a line's text begins, or `None` if the line holds nothing.
pub(super) fn indent_of(line: &str) -> Option<usize> {
    let text = line.trim_start_matches([' ', '\t']);
    (!text.is_empty()).then(|| line.len() - text.len())
}

/// Whether a line holds nothing but what closes a body.
fn closes_a_body(line: &str) -> bool {
    let text = line.trim_matches([' ', '\t']);
    if text.is_empty() {
        return false;
    }
    text == "end" || text.bytes().all(|b| BODY_CLOSERS.contains(&b))
}

/// The last line of the body each line opens, 1-based, one entry per line.
///
/// Every body is resolved in one pass over the file rather than one forward
/// scan per declaration: a file whose indentation only ever deepens gives each
/// scan the whole remainder to walk, which is `O(lines²)` over the file and
/// stalls a query on a few thousand lines.
pub(super) fn body_ends(lines: &[&str]) -> Vec<u64> {
    let indents: Vec<Option<usize>> = lines.iter().map(|line| indent_of(line)).collect();

    // The last line holding text before each one, so a body ends on the last
    // line that belonged to it rather than on the blank lines trailing it.
    // One entry longer than the file, for a body that runs to the end.
    let mut last_filled: Vec<Option<usize>> = Vec::with_capacity(lines.len() + 1);
    let mut filled: Option<usize> = None;
    for indent in &indents {
        last_filled.push(filled);
        if indent.is_some() {
            filled = Some(last_filled.len() - 1);
        }
    }
    last_filled.push(filled);

    // Where each line's body stops: the first line below it at or above its own
    // indentation. Read backwards, the lines still on the stack are exactly
    // those no later line has closed, so the nearest one is the answer.
    let mut stop_at: Vec<Option<usize>> = vec![None; lines.len()];
    let mut open: Vec<(usize, usize)> = Vec::new();
    for index in (0..lines.len()).rev() {
        let Some(indent) = indents[index] else {
            continue;
        };
        while open.last().is_some_and(|&(_, deeper)| deeper > indent) {
            open.pop();
        }
        stop_at[index] = open.last().map(|&(at, _)| at);
        open.push((index, indent));
    }

    (0..lines.len())
        .map(|index| match stop_at[index] {
            Some(at) if closes_a_body(lines[at]) => at as u64 + 1,
            stop => match last_filled[stop.unwrap_or(lines.len())] {
                Some(at) if at > index => at as u64 + 1,
                _ => index as u64 + 1,
            },
        })
        .collect()
}
