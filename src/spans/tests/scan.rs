//! The four declaration shapes, and what each of them must refuse.

use super::names;
use crate::spans::{declaration_name, declarations};

#[test]
fn a_keyword_must_open_its_line_to_declare() {
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
fn line_declaration_hint_matches_the_full_extractor() {
    assert_eq!(declaration_name("def handler(request):"), Some("handler"));
    assert_eq!(declaration_name("    pub fn new() -> Self {"), Some("new"));
    assert_eq!(declaration_name("    return validate_token(x)"), None);
    assert_eq!(declaration_name("from enum import Enum"), None);
    assert_eq!(declaration_name("impl<T> Repo<T> {"), None);
    assert_eq!(names("impl<T> Repo<T> {\n}\n"), vec!["Repo"]);
}

#[test]
fn go_methods_declare_the_name_after_the_receiver() {
    let src = "func (r *Repo) Save(ctx context.Context) error {\n\treturn nil\n}\n";
    assert_eq!(names(src), vec!["Save"]);
}

#[test]
fn signatures_without_a_keyword_declare_their_name() {
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
    let src = "static int compute_hash(const char *s) {\n  return 0;\n}\n";
    assert_eq!(names(src), vec!["compute_hash"]);
}

#[test]
fn a_keyword_declaration_keeps_its_kind_when_both_rules_agree() {
    let decls = declarations("class Repo(db: Db) {\n}\n");
    assert_eq!(decls[0].name, "Repo");
    assert_eq!(decls[0].kind, "class");
}

#[test]
fn control_flow_is_never_a_declaration() {
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
    assert!(names("total = compute(x)\n").is_empty());
    assert!(names("count = 0\n").is_empty());
    assert!(names("if (a == b) {\n}\n").is_empty());
}

#[test]
fn rust_module_declarations_are_found() {
    let src = "mod file;\npub mod io;\npub(crate) mod std;\n";
    assert_eq!(names(src), vec!["file", "io", "std"]);
}

#[test]
fn a_parenthesized_visibility_does_not_end_the_qualifier_run() {
    let src = "pub(crate) unsafe trait Link {}\n";
    assert_eq!(names(src), vec!["Link"]);

    let src = "pub(in crate::util) fn helper() {}\n";
    assert_eq!(names(src), vec!["helper"]);

    let src = "pub(super) struct Inner;\n";
    assert_eq!(names(src), vec!["Inner"]);
}

#[test]
fn a_visibility_scope_is_not_read_as_a_parameter_list() {
    let src = "pub(crate) trait Kill {\n    fn kill(&mut self);\n}\n";
    assert_eq!(names(src), vec!["Kill", "kill"]);

    let src = "pub(super) enum Tick {\n    Fired,\n}\n";
    assert_eq!(names(src), vec!["Tick"]);

    let src = "pub(crate) mod cell {\n    use std::cell::RefCell;\n}\n";
    assert_eq!(names(src), vec!["cell"]);
}

#[test]
fn a_signature_named_like_a_qualifier_is_still_a_signature() {
    assert_eq!(names("open(int fd) {\n}\n"), vec!["open"]);

    assert_eq!(names("static void open(int fd) {\n}\n"), vec!["open"]);
    assert_eq!(names("static int final(void) {\n}\n"), vec!["final"]);
}

#[test]
fn a_generic_impl_declares_the_type_it_implements() {
    assert_eq!(names("impl<T> Repo<T> {\n}\n"), vec!["Repo"]);
    assert_eq!(names("impl<T: Debug> Repo<T> {\n}\n"), vec!["Repo"]);
    assert_eq!(names("impl<T> Store for Repo<T> {\n}\n"), vec!["Store"]);
}

#[test]
fn an_arrow_does_not_close_a_generic_parameter_list() {
    assert_eq!(
        names("impl<F: Fn() -> u32> Runner<F> {\n}\n"),
        vec!["Runner"]
    );
}

#[test]
fn a_comparison_is_not_a_generic_parameter_list() {
    assert!(names("if a<b && c>(d) {\n}\n").is_empty());
}

#[test]
fn a_call_after_a_non_qualifier_still_ends_the_run() {
    assert_eq!(names("if (ready(x)) {\n}\n"), Vec::<String>::new());
    assert_eq!(names("while (next(it)) {\n}\n"), Vec::<String>::new());
}
