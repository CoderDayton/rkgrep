//! What counts as a comment or a literal, and what masking must preserve.

use super::names;
use crate::spans::{comment_source, mask_source};

#[test]
fn a_hash_that_opens_code_is_not_a_comment() {
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
    let src = "fn parse_url() { let u = \"http://parse/x\"; }";
    assert_eq!(names(src), vec!["parse_url"]);
}

#[test]
fn triple_quoted_docstrings_mask_as_one_unit() {
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

#[test]
fn a_block_comment_spans_the_lines_it_was_opened_across() {
    let src = "a /* x\n y */ b\n";
    assert_eq!(mask_source(src), "a     \n      b\n");
}

#[test]
fn a_star_ending_a_line_does_not_close_a_comment_on_the_next() {
    let src = "/* a *\n/ b */ c\n";
    let masked = mask_source(src);
    assert!(masked.contains('c'), "{masked:?}");
    assert!(!masked.contains('b'), "{masked:?}");
}

#[test]
fn a_backslash_ending_a_line_escapes_the_newline_inside_a_literal() {
    let src = "s = \"a\\\n b\"; code()\n";
    let masked = mask_source(src);
    assert!(masked.contains("code()"), "{masked:?}");
    assert!(!masked.contains('b'), "{masked:?}");
}

/// Masking is read line by line, and every rule downstream reads line numbers
/// off the result, so the bytes have to line up on real files and not only on
/// the ones a test thought to write.
#[test]
fn masking_this_crate_moves_nothing() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut checked = 0usize;
    let mut stack = vec![root];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("reading src") {
            let path = entry.expect("reading src").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().is_none_or(|ext| ext != "rs") {
                continue;
            }
            let src = std::fs::read_to_string(&path).expect("reading source");
            for masked in [mask_source(&src), comment_source(&src)] {
                assert_eq!(masked.len(), src.len(), "{}", path.display());
                let moved = masked
                    .bytes()
                    .zip(src.bytes())
                    .any(|(out, given)| (out == b'\n') != (given == b'\n'));
                assert!(!moved, "{}", path.display());
            }
            checked += 1;
        }
    }
    assert!(checked > 10, "expected this crate to have sources");
}
