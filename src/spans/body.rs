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
///
/// The closer sits back at the declaration's own indent, so the body run ends
/// above it and it would otherwise be left out of the span. A function
/// reported without its closing brace reads as truncated source.
fn closes_a_body(line: &str) -> bool {
    let text = line.trim_matches([' ', '\t']);
    if text.is_empty() {
        return false;
    }
    // Ruby and Lua close with a word rather than a delimiter.
    text == "end" || text.bytes().all(|b| BODY_CLOSERS.contains(&b))
}

/// The last line of the body `start_line` opens.
///
/// The body is the run of lines indented deeper than the declaration itself.
/// Blank lines neither end it nor extend it — a paragraph break inside a
/// function does not close the function, and a span should not trail off into
/// the gap below its last statement.
pub(super) fn body_end(lines: &[&str], start_line: u64, indent: usize) -> u64 {
    let mut end = start_line;
    for (offset, line) in lines.iter().enumerate().skip(start_line as usize) {
        match indent_of(line) {
            None => {}
            Some(deeper) if deeper > indent => end = offset as u64 + 1,
            Some(_) => {
                if closes_a_body(line) {
                    end = offset as u64 + 1;
                }
                break;
            }
        }
    }
    end
}
