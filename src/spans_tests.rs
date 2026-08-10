//! Unit tests for declaration extraction and masking.

use super::*;

fn names(src: &str) -> Vec<String> {
    declarations(src).into_iter().map(|d| d.name).collect()
}

#[test]
fn masking_preserves_length_and_newlines() {
    let src = "let u = \"http://x/y\"; // note\nfn real() {}";
    let masked = mask_source(src);
    assert_eq!(masked.len(), src.len());
    assert_eq!(masked.matches('\n').count(), src.matches('\n').count());
    assert!(masked.contains("fn real()"));
    assert!(!masked.contains("note"));
}

#[test]
fn a_url_in_a_string_is_not_a_comment() {
    // Masking strings before comments keeps `//` inside a literal from
    // hiding the declaration that shares its line.
    let src = "fn parse_url() { let u = \"http://parse/x\"; }";
    assert_eq!(names(src), vec!["parse_url"]);
}

#[test]
fn triple_quoted_docstrings_mask_as_one_unit() {
    // Mispairing a docstring's quotes masks the code that follows it, which
    // silently erases most declarations in a Python file.
    let src = "\"\"\"Doc mentioning def fake() and class Ghost.\"\"\"\n\
               def real_one():\n    \
               \"\"\"Inner doc with class AlsoGhost.\"\"\"\n    \
               return 1\n\
               class RealTwo:\n    \
               def method(self):\n        \
               pass\n";
    assert_eq!(names(src), vec!["real_one", "RealTwo", "method"]);
}

#[test]
fn a_keyword_must_open_its_line_to_declare() {
    // `enum` is a declaration keyword, but `from enum import Enum` declares
    // nothing; reading it as a symbol named `import` also swallows every
    // real declaration until the next match.
    let src = "from enum import Enum\nimport type_helpers\ndef real():\n    pass\n";
    assert_eq!(names(src), vec!["real"]);
}

#[test]
fn modifiers_may_precede_a_declaration() {
    let src = "pub fn rust_one() {}\n\
               export default function jsOne() {}\n\
               async def py_one():\n    pass\n\
               public static class JavaOne {}\n";
    assert_eq!(names(src), vec!["rust_one", "jsOne", "py_one", "JavaOne"]);
}

#[test]
fn an_unterminated_quote_does_not_swallow_the_file() {
    let src = "# don't stop here\ndef survivor():\n    pass\n";
    assert_eq!(names(src), vec!["survivor"]);
}

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
fn identifier_tokens_split_every_common_case() {
    assert_eq!(identifier_tokens("validateToken"), ["validate", "token"]);
    assert_eq!(identifier_tokens("validate_token"), ["validate", "token"]);
    assert_eq!(identifier_tokens("ValidateToken"), ["validate", "token"]);
    assert_eq!(identifier_tokens("HTTPServer"), ["http", "server"]);
    assert_eq!(identifier_tokens("parse2Json"), ["parse", "2", "json"]);
}

#[test]
fn line_declaration_hint_matches_the_full_extractor() {
    assert_eq!(declaration_name("def handler(request):"), Some("handler"));
    assert_eq!(declaration_name("    pub fn new() -> Self {"), Some("new"));
    assert_eq!(declaration_name("    return validate_token(x)"), None);
    assert_eq!(declaration_name("from enum import Enum"), None);
    // An impl block re-opens a name declared elsewhere, so it is not the
    // signal that this file is where the query is defined.
    assert_eq!(declaration_name("impl<T> Repo<T> {"), None);
    // The table still carries it: nesting depth is read from it.
    assert_eq!(names("impl<T> Repo<T> {\n}\n"), vec!["Repo"]);
}

#[test]
fn an_oversized_source_yields_no_declarations() {
    let huge = "a".repeat(MAX_SOURCE_BYTES + 1);
    assert!(declarations(&huge).is_empty());
}

// --------------------------------------------------------------------------
// Declaration forms with no keyword in front of the name
// --------------------------------------------------------------------------

#[test]
fn tables_are_sorted_for_binary_search() {
    // Lookup is a binary search, so an unsorted table silently stops
    // recognizing whatever sits past the first inversion.
    for table in [KEYWORDS, MODIFIERS, CONTROL_WORDS] {
        assert!(table.windows(2).all(|w| w[0] < w[1]), "{table:?}");
    }
}

#[test]
fn go_methods_declare_the_name_after_the_receiver() {
    let src = "func (r *Repo) Save(ctx context.Context) error {\n\treturn nil\n}\n";
    assert_eq!(names(src), vec!["Save"]);
}

#[test]
fn signatures_without_a_keyword_declare_their_name() {
    // C, C++, Java, and every method body in a JavaScript class.
    assert_eq!(names("int main(void) {\n\treturn 0;\n}\n"), vec!["main"]);
    assert_eq!(names("void Repo::save(int x) {\n}\n"), vec!["save"]);
    assert_eq!(
        names("  public int compute(int x) {\n  }\n"),
        vec!["compute"]
    );
    assert_eq!(names("deploy() {\n  echo hi\n}\n"), vec!["deploy"]);
}

#[test]
fn a_return_type_is_not_the_declared_name() {
    // The keyword rule reads `static` as the declaring keyword and `int` as
    // the name; the signature the line opens says otherwise.
    let src = "static int compute_hash(const char *s) {\n  return 0;\n}\n";
    assert_eq!(names(src), vec!["compute_hash"]);
}

#[test]
fn a_keyword_declaration_keeps_its_kind_when_both_rules_agree() {
    // `class Repo(db: Db) {` is a class, not a function, even though it has
    // a parameter list and opens a brace.
    let decls = declarations("class Repo(db: Db) {\n}\n");
    assert_eq!(decls[0].name, "Repo");
    assert_eq!(decls[0].kind, "class");
}

#[test]
fn control_flow_is_never_a_declaration() {
    // Each of these opens a brace after a parenthesized group, which is the
    // shape the signature rule looks for.
    for line in [
        "void f(void) {\n  if (ready(x)) {\n    return 1;\n  }\n}\n",
        "function f() {\n  while (queue.length) {\n    step();\n  }\n}\n",
        "function f() {\n  for (const k of keys) {\n    use(k);\n  }\n}\n",
    ] {
        assert_eq!(names(line), vec!["f"], "{line}");
    }
}

#[test]
fn an_unclosed_argument_list_is_a_call_not_a_definition() {
    // `describe("x", () => {` opens two groups and closes one; the callback
    // is the thing being defined, and it has no name.
    assert!(names("describe(\"x\", () => {\n  ok();\n});\n").is_empty());
    assert!(names("callback(err, function () {\n  ok();\n});\n").is_empty());
}

#[test]
fn callables_bound_to_a_name_declare_that_name() {
    assert_eq!(names("handler = lambda req: req\n"), vec!["handler"]);
    assert_eq!(names("let load = function (id) {\n};\n"), vec!["load"]);
    assert_eq!(
        names("Api.prototype.load = function (id) {\n};\n"),
        vec!["load"]
    );
    assert_eq!(
        names("var h = func(w http.ResponseWriter) {\n}\n"),
        vec!["h"]
    );
    assert_eq!(names("onClick = (event) => {\n};\n"), vec!["onClick"]);
}

#[test]
fn assigning_a_value_declares_nothing() {
    // Only a callable on the right makes the left side a declaration.
    assert!(names("total = compute(x)\n").is_empty());
    assert!(names("count = 0\n").is_empty());
    assert!(names("if (a == b) {\n}\n").is_empty());
}

// --------------------------------------------------------------------------
// Nesting
// --------------------------------------------------------------------------

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
fn rust_module_declarations_are_found() {
    // `mod` opens a Rust module, and a bare `mod name;` is the whole
    // declaration. Missing it costs the file that declares a module the top
    // rank for that module's name.
    let src = "mod file;\npub mod io;\npub(crate) mod std;\n";
    assert_eq!(names(src), vec!["file", "io", "std"]);
}

#[test]
fn a_parenthesized_visibility_does_not_end_the_qualifier_run() {
    // `pub(crate)` is one qualifier, not a word followed by a call. Reading
    // the `(` as the end of the run rejects the whole line, which loses every
    // crate-visible declaration in a Rust file.
    let src = "pub(crate) unsafe trait Link {}\n";
    assert_eq!(names(src), vec!["Link"]);

    let src = "pub(in crate::util) fn helper() {}\n";
    assert_eq!(names(src), vec!["helper"]);

    let src = "pub(super) struct Inner;\n";
    assert_eq!(names(src), vec!["Inner"]);
}

#[test]
fn a_visibility_scope_is_not_read_as_a_parameter_list() {
    // `pub(crate) trait Kill {` is also the shape of a signature: a name, a
    // parameter list that closes, and a brace. Read that way it declares
    // `pub`, and the file declaring the trait falls below every file that
    // implements it.
    let src = "pub(crate) trait Kill {\n    fn kill(&mut self);\n}\n";
    assert_eq!(names(src), vec!["Kill", "kill"]);

    let src = "pub(super) enum Tick {\n    Fired,\n}\n";
    assert_eq!(names(src), vec!["Tick"]);

    let src = "pub(crate) mod cell {\n    use std::cell::RefCell;\n}\n";
    assert_eq!(names(src), vec!["cell"]);
}

#[test]
fn a_signature_named_like_a_qualifier_is_still_a_signature() {
    // `open` is a Kotlin qualifier and a C function. With no keyword on the
    // line there is nothing to defer to, so the signature rule keeps it.
    assert_eq!(names("open(int fd) {\n}\n"), vec!["open"]);

    // With a keyword on the line, the qualifier only wins when it comes
    // first. Here `static` is the keyword rule's own reading and the name is
    // behind it, so the signature still names the function.
    assert_eq!(names("static void open(int fd) {\n}\n"), vec!["open"]);
    assert_eq!(names("static int final(void) {\n}\n"), vec!["final"]);
}

#[test]
fn a_generic_impl_declares_the_type_it_implements() {
    // A keyword followed straight by `<` used to end the word run, so every
    // generic impl block was invisible. Its methods then reported the depth
    // of a top-level function, which inverts the shadowing penalty against
    // the same method inside a plain impl.
    assert_eq!(names("impl<T> Repo<T> {\n}\n"), vec!["Repo"]);
    assert_eq!(names("impl<T: Debug> Repo<T> {\n}\n"), vec!["Repo"]);
    assert_eq!(names("impl<T> Store for Repo<T> {\n}\n"), vec!["Store"]);
}

#[test]
fn an_arrow_does_not_close_a_generic_parameter_list() {
    // `->` and `=>` both end in `>`. Reading either as the closing bracket
    // cuts the run short and loses the declaration again -- and the bound it
    // cuts inside has the shape of a signature, so the line then declares the
    // trait in the bound instead of the type.
    assert_eq!(
        names("impl<F: Fn() -> u32> Runner<F> {\n}\n"),
        vec!["Runner"]
    );
}

#[test]
fn a_comparison_is_not_a_generic_parameter_list() {
    // Only a keyword may carry one. Skipping angle brackets after any word
    // would read `if a<b && c>(d) {` as a declaration of `d`.
    assert!(names("if a<b && c>(d) {\n}\n").is_empty());
}

#[test]
fn a_call_after_a_non_qualifier_still_ends_the_run() {
    // Only a qualifier may carry a scope. Skipping parentheses after any word
    // would make `if (ready(x)) {` look like a declaration of `x`.
    assert_eq!(names("if (ready(x)) {\n}\n"), Vec::<String>::new());
    assert_eq!(names("while (next(it)) {\n}\n"), Vec::<String>::new());
}
