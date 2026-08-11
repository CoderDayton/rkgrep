//! What counts as a comment or a literal, and what masking must preserve.

use super::names;
use crate::spans::{comment_source, mask_source};

#[test]
fn a_hash_that_opens_code_is_not_a_comment() {
    // Rust attributes, C preprocessor directives and CSS colors all open with
    // the byte a dozen other languages open a comment with.
    let src = "#![allow(dead_code)]\n#[derive(Debug)]\nstruct Kept;\n";
    assert_eq!(mask_source(src), src);
    assert_eq!(comment_source(src).trim(), "");

    let c = "#include <stdio.h>\n#define LIMIT 8\n";
    assert_eq!(mask_source(c), c);

    let css = "a { color: #fff; border: #aabbcc; }\n";
    assert_eq!(mask_source(css), css);
}

#[test]
fn a_hash_comment_still_masks() {
    for src in [
        "# spaced\n",
        "#unspaced\n",
        "## doubled\n",
        "#!/bin/sh\n",
        "#\n",
    ] {
        assert_eq!(mask_source(src).trim(), "", "{src:?}");
    }
}

#[test]
fn comment_source_keeps_comments_and_drops_everything_else() {
    let src = "fn real() {\n    let s = \"needle\"; // needle here\n}\n/* needle */\n";
    let comments = comment_source(src);
    assert_eq!(comments.len(), src.len());
    assert_eq!(comments.matches('\n').count(), src.matches('\n').count());
    assert_eq!(comments.matches("needle").count(), 2);
    assert!(!comments.contains("fn real"));
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
fn an_unterminated_quote_does_not_swallow_the_file() {
    let src = "# don't stop here\ndef survivor():\n    pass\n";
    assert_eq!(names(src), vec!["survivor"]);
}
