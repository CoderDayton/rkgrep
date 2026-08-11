//! Declaration extraction with offset-preserving comment/string masking.
//!
//! The unit rkgrep returns is a declaration, not a fixed window of N lines: a
//! function is something a reader understands, `±10 lines` is an arbitrary cut
//! through one. Extraction is heuristic and language-agnostic — one keyword set
//! driving one set of hand-written scans — so it works on anything ripgrep can
//! match without needing a parser per language.
//!
//! Masking runs before matching so a declaration written inside a comment or a
//! string literal is never reported. Masked regions are overwritten with
//! spaces rather than removed, preserving byte offsets and newline positions,
//! so line numbers taken from the masked text are valid against the original.
//!
//! One module per concern:
//!
//! - [`words`] — the keyword tables and byte sets every rule is written against
//! - [`mask`] — comments and literals blanked out, offsets preserved
//! - [`scan`] — the four shapes a declaration is recognized by, one line at a time
//! - [`body`] — where the body a declaration opens ends
//!
//! [`declarations`] runs them over a whole file; [`enclosing`] answers which
//! declaration owns a given line.

mod body;
mod mask;
mod scan;
mod words;

pub use mask::{comment_source, mask_source};
pub use scan::{declaration_name, kind_declares, scan_declaration};
pub use words::identifier_tokens;

use body::{body_end, indent_of};

/// Above this, ripgrep is welcome to the file but we will not build a
/// declaration table for it.
pub const MAX_SOURCE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declaration {
    pub name: String,
    pub kind: String,
    /// 1-based, inclusive.
    pub start_line: u64,
    /// 1-based, inclusive. Ends at the body the declaration opens, or before
    /// the first declaration nested inside it, whichever comes first — so a
    /// declaration is charged neither for what the file holds after it nor
    /// for the declarations its own body is made of.
    pub end_line: u64,
    /// How deeply the declaration nests, from indentation: 0 for a top-level
    /// declaration, 1 for a method of a top-level class, and so on.
    pub depth: usize,
}

/// Every declaration in `content`, in file order.
pub fn declarations(content: &str) -> Vec<Declaration> {
    if content.len() > MAX_SOURCE_BYTES {
        return Vec::new();
    }
    let masked = mask_source(content);
    let lines: Vec<&str> = masked.lines().collect();
    let mut hits: Vec<(u64, String, String, usize)> = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if let Some((kind, name)) = scan_declaration(line) {
            let indent = indent_of(line).unwrap_or(0);
            hits.push((
                (index + 1) as u64,
                kind.to_string(),
                name.to_string(),
                indent,
            ));
        }
    }

    let mut open: Vec<usize> = Vec::new();
    let mut out = Vec::with_capacity(hits.len());
    for (i, (start_line, kind, name, indent)) in hits.iter().enumerate() {
        // A declaration ends at its body, and one whose body is made of other
        // declarations ends before the first of them: the methods of a class
        // are spans in their own right, so returning the class whole returns
        // them a second time and spends a whole budget on one file. Hits are
        // in file order, so the next one is that first nested declaration
        // whenever the body reaches it, and a sibling the body already stops
        // above.
        let body = body_end(&lines, *start_line, *indent);
        let end_line = match hits.get(i + 1) {
            Some((next, _, _, _)) => body.min(next.saturating_sub(1)),
            None => body,
        }
        .max(*start_line);

        while open.last().is_some_and(|top| *top >= *indent) {
            open.pop();
        }
        out.push(Declaration {
            name: name.clone(),
            kind: kind.clone(),
            start_line: *start_line,
            end_line,
            depth: open.len(),
        });
        open.push(*indent);
    }
    out
}

/// Declaration containing `line`, or `None` for code inside none of them.
///
/// Declarations are ordered and non-overlapping, so the only candidate is the
/// last one starting at or before the line. A linear scan here would be
/// O(matches x declarations).
///
/// Spans end at their bodies rather than running to the next declaration, so
/// they do not tile the file: a line between two of them -- a class-level
/// constant below the last method -- belongs to neither, and is reported as a
/// window instead of attributed to a body that does not hold it.
pub fn enclosing(decls: &[Declaration], line: u64) -> Option<&Declaration> {
    let idx = decls.partition_point(|d| d.start_line <= line);
    let found = decls.get(idx.checked_sub(1)?)?;
    (line <= found.end_line).then_some(found)
}

#[cfg(test)]
mod tests;
