//! Where a declaration's span starts and ends, how deep it nests, and which
//! declaration owns a given line.

use crate::spans::{declarations, enclosing, MAX_SOURCE_BYTES};

#[test]
fn a_span_ends_before_the_next_declaration() {
    let src = "fn first() {}\nfn second() {}\n";
    let decls = declarations(src);
    assert_eq!((decls[0].start_line, decls[0].end_line), (1, 1));
    assert_eq!((decls[1].start_line, decls[1].end_line), (2, 2));
}

#[test]
fn a_declaration_ends_at_its_body_not_at_the_end_of_the_file() {
    // The last declaration in a file used to run to EOF, so a trailing data
    // literal became part of the function above it and the span outgrew any
    // budget the packer could give it.
    let mut src = String::from("def find_user(id):\n    return id\n\nDATA = [\n");
    for i in 0..50 {
        src.push_str(&format!("    \"row {i}\",\n"));
    }
    src.push_str("]\n");
    let decls = declarations(&src);
    assert_eq!((decls[0].start_line, decls[0].end_line), (1, 2));
}

#[test]
fn a_closing_brace_stays_with_the_body_it_closes() {
    // The brace sits at the declaration's own indent, so the body run ends
    // above it. A function span cut off before its closing brace reads as
    // truncated source.
    let src = "int main(void) {\n  return 0;\n}\n\nstatic int helper(void) {\n  return 1;\n}\n";
    let decls = declarations(src);
    assert_eq!((decls[0].start_line, decls[0].end_line), (1, 3));
    assert_eq!((decls[1].start_line, decls[1].end_line), (5, 7));
}

#[test]
fn a_word_that_closes_a_block_stays_with_its_body() {
    // Ruby and Lua close a body with a word rather than a brace, and it sits
    // at the declaration's own indent exactly as a brace does.
    let src = "def save(x)\n  store(x)\nend\n\ndef load(x)\n  fetch(x)\nend\n";
    let decls = declarations(src);
    assert_eq!((decls[0].start_line, decls[0].end_line), (1, 3));
}

#[test]
fn a_declaration_that_contains_others_ends_before_the_first_of_them() {
    // A class is its header. The body is the methods, each already a span of
    // its own, so returning the class whole returns them twice and spends a
    // whole budget on one file.
    let src = "class Repo:\n    \"\"\"Docs.\"\"\"\n    def save(self):\n        pass\n    def load(self):\n        pass\n";
    let decls = declarations(src);
    assert_eq!(decls[0].name, "Repo");
    assert_eq!((decls[0].start_line, decls[0].end_line), (1, 2));
}

#[test]
fn enclosing_returns_none_between_two_declarations() {
    // A class-level assignment after the last method is inside neither: the
    // method has ended and the class ends at its header. Reported as a window
    // rather than attributed to a body that does not hold it.
    let src = "class Repo:\n    def save(self):\n        pass\n    LIMIT = 10\n";
    let decls = declarations(src);
    assert!(enclosing(&decls, 4).is_none());
}

#[test]
fn spans_stay_small_in_a_realistic_file() {
    // Regression for a corrupted span table: one bogus declaration used to
    // produce a single span covering hundreds of lines.
    let mut src = String::new();
    for i in 0..20 {
        src.push_str(&format!(
            "def f{i}():\n    \"\"\"doc {i}\"\"\"\n    return {i}\n"
        ));
    }
    let decls = declarations(&src);
    assert_eq!(decls.len(), 20);
    assert!(decls.iter().all(|d| d.end_line - d.start_line < 6));
}

#[test]
fn enclosing_finds_the_owning_declaration() {
    let src = "fn first() {\n    body\n}\nfn second() {\n    body\n}\n";
    let decls = declarations(src);
    assert_eq!(enclosing(&decls, 2).map(|d| d.name.as_str()), Some("first"));
    assert_eq!(
        enclosing(&decls, 5).map(|d| d.name.as_str()),
        Some("second")
    );
}

#[test]
fn enclosing_returns_none_above_the_first_declaration() {
    let src = "import os\n\nfn first() {}\n";
    let decls = declarations(src);
    assert!(enclosing(&decls, 1).is_none());
}

#[test]
fn indentation_gives_the_nesting_depth() {
    let src = "\
def top_level():
    return 1

class Repo:
    def save(self):
        def inner():
            return 2
        return inner
";
    let decls = declarations(src);
    let depths: Vec<(String, usize)> = decls.iter().map(|d| (d.name.clone(), d.depth)).collect();
    assert_eq!(
        depths,
        vec![
            ("top_level".to_string(), 0),
            ("Repo".to_string(), 0),
            ("save".to_string(), 1),
            ("inner".to_string(), 2),
        ]
    );
}

#[test]
fn a_file_that_does_not_indent_reports_everything_at_top_level() {
    // C functions all start at column zero; nothing there is nested.
    let src = "int a(void) {\n  return 1;\n}\nint b(void) {\n  return 2;\n}\n";
    assert!(declarations(src).iter().all(|d| d.depth == 0));
}

#[test]
fn an_oversized_source_yields_no_declarations() {
    let huge = "a".repeat(MAX_SOURCE_BYTES + 1);
    assert!(declarations(&huge).is_empty());
}
