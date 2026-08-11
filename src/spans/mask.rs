//! Blanking out what is not code, without moving anything.
//!
//! Masked regions are overwritten with spaces rather than removed, so byte
//! offsets and newline positions survive: a line number taken from masked text
//! is valid against the original file. Every rule downstream depends on that.

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
        // A bare `#`, or one before punctuation or whitespace.
        _ => true,
    }
}

/// What a region of a source file that is not code holds.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Masked {
    Comment,
    Literal,
}

/// Blank `out[from..to]`, keeping newlines so line numbers survive.
fn blank(out: &mut [u8], from: usize, to: usize) {
    for byte in &mut out[from..to] {
        if *byte != b'\n' {
            *byte = b' ';
        }
    }
}

/// Every comment and string literal in `src`, as byte ranges in file order.
///
/// A scanner rather than a regex: correctly pairing triple-quoted strings
/// needs lookahead the `regex` crate does not offer, and mispairing them masks
/// away the code that follows a docstring instead of the docstring itself.
///
/// Quotes and comment markers are ASCII, and a UTF-8 continuation byte is
/// never equal to one, so scanning bytes cannot begin a literal mid-character.
/// Every range therefore starts and ends on a character boundary, and blanking
/// one byte-for-byte leaves valid UTF-8.
fn masked_regions(src: &[u8]) -> Vec<(usize, usize, Masked)> {
    let mut regions = Vec::new();
    let n = src.len();
    let mut i = 0usize;

    while i < n {
        match src[i] {
            q @ (b'"' | b'\'') => {
                let triple = i + 2 < n && src[i + 1] == q && src[i + 2] == q;
                let start = i;
                if triple {
                    i += 3;
                    while i < n {
                        if src[i] == b'\\' {
                            i += 2;
                            continue;
                        }
                        if src[i] == q && i + 2 < n && src[i + 1] == q && src[i + 2] == q {
                            i += 3;
                            break;
                        }
                        i += 1;
                    }
                } else {
                    i += 1;
                    while i < n {
                        match src[i] {
                            b'\\' => i += 2,
                            // An unterminated quote (an apostrophe in prose)
                            // must not swallow the rest of the file.
                            b'\n' => break,
                            b if b == q => {
                                i += 1;
                                break;
                            }
                            _ => i += 1,
                        }
                    }
                }
                regions.push((start, i.min(n), Masked::Literal));
            }
            b'/' if i + 1 < n && src[i + 1] == b'*' => {
                let start = i;
                i += 2;
                while i + 1 < n && !(src[i] == b'*' && src[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(n);
                regions.push((start, i, Masked::Comment));
            }
            b'/' if i + 1 < n && src[i + 1] == b'/' => {
                let start = i;
                while i < n && src[i] != b'\n' {
                    i += 1;
                }
                regions.push((start, i, Masked::Comment));
            }
            b'#' if hash_opens_comment(src, i) => {
                let start = i;
                while i < n && src[i] != b'\n' {
                    i += 1;
                }
                regions.push((start, i, Masked::Comment));
            }
            _ => i += 1,
        }
    }

    regions
}

/// `content` with every comment and string literal blanked out.
///
/// Masked regions are overwritten with spaces rather than removed, so byte
/// offsets and newline positions survive and line numbers taken from the
/// result are valid against the original.
pub fn mask_source(content: &str) -> String {
    let mut out = content.as_bytes().to_vec();
    for (start, end, _) in masked_regions(content.as_bytes()) {
        blank(&mut out, start, end);
    }
    String::from_utf8(out).unwrap_or_else(|_| content.to_string())
}

/// `content` with everything but its comments blanked out.
///
/// The inverse of [`mask_source`], preserving offsets the same way, so a
/// pattern searched against the result can only match inside a comment while
/// still reporting the line numbers it has in the original file.
pub fn comment_source(content: &str) -> String {
    let src = content.as_bytes();
    let mut out = src.to_vec();
    let mut code_from = 0usize;
    for (start, end, kind) in masked_regions(src) {
        blank(&mut out, code_from, start);
        if kind != Masked::Comment {
            blank(&mut out, start, end);
        }
        code_from = end;
    }
    blank(&mut out, code_from, src.len());
    String::from_utf8(out).unwrap_or_else(|_| content.to_string())
}
