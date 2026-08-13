//! Blanking out what is not code, without moving anything.
//!
//! Masked regions are overwritten with spaces rather than removed, so byte
//! offsets and newline positions survive: a line number taken from masked text
//! is valid against the original file. Every rule downstream depends on that.
//!
//! Masking runs one line at a time, carrying what each line leaves open, so a
//! file is masked as it is read rather than copied whole and then rewritten.

use super::words::{is_preprocessor_directive, is_word_byte};

/// Digit counts a CSS hex color is written with: `#fff`, `#ffff`, `#ffffff`,
/// `#ffffffff`.
const HEX_COLOR_LENGTHS: &[usize] = &[3, 4, 6, 8];

/// The leading run of word bytes in `rest`, which is empty when it opens with
/// anything else.
fn word_prefix(rest: &[u8]) -> &[u8] {
    let end = rest
        .iter()
        .position(|&b| !is_word_byte(b))
        .unwrap_or(rest.len());
    &rest[..end]
}

/// Whether the `#` at `at` opens a line comment.
///
/// The same byte opens a Rust attribute, a C preprocessor directive and a CSS
/// color, and reading those as comments hides the code on their line. What
/// follows the `#` is the only thing that separates them, so a shebang counts
/// as a comment while `#![allow(..)]` does not.
fn hash_opens_comment(src: &[u8], at: usize) -> bool {
    let rest = &src[at + 1..];
    match rest.first() {
        Some(b'[') => false,
        Some(b'!') => rest.get(1) != Some(&b'['),
        Some(&byte) if is_word_byte(byte) => {
            let word = word_prefix(rest);
            let directive = std::str::from_utf8(word).is_ok_and(is_preprocessor_directive);
            let color =
                HEX_COLOR_LENGTHS.contains(&word.len()) && word.iter().all(u8::is_ascii_hexdigit);
            !directive && !color
        }
        _ => true,
    }
}

/// What a region of a source file that is not code holds.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Masked {
    Comment,
    Literal,
}

/// Which half of a source file a masked line keeps.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Everything but comments and string literals.
    Code,
    /// Comments alone.
    Comments,
}

/// What a line leaves open for the next one to close.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Carry {
    Code,
    Block,
    Str { quote: u8, triple: bool },
}

/// Where a block comment already open at `from` ends, and what it leaves open.
fn end_of_block(src: &[u8], from: usize) -> (usize, Carry) {
    let n = src.len();
    let mut i = from;
    while i + 1 < n && !(src[i] == b'*' && src[i + 1] == b'/') {
        i += 1;
    }
    match i + 1 < n {
        true => ((i + 2).min(n), Carry::Code),
        false => (n, Carry::Block),
    }
}

/// Where a literal already open at `from` ends, and what it leaves open.
fn end_of_str(src: &[u8], from: usize, quote: u8, triple: bool) -> (usize, Carry) {
    let n = src.len();
    let mut i = from;
    if triple {
        while i < n {
            if src[i] == b'\\' {
                i += 2;
                continue;
            }
            if src[i] == quote && i + 2 < n && src[i + 1] == quote && src[i + 2] == quote {
                return (i + 3, Carry::Code);
            }
            i += 1;
        }
        return (n, Carry::Str { quote, triple });
    }

    while i < n {
        match src[i] {
            // A backslash on the end of the line escaped the newline itself,
            // and the literal runs on into the line after it.
            b'\\' if i + 1 == n => return (n, Carry::Str { quote, triple }),
            b'\\' => i += 2,
            b if b == quote => return (i + 1, Carry::Code),
            _ => i += 1,
        }
    }
    // An unterminated quote -- an apostrophe in prose -- must not swallow the
    // rest of the file, so the end of the line closes it.
    (n, Carry::Code)
}

/// Masks a file one line at a time.
///
/// A comment or a literal is free to span lines, so what a line leaves open is
/// carried into the next: the masker is the state that makes a line-by-line
/// read equivalent to a scan of the whole file.
pub struct Masker {
    carry: Carry,
    /// Reused across lines, so masking a file allocates once rather than once
    /// per line.
    regions: Vec<(usize, usize, Masked)>,
}

impl Default for Masker {
    fn default() -> Self {
        Self {
            carry: Carry::Code,
            regions: Vec::new(),
        }
    }
}

/// Copy `chunk` through, or replace it with as many spaces as it has bytes.
fn push(text: &mut String, chunk: &str, keep: bool) {
    match keep {
        true => text.push_str(chunk),
        false => {
            for _ in 0..chunk.len() {
                text.push(' ');
            }
        }
    }
}

impl Masker {
    /// Advance over one line, recording the comments and literals it holds.
    ///
    /// `line` carries no newline: a line comment ends where it ends, and a
    /// rendering of it has exactly the bytes of the line it came from.
    pub fn scan_line(&mut self, line: &str) {
        self.scan(line.as_bytes());
    }

    /// Write the line just scanned into `out`, with everything `mode` does not
    /// keep replaced by spaces. One scan renders as many modes as are asked
    /// for, so a file matched against its comments is still read once.
    pub fn render(&self, line: &str, mode: Mode, out: &mut String) {
        let keep_code = mode == Mode::Code;
        out.clear();
        let mut at = 0usize;
        for &(start, end, kind) in &self.regions {
            push(out, &line[at..start], keep_code);
            push(
                out,
                &line[start..end],
                !keep_code && kind == Masked::Comment,
            );
            at = end;
        }
        push(out, &line[at..], keep_code);
    }

    /// Every comment and literal on one line, as byte ranges in line order.
    fn scan(&mut self, src: &[u8]) {
        self.regions.clear();
        let n = src.len();
        let mut i = 0usize;

        // Whatever the line before left open runs from the first byte of this
        // one, and is closed before anything new is looked for.
        match self.carry {
            Carry::Code => {}
            Carry::Block => {
                let (end, carry) = end_of_block(src, 0);
                self.carry = carry;
                self.regions.push((0, end, Masked::Comment));
                i = end;
            }
            Carry::Str { quote, triple } => {
                let (end, carry) = end_of_str(src, 0, quote, triple);
                self.carry = carry;
                self.regions.push((0, end, Masked::Literal));
                i = end;
            }
        }

        while i < n {
            match src[i] {
                q @ (b'"' | b'\'') => {
                    let triple = i + 2 < n && src[i + 1] == q && src[i + 2] == q;
                    let from = match triple {
                        true => i + 3,
                        false => i + 1,
                    };
                    let (end, carry) = end_of_str(src, from, q, triple);
                    self.carry = carry;
                    self.regions.push((i, end.min(n), Masked::Literal));
                    i = end;
                }
                b'/' if i + 1 < n && src[i + 1] == b'*' => {
                    let (end, carry) = end_of_block(src, i + 2);
                    self.carry = carry;
                    self.regions.push((i, end, Masked::Comment));
                    i = end;
                }
                b'/' if i + 1 < n && src[i + 1] == b'/' => {
                    self.regions.push((i, n, Masked::Comment));
                    i = n;
                }
                b'#' if hash_opens_comment(src, i) => {
                    self.regions.push((i, n, Masked::Comment));
                    i = n;
                }
                _ => i += 1,
            }
        }
    }
}

/// Split `content` into lines with the newline that ended each one, if any.
#[cfg(test)]
pub fn lines_with_endings(content: &str) -> impl Iterator<Item = (&str, &str)> {
    content
        .split_inclusive('\n')
        .map(|chunk| match chunk.strip_suffix('\n') {
            Some(line) => (line, "\n"),
            None => (chunk, ""),
        })
}

/// `content` masked line by line, keeping the half `mode` names.
///
/// A query masks as it reads and holds no copy of the file. This is the same
/// masking stated over a whole string, which is what a test can be written
/// against.
#[cfg(test)]
fn mask_all(content: &str, mode: Mode) -> String {
    let mut masker = Masker::default();
    let mut out = String::with_capacity(content.len());
    let mut masked = String::new();
    for (line, ending) in lines_with_endings(content) {
        masker.scan_line(line);
        masker.render(line, mode, &mut masked);
        out.push_str(&masked);
        out.push_str(ending);
    }
    out
}

/// `content` with every comment and string literal blanked out.
#[cfg(test)]
pub fn mask_source(content: &str) -> String {
    mask_all(content, Mode::Code)
}

/// `content` with everything but its comments blanked out.
#[cfg(test)]
pub fn comment_source(content: &str) -> String {
    mask_all(content, Mode::Comments)
}
