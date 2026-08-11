//! The word tables and byte sets every rule is written against.
//!
//! Recognizing a declaration is mostly asking "is this word one of these
//! twenty" and "is every byte of this run one of these dozen", for every
//! matched line in the tree. Both questions are answered from a table built at
//! compile time rather than by walking a list.

/// Declaration keywords, sorted so lookup is a binary search.
/// `tables_are_sorted_for_binary_search` keeps this promise honest.
pub(super) const KEYWORDS: &[&str] = &[
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
pub(super) const MODIFIERS: &[&str] = &[
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

/// Words that open a control-flow construct. Whatever follows one of these is
/// a call or a test, never a definition: `if (ready(x)) {` declares nothing,
/// and without this the signature rule would name it `ready`.
pub(super) const CONTROL_WORDS: &[&str] = &[
    "and", "assert", "await", "case", "catch", "defer", "delete", "do", "elif", "else", "except",
    "finally", "for", "go", "if", "in", "is", "lock", "match", "new", "not", "or", "raise",
    "return", "select", "sizeof", "switch", "throw", "try", "typeof", "unless", "until", "using",
    "when", "while", "with", "yield",
];

/// C preprocessor directives. `#` opens a line comment in shell, Python, Ruby
/// and YAML, and opens one of these in C — only the word after it tells them
/// apart. Sorted, as [`KEYWORDS`] is.
pub(super) const PREPROCESSOR_DIRECTIVES: &[&str] = &[
    "define", "elif", "else", "endif", "error", "if", "ifdef", "ifndef", "include", "line",
    "pragma", "undef", "warning",
];

/// Every value a byte can take, which is the width of the tables here.
const BYTE_VALUES: usize = 256;

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
const CONTROL_INDEX: [(u8, u8); BYTE_VALUES] = first_byte_index(CONTROL_WORDS);
const DIRECTIVE_INDEX: [(u8, u8); BYTE_VALUES] = first_byte_index(PREPROCESSOR_DIRECTIVES);

pub(super) fn is_keyword(word: &str) -> bool {
    table_holds(KEYWORDS, &KEYWORD_INDEX, word)
}

pub(super) fn is_modifier(word: &str) -> bool {
    table_holds(MODIFIERS, &MODIFIER_INDEX, word)
}

pub(super) fn is_control(word: &str) -> bool {
    table_holds(CONTROL_WORDS, &CONTROL_INDEX, word)
}

pub(super) fn is_preprocessor_directive(word: &str) -> bool {
    table_holds(PREPROCESSOR_DIRECTIVES, &DIRECTIVE_INDEX, word)
}

pub(super) fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// A byte set as a table, for the shape tests the scanners run.
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
pub(super) const SIGNATURE_PREFIX: [bool; BYTE_VALUES] = byte_set(b" \t*&:<>,[]~");

/// What may sit between a parameter list and the brace: return types, `const`,
/// `noexcept`, `throws E`.
pub(super) const SIGNATURE_TAIL: [bool; BYTE_VALUES] = byte_set(b" \t*&:<>,[]?-.");

/// What may sit in front of an assigned name: `Api.prototype.load` is a name.
pub(super) const ASSIGNMENT_PREFIX: [bool; BYTE_VALUES] = byte_set(b" \t.:[]*&@$");

/// Operators that make an `=` a compound assignment rather than a binding.
pub(super) const COMPOUND_OPERATOR: [bool; BYTE_VALUES] = {
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
pub(super) fn all_bytes_in(text: &str, set: &[bool; BYTE_VALUES]) -> bool {
    text.bytes().all(|byte| set[byte as usize])
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
