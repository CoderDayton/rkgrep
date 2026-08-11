//! The four shapes a declaration is recognized by, read from one line.
//!
//! A keyword opening the line, a Go receiver between keyword and name, a bare
//! signature with a parameter list and a brace, and a callable bound to a
//! name. [`scan_declaration`] runs them and settles what to do when two
//! disagree.
//!
//! Hand-written scans rather than regexes: this runs on every matched line in
//! the tree during the walk, and reading the two or three words that decide
//! the question costs a fraction of what an alternation over the tables does.

use super::words::{
    all_bytes_in, is_control, is_keyword, is_modifier, is_word_byte, ASSIGNMENT_PREFIX,
    COMPOUND_OPERATOR, SIGNATURE_PREFIX, SIGNATURE_TAIL,
};

/// Leading words examined on one line. A declaration is a short run of
/// qualifiers, a keyword, and a name; nothing needs more than a handful, and a
/// fixed bound keeps the scan off the heap.
const MAX_LEADING_WORDS: usize = 8;

fn skip_blanks(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
    }
    i
}

fn word_end(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && is_word_byte(bytes[i]) {
        i += 1;
    }
    i
}

fn opens_identifier(word: &str) -> bool {
    word.as_bytes()
        .first()
        .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
}

/// Index just past the `)` closing the group that opens at `open`, or `None`
/// if it does not close on this line. An argument list that runs off the end
/// of the line is the signature of nothing -- `callback(err, function () {`
/// opens two groups and closes one.
fn close_paren(bytes: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (offset, &byte) in bytes.iter().enumerate().skip(open) {
        match byte {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(offset + 1);
                }
            }
            _ => {}
        }
    }
    None
}

/// Index just past the `>` closing the parameter list that opens at `open`,
/// or `None` if it does not close on this line.
///
/// `->` and `=>` both end in `>` and neither closes anything, so a `>` that
/// follows one is stepped over rather than counted: without that, the bound in
/// `impl<F: Fn() -> u32> Runner<F>` ends the list at the arrow.
fn close_angle(bytes: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (offset, &byte) in bytes.iter().enumerate().skip(open) {
        match byte {
            b'<' => depth += 1,
            b'>' if matches!(
                offset.checked_sub(1).and_then(|prev| bytes.get(prev)),
                Some(b'-' | b'=')
            ) => {}
            b'>' => {
                depth -= 1;
                if depth == 0 {
                    return Some(offset + 1);
                }
            }
            _ => {}
        }
    }
    None
}

/// Whether every word in `text` is safe to see in front of a declared name.
fn words_are_declarative(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if !is_word_byte(bytes[i]) {
            i += 1;
            continue;
        }
        let start = i;
        i = word_end(bytes, i);
        if is_control(&text[start..i]) {
            return false;
        }
    }
    true
}

/// A declaration keyword opening the line, after zero or more qualifiers.
///
/// Qualifiers are consumed greedily and then given back one at a time, because
/// `const` and `static` are both qualifier and keyword: `const static x`
/// declares `x`, not `static`.
fn scan_keyword_declaration(line: &str) -> Option<(&str, &str)> {
    let bytes = line.as_bytes();
    let mut words: [&str; MAX_LEADING_WORDS] = [""; MAX_LEADING_WORDS];
    let mut count = 0usize;

    let mut i = skip_blanks(bytes, 0);
    while count < MAX_LEADING_WORDS {
        let start = i;
        i = word_end(bytes, i);
        if i == start {
            break;
        }
        let word = &line[start..i];
        words[count] = word;
        count += 1;
        // A qualifier may carry a scope: Rust writes `pub(crate) fn`, and
        // without skipping the parenthesized part the run ends on the `(` and
        // the declaration is missed entirely.
        if bytes.get(i) == Some(&b'(') && is_modifier(word) {
            match close_paren(bytes, i) {
                Some(end) => i = end,
                None => break,
            }
        }
        // A keyword may carry generic parameters the same way: Rust writes
        // `impl<T> Repo<T>`, and the run would otherwise end on the `<`. Only
        // a keyword, so `if a<b && c>(d) {` is still a comparison.
        else if bytes.get(i) == Some(&b'<') && is_keyword(word) {
            match close_angle(bytes, i) {
                Some(end) => i = end,
                None => break,
            }
        }
        let after = i;
        i = skip_blanks(bytes, i);
        if i == after {
            break;
        }
    }
    if count < 2 {
        return None;
    }

    let leading_modifiers = words[..count].iter().take_while(|w| is_modifier(w)).count();
    for split in (0..=leading_modifiers.min(count - 2)).rev() {
        let name = words[split + 1];
        if is_keyword(words[split]) && opens_identifier(name) {
            return Some((words[split], name));
        }
    }
    None
}

/// A Go method: `func (r *Repo) Save(ctx context.Context) error`.
fn scan_receiver_declaration(line: &str) -> Option<(&str, &str)> {
    let bytes = line.as_bytes();
    let mut i = skip_blanks(bytes, 0);
    let start = i;
    i = word_end(bytes, i);
    let keyword = &line[start..i];
    if keyword != "func" && keyword != "fn" {
        return None;
    }
    i = skip_blanks(bytes, i);
    if bytes.get(i) != Some(&b'(') {
        return None;
    }
    i = skip_blanks(bytes, close_paren(bytes, i)?);
    let name_start = i;
    let name_end = word_end(bytes, i);
    let name = line.get(name_start..name_end)?;
    opens_identifier(name).then_some((keyword, name))
}

/// A function written as a signature with no declaring keyword at all:
/// `int main(void) {`, `public int compute(int x) {`, `void Repo::save() {`,
/// `async loadUser(id) {`, `deploy() {`.
fn scan_signature_declaration(line: &str) -> Option<(&str, &str)> {
    if !line.trim_end().ends_with('{') {
        return None;
    }
    let bytes = line.as_bytes();
    let open = memchr::memchr(b'(', bytes)?;

    let mut cursor = open;
    while cursor > 0 && (bytes[cursor - 1] == b' ' || bytes[cursor - 1] == b'\t') {
        cursor -= 1;
    }
    let name_end = cursor;
    while cursor > 0 && is_word_byte(bytes[cursor - 1]) {
        cursor -= 1;
    }
    let name = &line[cursor..name_end];
    if !opens_identifier(name) || is_control(name) || is_keyword(name) {
        return None;
    }

    let prefix = &line[..cursor];
    if !all_bytes_in(prefix, &SIGNATURE_PREFIX) || !words_are_declarative(prefix) {
        return None;
    }

    let after = close_paren(bytes, open)?;
    let tail = line[after..].trim_end().trim_end_matches('{');
    if !all_bytes_in(tail, &SIGNATURE_TAIL) || !words_are_declarative(tail) {
        return None;
    }
    Some(("function", name))
}

/// A callable bound to a name: `handler = lambda req: ...`,
/// `let load = function (id) {`, `Api.prototype.load = function () {`,
/// `handler := func(w http.ResponseWriter) {`.
fn scan_assignment_declaration(line: &str) -> Option<(&str, &str)> {
    let bytes = line.as_bytes();
    let eq = memchr::memchr_iter(b'=', bytes).find(|&i| {
        bytes.get(i + 1) != Some(&b'=')
            // `:=` declares in Go; every other compound operator reassigns.
            && !(i > 0 && COMPOUND_OPERATOR[bytes[i - 1] as usize])
    })?;

    let rhs = line[eq + 1..].trim_start();
    let rhs_word_end = word_end(rhs.as_bytes(), 0);
    let named_callable = matches!(
        &rhs[..rhs_word_end],
        "function" | "func" | "fn" | "lambda" | "async" | "def"
    );
    let arrow = rhs.contains("=>") && (rhs.starts_with('(') || rhs_word_end > 0);
    if !named_callable && !arrow {
        return None;
    }

    let mut cursor = eq;
    while cursor > 0 && (bytes[cursor - 1] == b' ' || bytes[cursor - 1] == b'\t') {
        cursor -= 1;
    }
    let name_end = cursor;
    while cursor > 0 && is_word_byte(bytes[cursor - 1]) {
        cursor -= 1;
    }
    let name = &line[cursor..name_end];
    if !opens_identifier(name) || is_control(name) {
        return None;
    }

    let prefix = &line[..cursor];
    if !all_bytes_in(prefix, &ASSIGNMENT_PREFIX) || !words_are_declarative(prefix) {
        return None;
    }
    Some(("function", name))
}

/// Byte offset of `part` within `line`, which it must be a subslice of.
fn offset_in(line: &str, part: &str) -> usize {
    part.as_ptr() as usize - line.as_ptr() as usize
}

/// Recognize a declaration opening `line`, returning its kind and name.
pub fn scan_declaration(line: &str) -> Option<(&str, &str)> {
    let keyword = scan_keyword_declaration(line);
    if keyword.is_none() {
        if let Some(found) = scan_receiver_declaration(line) {
            return Some(found);
        }
    }
    if let Some((kind, name)) = scan_signature_declaration(line) {
        return match keyword {
            Some((_, keyword_name)) if keyword_name == name => keyword,
            Some((_, keyword_name)) if offset_in(line, name) < offset_in(line, keyword_name) => {
                keyword
            }
            _ => Some((kind, name)),
        };
    }
    keyword.or_else(|| scan_assignment_declaration(line))
}

/// Whether a declaration of this kind declares the name it carries.
pub fn kind_declares(kind: &str) -> bool {
    kind != "impl"
}

/// The name a line declares, read from the line alone.
pub fn declaration_name(line: &str) -> Option<&str> {
    scan_declaration(line)
        .filter(|(kind, _)| kind_declares(kind))
        .map(|(_, name)| name)
}
