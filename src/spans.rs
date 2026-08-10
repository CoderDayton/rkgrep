//! Declaration extraction with offset-preserving comment/string masking.
//!
//! The unit rkgrep returns is a declaration, not a fixed window of N lines: a
//! function is something a reader understands, `±10 lines` is an arbitrary cut
//! through one. Extraction is heuristic and language-agnostic — one keyword set
//! driving one regex — so it works on anything ripgrep can match without
//! needing a parser per language.
//!
//! Masking runs before matching so a declaration written inside a comment or a
//! string literal is never reported. Masked regions are overwritten with
//! spaces rather than removed, preserving byte offsets and newline positions,
//! so line numbers taken from the masked text are valid against the original.

/// Above this, ripgrep is welcome to the file but we will not build a
/// declaration table for it.
pub const MAX_SOURCE_BYTES: usize = 8 * 1024 * 1024;

/// Declaration keywords, sorted so lookup is a binary search. `sorted_tables`
/// keeps this promise honest.
const KEYWORDS: &[&str] = &[
    "class",
    "const",
    "def",
    "enum",
    "fn",
    "func",
    "function",
    "impl",
    "interface",
    "macro",
    "mod",
    "module",
    "namespace",
    "record",
    "static",
    "struct",
    "trait",
    "type",
    "var",
];

/// Qualifiers that may precede a declaration keyword. The keyword must open
/// its line after zero or more of these; otherwise `from enum import Enum`
/// reads as declaring a symbol named `import`, and that one bogus declaration
/// swallows every real one after it. Sorted, as [`KEYWORDS`] is.
const MODIFIERS: &[&str] = &[
    "abstract",
    "async",
    "const",
    "declare",
    "default",
    "export",
    "extern",
    "final",
    "inline",
    "internal",
    "local",
    "open",
    "override",
    "partial",
    "private",
    "protected",
    "pub",
    "public",
    "readonly",
    "sealed",
    "static",
    "unsafe",
    "virtual",
];

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
    ///
    /// What separates `def save` in a module from `save` as a method of some
    /// unrelated class -- both declare the name, and only one of them is what
    /// "where is save defined" is asking for. Indentation rather than a parser
    /// because the whole extractor is language-agnostic; a language that does
    /// not indent its bodies simply reports everything at depth 0.
    pub depth: usize,
}

/// Leading words examined on one line. A declaration is a short run of
/// qualifiers, a keyword, and a name; nothing needs more than a handful, and a
/// fixed bound keeps the scan off the heap.
const MAX_LEADING_WORDS: usize = 8;

/// Where the words starting with each byte sit in a sorted table.
///
/// Searching a whole table compares several strings to answer what the first
/// byte usually settles. The index is built from the table, so the sorting
/// those tables already require is what makes it correct.
const fn first_byte_index(words: &[&str]) -> [(u8, u8); BYTE_VALUES] {
    let mut index = [(0u8, 0u8); BYTE_VALUES];
    let mut at = 0usize;
    while at < words.len() {
        let byte = words[at].as_bytes()[0] as usize;
        if index[byte].1 == 0 {
            index[byte].0 = at as u8;
        }
        index[byte].1 += 1;
        at += 1;
    }
    index
}

/// Whether `words` holds `word`, searching only the run that could.
#[inline]
fn table_holds(words: &[&str], index: &[(u8, u8); BYTE_VALUES], word: &str) -> bool {
    let Some(&first) = word.as_bytes().first() else {
        return false;
    };
    let (start, len) = index[first as usize];
    let (start, len) = (start as usize, len as usize);
    words[start..start + len].binary_search(&word).is_ok()
}

const KEYWORD_INDEX: [(u8, u8); BYTE_VALUES] = first_byte_index(KEYWORDS);
const MODIFIER_INDEX: [(u8, u8); BYTE_VALUES] = first_byte_index(MODIFIERS);

fn is_keyword(word: &str) -> bool {
    table_holds(KEYWORDS, &KEYWORD_INDEX, word)
}

fn is_modifier(word: &str) -> bool {
    table_holds(MODIFIERS, &MODIFIER_INDEX, word)
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Every value a byte can take, which is the width of the tables below.
const BYTE_VALUES: usize = 256;

/// A byte set as a table, for the shape tests below.
///
/// Those ask "is this byte one of about a dozen" for every byte of a line, and
/// `[u8]::contains` answers it by walking the dozen. One indexed load answers
/// it in the same time whatever the set holds.
const fn byte_set(extra: &[u8]) -> [bool; BYTE_VALUES] {
    let mut table = [false; BYTE_VALUES];
    let mut byte = 0usize;
    while byte < BYTE_VALUES {
        table[byte] = (byte as u8).is_ascii_alphanumeric() || byte == b'_' as usize;
        byte += 1;
    }
    let mut at = 0usize;
    while at < extra.len() {
        table[extra[at] as usize] = true;
        at += 1;
    }
    table
}

/// What may sit between the start of a line and a declared name: return types,
/// qualifiers, namespaces, pointers.
const SIGNATURE_PREFIX: [bool; BYTE_VALUES] = byte_set(b" \t*&:<>,[]~");

/// What may sit between a parameter list and the brace: return types, `const`,
/// `noexcept`, `throws E`.
const SIGNATURE_TAIL: [bool; BYTE_VALUES] = byte_set(b" \t*&:<>,[]?-.");

/// What may sit in front of an assigned name: `Api.prototype.load` is a name.
const ASSIGNMENT_PREFIX: [bool; BYTE_VALUES] = byte_set(b" \t.:[]*&@$");

/// Operators that make an `=` a compound assignment rather than a binding.
const COMPOUND_OPERATOR: [bool; BYTE_VALUES] = {
    let mut table = [false; BYTE_VALUES];
    let operators = b"=!<>+-*/%&|^";
    let mut at = 0usize;
    while at < operators.len() {
        table[operators[at] as usize] = true;
        at += 1;
    }
    table
};

/// Whether every byte of `text` is in `set`.
#[inline]
fn all_bytes_in(text: &str, set: &[bool; BYTE_VALUES]) -> bool {
    text.bytes().all(|byte| set[byte as usize])
}

/// Words that open a control-flow construct. Whatever follows one of these is
/// a call or a test, never a definition: `if (ready(x)) {` declares nothing,
/// and without this the signature rule below would name it `ready`.
const CONTROL_WORDS: &[&str] = &[
    "and", "assert", "await", "case", "catch", "defer", "delete", "do", "elif", "else", "except",
    "finally", "for", "go", "if", "in", "is", "lock", "match", "new", "not", "or", "raise",
    "return", "select", "sizeof", "switch", "throw", "try", "typeof", "unless", "until", "using",
    "when", "while", "with", "yield",
];

const CONTROL_INDEX: [(u8, u8); BYTE_VALUES] = first_byte_index(CONTROL_WORDS);

fn is_control(word: &str) -> bool {
    table_holds(CONTROL_WORDS, &CONTROL_INDEX, word)
}

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
/// A hand-written scan rather than a regex. This runs on every matched line in
/// the tree during the walk, and a 23-way alternation of qualifiers in front of
/// a 17-way alternation of keywords costs several times what reading the two or
/// three words that decide the question does: replacing it took 12-23% off a
/// single-threaded search of a 45k-file tree.
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
        // Words must be separated by blanks; anything else ends the run.
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
///
/// The receiver sits between the keyword and the name, so the keyword rule --
/// which wants the name next -- sees nothing at all.
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
///
/// This is most of C, C++, Java, C#, shell, and every method body in a
/// JavaScript class. The shape is a name, a parameter list that closes, and a
/// brace opening the body on the same line -- the brace is what separates a
/// definition from the call `compute(x);` one line below it.
fn scan_signature_declaration(line: &str) -> Option<(&str, &str)> {
    if !line.trim_end().ends_with('{') {
        return None;
    }
    let bytes = line.as_bytes();
    let open = memchr::memchr(b'(', bytes)?;

    // The name is the identifier the parameter list hangs off.
    let mut cursor = open;
    while cursor > 0 && (bytes[cursor - 1] == b' ' || bytes[cursor - 1] == b'\t') {
        cursor -= 1;
    }
    let name_end = cursor;
    while cursor > 0 && is_word_byte(bytes[cursor - 1]) {
        cursor -= 1;
    }
    let name = &line[cursor..name_end];
    // A keyword here means the parameter list belongs to something else --
    // `func (r *Repo) Save(...)` hangs its first list off `func`.
    if !opens_identifier(name) || is_control(name) || is_keyword(name) {
        return None;
    }

    // In front of the name: return types, qualifiers, namespaces, pointers.
    // Anything else -- a dot, an equals sign, a brace -- means this line is
    // calling the name rather than defining it.
    let prefix = &line[..cursor];
    if !all_bytes_in(prefix, &SIGNATURE_PREFIX) || !words_are_declarative(prefix) {
        return None;
    }

    // Between the parameter list and the brace: return types, `const`,
    // `noexcept`, `throws E`. A parenthesis here is a member initializer list
    // or a call, neither of which this rule can read.
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
///
/// The name is on the left of the assignment and the thing being named is on
/// the right, which is the reverse of every other rule here.
fn scan_assignment_declaration(line: &str) -> Option<(&str, &str)> {
    let bytes = line.as_bytes();
    // `memchr` over the line rather than a byte loop: an assignment's `=` is
    // usually well into the line, and every byte before it is a comparison
    // this skips.
    let eq = memchr::memchr_iter(b'=', bytes).find(|&i| {
        bytes.get(i + 1) != Some(&b'=')
            // `:=` declares in Go; every other compound operator reassigns.
            && !(i > 0 && COMPOUND_OPERATOR[bytes[i - 1] as usize])
    })?;

    // What is being assigned decides this before the name does, and reading
    // one word answers it: an assignment of anything but a function is most of
    // the lines that reach here.
    let rhs = line[eq + 1..].trim_start();
    let rhs_word_end = word_end(rhs.as_bytes(), 0);
    let named_callable = matches!(
        &rhs[..rhs_word_end],
        "function" | "func" | "fn" | "lambda" | "async" | "def"
    );
    // `(a, b) => ...` and `a => ...`; the arrow is what makes it a function
    // rather than the tuple or the variable it would otherwise be.
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

    // `Api.prototype.load` is a name; `if (ready` is not.
    let prefix = &line[..cursor];
    if !all_bytes_in(prefix, &ASSIGNMENT_PREFIX) || !words_are_declarative(prefix) {
        return None;
    }
    Some(("function", name))
}

/// Byte offset of `part` within `line`, which it must be a subslice of.
///
/// The scanners return borrows of the line they read, so their positions are
/// already known; recovering one by searching for the text would find the
/// wrong occurrence whenever a word repeats on the line.
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
            // The keyword rule reads `static int compute_hash(...)` as
            // declaring `int`: it cannot tell a return type from a name. The
            // signature the line actually opens can, so it wins -- but only
            // when the two disagree, since `class Repo(db: Db) {` is better
            // described by its keyword than as a function.
            Some((_, keyword_name)) if keyword_name == name => keyword,
            // The name a line declares is the last one it offers. A
            // signature found in front of the keyword's own name is part of
            // what the keyword rule already consumed -- the qualifier in
            // `pub(crate) trait Kill {`, the bound in `impl<F: Fn() -> u32>
            // Runner<F> {` -- while `static void open(int fd) {` and
            // `static int compute_hash(...) {` both name theirs behind it.
            Some((_, keyword_name)) if offset_in(line, name) < offset_in(line, keyword_name) => {
                keyword
            }
            _ => Some((kind, name)),
        };
    }
    keyword.or_else(|| scan_assignment_declaration(line))
}
/// A scanner rather than a regex: correctly pairing triple-quoted strings
/// needs lookahead the `regex` crate does not offer, and mispairing them masks
/// away the code that follows a docstring instead of the docstring itself.
///
/// Quotes and comment markers are ASCII, and a UTF-8 continuation byte is
/// never equal to one, so scanning bytes cannot begin a literal mid-character.
/// Every masked byte becomes one space, so byte offsets survive and the result
/// is still valid UTF-8.
pub fn mask_source(content: &str) -> String {
    let src = content.as_bytes();
    let mut out = src.to_vec();
    let n = src.len();
    let mut i = 0usize;

    // Blank everything from `from` up to (not including) `to`, keeping newlines.
    let blank = |out: &mut Vec<u8>, from: usize, to: usize| {
        for byte in &mut out[from..to] {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
    };

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
                blank(&mut out, start, i.min(n));
            }
            b'/' if i + 1 < n && src[i + 1] == b'*' => {
                let start = i;
                i += 2;
                while i + 1 < n && !(src[i] == b'*' && src[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(n);
                blank(&mut out, start, i);
            }
            b'/' if i + 1 < n && src[i + 1] == b'/' => {
                let start = i;
                while i < n && src[i] != b'\n' {
                    i += 1;
                }
                blank(&mut out, start, i);
            }
            b'#' => {
                let start = i;
                while i < n && src[i] != b'\n' {
                    i += 1;
                }
                blank(&mut out, start, i);
            }
            _ => i += 1,
        }
    }

    String::from_utf8(out).unwrap_or_else(|_| content.to_string())
}

/// Where a line's text begins, or `None` if the line holds nothing.
fn indent_of(line: &str) -> Option<usize> {
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

/// Delimiters a body can close with, and the punctuation that may trail them:
/// `}`, `};`, `]),`.
const BODY_CLOSERS: &[u8] = b")]};,";

/// The last line of the body `start_line` opens.
///
/// The body is the run of lines indented deeper than the declaration itself.
/// Blank lines neither end it nor extend it — a paragraph break inside a
/// function does not close the function, and a span should not trail off into
/// the gap below its last statement.
fn body_end(lines: &[&str], start_line: u64, indent: usize) -> u64 {
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

/// Every declaration in `content`, in file order.
///
/// Returns an empty table for sources above [`MAX_SOURCE_BYTES`]; such a file
/// still matches, its hits simply fall back to fixed context windows.
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

    // Enclosing declarations are the ones still open at a shallower indent, so
    // a stack of open indents gives the nesting depth in one pass.
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

/// Whether a declaration of this kind declares the name it carries.
///
/// `impl Repo` re-opens a type its `struct` declares, and `impl Store for
/// Repo` names a trait declared somewhere else again. Both belong in the
/// table — nesting depth is read from it, and the methods inside one are
/// found through it — but neither is an answer to "where is this declared",
/// and an impl block names its type more often than the declaration does, so
/// it wins on match count whenever it is allowed to compete.
pub fn kind_declares(kind: &str) -> bool {
    kind != "impl"
}

/// The name a line declares, read from the line alone.
///
/// A cheap pre-ranking signal taken straight from the searcher's matched line,
/// before any file is read or masked. It can be fooled by a declaration
/// written inside a docstring; [`declarations`] masks those away and is the
/// authority. This only decides which files are worth reading.
pub fn declaration_name(line: &str) -> Option<&str> {
    scan_declaration(line)
        .filter(|(kind, _)| kind_declares(kind))
        .map(|(_, name)| name)
}

/// Declaration containing `line`, or `None` for code inside none of them.
///
/// Declarations are ordered and non-overlapping, so the only candidate is the
/// last one starting at or before the line. A linear scan here would be
/// O(matches × declarations), which is the difference between a fast query and
/// a slow one in a file with hundreds of symbols.
///
/// Spans end at their bodies rather than running to the next declaration, so
/// they no longer tile the file: a line between two of them — a class-level
/// constant below the last method — belongs to neither, and is reported as a
/// window instead of attributed to a body that does not hold it.
pub fn enclosing(decls: &[Declaration], line: u64) -> Option<&Declaration> {
    let idx = decls.partition_point(|d| d.start_line <= line);
    let found = decls.get(idx.checked_sub(1)?)?;
    (line <= found.end_line).then_some(found)
}

/// Split camelCase/PascalCase/snake_case/kebab-case into lowercase parts.
pub fn identifier_tokens(name: &str) -> Vec<String> {
    let chars: Vec<char> = name.chars().collect();
    let mut parts: Vec<String> = Vec::new();
    let mut current = String::new();

    let flush = |current: &mut String, parts: &mut Vec<String>| {
        if !current.is_empty() {
            parts.push(std::mem::take(current).to_lowercase());
        }
    };

    for (i, &c) in chars.iter().enumerate() {
        if !(c.is_alphanumeric()) {
            flush(&mut current, &mut parts);
            continue;
        }
        let prev = i.checked_sub(1).map(|p| chars[p]);
        let next = chars.get(i + 1).copied();
        let starts_word = match prev {
            None => false,
            Some(p) => {
                // camelCase, ACRONYMWord, and letter/digit transitions.
                (p.is_lowercase() && c.is_uppercase())
                    || (p.is_uppercase()
                        && c.is_uppercase()
                        && next.is_some_and(char::is_lowercase))
                    || (p.is_ascii_digit() != c.is_ascii_digit())
            }
        };
        if starts_word {
            flush(&mut current, &mut parts);
        }
        current.push(c);
    }
    flush(&mut current, &mut parts);
    parts
}

#[cfg(test)]
#[path = "spans_tests.rs"]
mod spans_tests;
