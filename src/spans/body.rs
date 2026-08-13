//! Where a declaration's body ends, read from indentation alone.
//!
//! Indentation rather than a parser, because the whole extractor is
//! language-agnostic; a language that does not indent its bodies simply
//! reports every declaration at depth 0 and ends each one at its own line.
//!
//! A body ends at a line the file has not reached yet, so [`DeclBuilder`]
//! holds a declaration until that line arrives. What it holds is one entry per
//! declaration and one per level of nesting, never one per line of the file.

use super::scan::scan_declaration;
use super::Declaration;

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

/// The declaration table of one file, built a line at a time.
#[derive(Default)]
pub struct DeclBuilder {
    /// In file order. An entry's `end_line` is its own line until the line
    /// that closes its body settles it.
    decls: Vec<Declaration>,
    /// Declarations whose body is still open, with the indentation that will
    /// close each, deepest last.
    pending: Vec<(usize, usize)>,
    /// Indentation of the declarations enclosing the next one, which is what
    /// its depth counts.
    open: Vec<usize>,
    /// The last line holding text, so a body ends on the last line that
    /// belonged to it rather than on the blank lines trailing it.
    prev_filled: Option<u64>,
}

impl DeclBuilder {
    /// Feed the next masked line of the file; `number` is 1-based.
    pub fn line(&mut self, number: u64, masked: &str) {
        // `str::lines` drops the carriage return of a CRLF file, and every
        // rule below is written against lines that have already lost it.
        let masked = masked.strip_suffix('\r').unwrap_or(masked);
        let Some(indent) = indent_of(masked) else {
            return;
        };
        // This line closes the body of every declaration it is not nested
        // inside: read forwards, it is the first line at or above their
        // indentation, which is where a body stops.
        let closes = closes_a_body(masked);
        while let Some(&(at, deeper)) = self.pending.last() {
            if deeper < indent {
                break;
            }
            self.pending.pop();
            self.settle(at, closes.then_some(number));
        }

        if let Some((kind, name)) = scan_declaration(masked) {
            while self.open.last().is_some_and(|top| *top >= indent) {
                self.open.pop();
            }
            self.decls.push(Declaration {
                name: name.to_string(),
                kind: kind.to_string(),
                start_line: number,
                end_line: number,
                depth: self.open.len(),
            });
            self.open.push(indent);
            self.pending.push((self.decls.len() - 1, indent));
        }

        self.prev_filled = Some(number);
    }

    /// Every declaration of the file, in file order.
    pub fn finish(mut self) -> Vec<Declaration> {
        while let Some((at, _)) = self.pending.pop() {
            self.settle(at, None);
        }

        // A declaration whose body is made of other declarations ends before
        // the first of them: the methods of a class are spans in their own
        // right, so returning the class whole returns them a second time and
        // spends a whole budget on one file. Declarations are in file order,
        // so that first nested one is simply the next entry, and a sibling the
        // body already stops above.
        for at in 0..self.decls.len() {
            let Some(next) = self.decls.get(at + 1).map(|d| d.start_line) else {
                break;
            };
            let decl = &mut self.decls[at];
            decl.end_line = decl
                .end_line
                .min(next.saturating_sub(1))
                .max(decl.start_line);
        }
        self.decls
    }

    /// Record where the body of `at` ends. `closer` is the line that closed
    /// it, when that line holds nothing but the closing delimiter.
    fn settle(&mut self, at: usize, closer: Option<u64>) {
        let start = self.decls[at].start_line;
        let end = match closer {
            Some(line) => line,
            // The body stops above the line that closed it, so it ends on the
            // last line that held text -- never on the blank ones between.
            None => self.prev_filled.unwrap_or(start),
        };
        self.decls[at].end_line = end.max(start);
    }
}
