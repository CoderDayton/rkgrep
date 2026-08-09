//! The counter is only worth anything if it agrees with tiktoken exactly, so
//! it is checked against a reference implementation rather than against itself.
//!
//! `riptoken` is a development dependency for this file alone. It builds its
//! encoder from the same published ranks at runtime — the cost this module
//! exists to avoid — and produces tiktoken's counts, which makes it the
//! authority these tests measure against.

use std::sync::LazyLock;

use base64::Engine;
use riptoken::CoreBPE;
use rustc_hash::FxHashMap;

use super::{count, table, Vocabulary, SPLIT_PATTERN, VOCAB};

/// The published ranks, read from the source tree rather than the built image,
/// so a test failure separates a bad table from a bad lookup.
fn published_ranks() -> Vec<(Vec<u8>, u32)> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/o200k_base.tiktoken");
    let text = std::fs::read(path).expect("the vocabulary ships in the source tree");
    text.split(|&b| b == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| {
            let mut cols = line.splitn(2, |&b| b == b' ');
            let (token, rank) = (cols.next().expect("a line"), cols.next().expect("a rank"));
            (
                base64::engine::general_purpose::STANDARD
                    .decode(token)
                    .expect("published tokens are base64"),
                std::str::from_utf8(rank)
                    .expect("ranks are ascii")
                    .trim()
                    .parse()
                    .expect("ranks are integers"),
            )
        })
        .collect()
}

/// tiktoken, built the slow way, as the reference.
static REFERENCE: LazyLock<CoreBPE> = LazyLock::new(|| {
    let ranks: FxHashMap<Vec<u8>, u32> = published_ranks().into_iter().collect();
    let specials = [
        ("<|endoftext|>".to_string(), 199_999),
        ("<|endofprompt|>".to_string(), 200_018),
    ]
    .into_iter()
    .collect();
    CoreBPE::new(ranks, specials, super::SPLIT_PATTERN_REFERENCE)
        .expect("the published ranks build an encoder")
});

fn assert_matches_reference(text: &str) {
    assert_eq!(
        count(text),
        REFERENCE.encode_ordinary(text).len(),
        "count disagrees with tiktoken for {text:?}"
    );
}

#[test]
fn every_published_token_resolves_to_its_own_rank() {
    let vocab: &Vocabulary = &VOCAB;
    for (token, rank) in published_ranks() {
        assert_eq!(
            vocab.rank(&token),
            Some(rank),
            "table lost {token:?} (rank {rank})"
        );
    }
}

#[test]
fn a_token_the_vocabulary_lacks_is_not_invented() {
    let vocab: &Vocabulary = &VOCAB;
    // A byte sequence no vocabulary entry can be: longer than the longest
    // token, and not valid UTF-8 either.
    assert_eq!(vocab.rank(&[0xffu8; 200]), None);
    assert_eq!(vocab.rank(b""), None);
}

#[test]
fn the_layout_bounds_hold_for_this_vocabulary() {
    let entries = published_ranks();
    assert!(entries.iter().all(|(t, _)| t.len() <= table::MAX_TOKEN_LEN));
    assert!(entries.iter().all(|(_, r)| *r <= table::MAX_RANK));
    let blob: usize = entries.iter().map(|(t, _)| t.len()).sum();
    assert!(blob <= table::MAX_BLOB_LEN);
}

#[test]
fn known_o200k_counts() {
    // Prose, code, punctuation, numbers, whitespace, CJK and emoji. The values
    // are `OpenAI`'s for the same input.
    let cases = [
        ("", 0),
        ("hello world", 2),
        ("The quick brown fox jumps over the lazy dog.", 10),
        ("fn main() { println!(\"{}\", 42); }", 10),
        ("café résumé naïve — em—dash … 你好世界 🚀🔥", 16),
        ("a\nb\r\nc\t\td   multiple   spaces", 11),
        ("1234567890 3.14159 0x1F <|endoftext|> not-special", 23),
        ("https://example.com/path?q=1&r=2#frag", 13),
    ];
    for (text, expected) in cases {
        assert_eq!(count(text), expected, "count for {text:?}");
    }
}

#[test]
fn whitespace_runs_match_the_reference() {
    // The one alternative the linear-time engine cannot express is the one
    // that decides where a run of blanks ends, so every shape of run is
    // checked: interior, trailing, before a line break, after one, and at the
    // end of the text with nothing following.
    for text in [
        "a b",
        "a  b",
        "a   b",
        "a ",
        "a  ",
        "a   ",
        "   ",
        " ",
        "",
        "a\n  b",
        "a  \n  b",
        "a  \n\n  b",
        "a\t\tb",
        "a \t b",
        "a  \r\n  b",
        "a  \r\n",
        "\n   ",
        "   \n",
        "def f():\n    return 1\n\n\n",
        "x = {\n    'k': 1,\n}\n   ",
    ] {
        assert_matches_reference(text);
    }
}

#[test]
fn real_source_files_match_the_reference() {
    // Whole files, because the pieces that break a tokenizer are the ones that
    // straddle constructs: a docstring against code, a URL in a comment, an
    // identifier against punctuation.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut checked = 0;
    for dir in ["src", "docs", "benches"] {
        for entry in std::fs::read_dir(root.join(dir)).expect("the tree ships these directories") {
            let path = entry.expect("a readable entry").path();
            let is_text = path
                .extension()
                .is_some_and(|e| e == "rs" || e == "md" || e == "toml");
            if !is_text {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("source files are UTF-8");
            assert_eq!(
                count(&text),
                REFERENCE.encode_ordinary(&text).len(),
                "count disagrees with tiktoken for {}",
                path.display()
            );
            checked += 1;
        }
    }
    assert!(checked > 5, "the parity corpus went missing");
}

#[test]
fn the_two_split_patterns_differ_only_in_the_lookahead() {
    // The reference pattern is the published one; ours is that pattern with
    // `\s+(?!\S)` removed. If they ever drift apart in any other way, the
    // whitespace rule in `split` is being applied to a different language.
    assert_eq!(
        SPLIT_PATTERN_REFERENCE_MINUS_LOOKAHEAD.as_str(),
        SPLIT_PATTERN,
        "the two split patterns have drifted"
    );
}

/// The reference pattern with its lookahead alternative removed, built by text
/// so the comparison above cannot be satisfied by editing one side.
static SPLIT_PATTERN_REFERENCE_MINUS_LOOKAHEAD: LazyLock<String> =
    LazyLock::new(|| super::SPLIT_PATTERN_REFERENCE.replace(r"\s+(?!\S)|", ""));
