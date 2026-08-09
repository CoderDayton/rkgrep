//! Ground truth: which file declares a symbol, and which files mention it.
//!
//! Every parser here is a third-party tree-sitter grammar, never rkgrep's
//! extractor, so the quality benchmark is not scored against its own notion
//! of what a declaration is.
//!
//! A declaration counts only at the top level of a file. That keeps the
//! answer unambiguous — a symbol declared once, at one place — which is what
//! makes a query usable as ground truth at all.

use std::collections::{BTreeMap, BTreeSet};

use tree_sitter::{Language as TsLanguage, Node, Parser};

/// A language the quality benchmark can measure retrieval on.
pub struct Language {
    pub name: &'static str,
    pub suffixes: &'static [&'static str],
    /// Glob handed to both `rg` and rkgrep, so they see the same files.
    pub glob: &'static str,
    /// Matches a declaration line, for the declarations-first rg baseline.
    /// `{name}` is replaced with the escaped query symbol.
    pub def_pattern: &'static str,
    /// Node kinds carrying a `name` field that declare a top-level symbol.
    def_kinds: &'static [&'static str],
    /// Node kinds that wrap declarations rather than being one, descended
    /// into before the `def_kinds` test is applied.
    wrapper_kinds: &'static [&'static str],
    /// Node kinds whose text counts as mentioning a symbol.
    ident_kinds: &'static [&'static str],
    ts: fn() -> TsLanguage,
}

pub const PYTHON: Language = Language {
    name: "python",
    suffixes: &[".py"],
    glob: "*.py",
    def_pattern: r"^\s*(?:async\s+def|def|class)\s+{name}\b",
    def_kinds: &["function_definition", "class_definition"],
    wrapper_kinds: &["decorated_definition"],
    ident_kinds: &["identifier"],
    ts: || tree_sitter_python::LANGUAGE.into(),
};

pub const GO: Language = Language {
    name: "go",
    suffixes: &[".go"],
    glob: "*.go",
    def_pattern: r"^\s*(?:func|type|var|const)\s+(?:\([^)]*\)\s*)?{name}\b",
    def_kinds: &[
        "function_declaration",
        "method_declaration",
        "type_spec",
        "var_spec",
        "const_spec",
    ],
    wrapper_kinds: &["type_declaration", "var_declaration", "const_declaration"],
    ident_kinds: &[
        "identifier",
        "field_identifier",
        "type_identifier",
        "package_identifier",
    ],
    ts: || tree_sitter_go::LANGUAGE.into(),
};

pub const RUST: Language = Language {
    name: "rust",
    suffixes: &[".rs"],
    glob: "*.rs",
    def_pattern: concat!(
        r"^\s*(?:pub\s+)?(?:pub\([^)]*\)\s+)?",
        r#"(?:default\s+|async\s+|const\s+|unsafe\s+|extern\s+(?:"[^"]*"\s+)?)*"#,
        r"(?:fn|struct|enum|union|trait|type|const|static|mod|macro_rules!)\s+{name}\b",
    ),
    def_kinds: &[
        "function_item",
        "struct_item",
        "enum_item",
        "union_item",
        "trait_item",
        "type_item",
        "const_item",
        "static_item",
        "mod_item",
        "macro_definition",
    ],
    wrapper_kinds: &[],
    ident_kinds: &["identifier", "type_identifier", "field_identifier"],
    ts: || tree_sitter_rust::LANGUAGE.into(),
};

/// Qualifier run in front of a JavaScript or TypeScript declaration, for the
/// rg baseline. Shared because the TypeScript grammar is a superset.
const JS_DEF_PATTERN: &str = concat!(
    r"^\s*(?:export\s+)?(?:default\s+)?(?:declare\s+)?(?:abstract\s+)?(?:async\s+)?",
    r"(?:function\s*\*?|class|const|let|var|interface|type|enum)\s+{name}\b",
);

/// Node kinds that bind a name in JavaScript and TypeScript. `variable_declarator`
/// covers `const x = ...`, which is how most of a modern module declares things.
const JS_DEF_KINDS: &[&str] = &[
    "function_declaration",
    "generator_function_declaration",
    "class_declaration",
    "variable_declarator",
    "interface_declaration",
    "type_alias_declaration",
    "enum_declaration",
    "abstract_class_declaration",
];

/// `export ...` and `const ...` wrap the declaration rather than being one.
const JS_WRAPPER_KINDS: &[&str] = &[
    "export_statement",
    "lexical_declaration",
    "variable_declaration",
];

const JS_IDENT_KINDS: &[&str] = &[
    "identifier",
    "property_identifier",
    "shorthand_property_identifier",
    "type_identifier",
];

pub const JAVASCRIPT: Language = Language {
    name: "javascript",
    suffixes: &[".js", ".mjs", ".jsx"],
    glob: "*.{js,mjs,jsx}",
    def_pattern: JS_DEF_PATTERN,
    def_kinds: JS_DEF_KINDS,
    wrapper_kinds: JS_WRAPPER_KINDS,
    ident_kinds: JS_IDENT_KINDS,
    ts: || tree_sitter_javascript::LANGUAGE.into(),
};

pub const TYPESCRIPT: Language = Language {
    name: "typescript",
    suffixes: &[".ts"],
    glob: "*.ts",
    def_pattern: JS_DEF_PATTERN,
    def_kinds: JS_DEF_KINDS,
    wrapper_kinds: JS_WRAPPER_KINDS,
    ident_kinds: JS_IDENT_KINDS,
    ts: || tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
};

pub const ALL: &[&Language] = &[&PYTHON, &GO, &RUST, &JAVASCRIPT, &TYPESCRIPT];

pub fn by_name(name: &str) -> Option<&'static Language> {
    ALL.iter().copied().find(|l| l.name == name)
}

/// What the parser says about a corpus.
pub struct Truth {
    /// symbol -> paths declaring it at the top level
    pub definitions: BTreeMap<String, BTreeSet<String>>,
    /// symbol -> paths mentioning it anywhere
    pub references: BTreeMap<String, BTreeSet<String>>,
    /// Files the grammar could not parse cleanly, excluded from truth.
    pub unparsed: usize,
}

impl Language {
    pub fn parse_truth(&self, files: &BTreeMap<String, String>) -> Truth {
        let mut parser = Parser::new();
        parser
            .set_language(&(self.ts)())
            .expect("grammar and tree-sitter runtime are the same major version");

        let mut truth = Truth {
            definitions: BTreeMap::new(),
            references: BTreeMap::new(),
            unparsed: 0,
        };
        for (path, text) in files {
            let Some(tree) = parser.parse(text.as_bytes(), None) else {
                truth.unparsed += 1;
                continue;
            };
            let root = tree.root_node();
            // A file with syntax errors yields a partial tree whose
            // declarations may be misattributed, so it is not truth.
            if root.has_error() {
                truth.unparsed += 1;
                continue;
            }
            self.collect_defs(root, text, path, &mut truth.definitions);
            self.collect_refs(root, text, path, &mut truth.references);
        }
        truth
    }

    /// Top-level declarations, descending only through wrapper kinds.
    fn collect_defs(
        &self,
        root: Node,
        text: &str,
        path: &str,
        out: &mut BTreeMap<String, BTreeSet<String>>,
    ) {
        let mut cursor = root.walk();
        let mut pending: Vec<Node> = root.named_children(&mut cursor).collect();
        while let Some(node) = pending.pop() {
            if self.def_kinds.contains(&node.kind()) {
                if let Some(name) = node.child_by_field_name("name") {
                    if let Ok(name) = name.utf8_text(text.as_bytes()) {
                        out.entry(name.to_string())
                            .or_default()
                            .insert(path.to_string());
                    }
                }
            } else if self.wrapper_kinds.contains(&node.kind()) {
                let mut inner = node.walk();
                pending.extend(node.named_children(&mut inner));
            }
        }
    }

    /// Every identifier anywhere in the file.
    fn collect_refs(
        &self,
        root: Node,
        text: &str,
        path: &str,
        out: &mut BTreeMap<String, BTreeSet<String>>,
    ) {
        let mut pending = vec![root];
        while let Some(node) = pending.pop() {
            if self.ident_kinds.contains(&node.kind()) {
                if let Ok(name) = node.utf8_text(text.as_bytes()) {
                    out.entry(name.to_string())
                        .or_default()
                        .insert(path.to_string());
                }
            }
            let mut cursor = node.walk();
            pending.extend(node.named_children(&mut cursor));
        }
    }
}
