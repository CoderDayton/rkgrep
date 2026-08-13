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

pub use body::DeclBuilder;
pub use mask::{Masker, Mode};
pub use scan::{declaration_name, kind_declares};
pub use words::identifier_tokens;

#[cfg(test)]
pub use mask::{comment_source, lines_with_endings, mask_source};

/// Above this, ripgrep is welcome to the file but we will not build a
/// declaration table for it.
pub const MAX_SOURCE_BYTES: usize = 8 * 1024 * 1024;

/// Files passed over for size, counted for the whole run.
///
/// One process serves one query, so a counter for the process is a counter for
/// the query. Threading one through the walk and the extractor separately
/// would buy nothing a caller could tell apart.
static OVERSIZED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// How many files this run passed over for being larger than
/// [`MAX_SOURCE_BYTES`]. A search that quietly returns less than it found is
/// worse than one that says so.
pub fn oversized_files() -> usize {
    OVERSIZED.load(std::sync::atomic::Ordering::Relaxed)
}

/// A reader over `path`, or `None` when it is larger than
/// [`MAX_SOURCE_BYTES`].
///
/// The size is settled from the directory entry, before a byte is read. What a
/// caller then does line by line it never has to hold whole, which is the
/// point: one worker per core reads at once, and the largest file in a batch
/// would otherwise set the memory a query costs.
pub fn open_source(path: &std::path::Path) -> Option<std::io::BufReader<std::fs::File>> {
    if std::fs::metadata(path).ok()?.len() > MAX_SOURCE_BYTES as u64 {
        OVERSIZED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        return None;
    }
    Some(std::io::BufReader::new(std::fs::File::open(path).ok()?))
}

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
///
/// A query streams a file through [`DeclBuilder`] instead. This is the same
/// table stated over a whole string, which is what a test can be written
/// against.
#[cfg(test)]
pub fn declarations(content: &str) -> Vec<Declaration> {
    if content.len() > MAX_SOURCE_BYTES {
        return Vec::new();
    }
    let mut masker = Masker::default();
    let mut builder = DeclBuilder::default();
    let mut masked = String::new();
    for (number, (line, _)) in lines_with_endings(content).enumerate() {
        masker.scan_line(line);
        masker.render(line, Mode::Code, &mut masked);
        builder.line(number as u64 + 1, &masked);
    }
    builder.finish()
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
