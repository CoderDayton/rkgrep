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

use super::pattern::SPLIT_PATTERN;
use super::{count, table, Vocabulary, VOCAB};

/// The published ranks, read from the source tree rather than the built image,
/// so a test failure separates a bad table from a bad lookup.
fn published_ranks() -> Vec<(Vec<u8>, u32)> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/tokenizer/o200k_base.tiktoken"
    );
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

/// One unbroken run of letters is a single piece, so the whole run merges
/// against the vocabulary at once. That is the path the linked queue in
/// [`super::Merges`] replaced a quadratic scan on, and the only way to know it
/// still merges in tiktoken's order is to ask tiktoken.
#[test]
fn one_long_unbroken_piece_matches_the_reference() {
    for len in [64, 500, 4096, 20_000] {
        assert_matches_reference(&"A".repeat(len));
        assert_matches_reference(&"deadbeef".repeat(len / 8));
        assert_matches_reference(&"9".repeat(len));
    }
}

/// A piece long enough that a quadratic merge would not finish. It is here as
/// a test rather than a benchmark because a regression shows up as a suite
/// that never returns, not as a wrong answer.
#[test]
fn a_very_long_piece_counts_promptly() {
    let text = "A".repeat(1_000_000);
    let started = std::time::Instant::now();
    let counted = count(&text);
    assert!(counted > 0);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(30),
        "counting one 1MB piece took {:?}",
        started.elapsed()
    );
}

#[test]
fn the_anchored_start_state_does_not_depend_on_context() {
    use regex_automata::dfa::Automaton;
    use regex_automata::{Anchored, Input};

    let haystack = "a b\n1_\u{300}\u{4e2d}!";
    let expected = *super::ANCHORED_START;
    for at in 0..=haystack.len() {
        if !haystack.is_char_boundary(at) {
            continue;
        }
        let input = Input::new(haystack).anchored(Anchored::Yes).range(at..);
        assert_eq!(
            super::SPLITTER.start_state_forward(&input),
            Ok(expected),
            "the start state at byte {at} is not the one `split` reuses"
        );
    }
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
        "\u{0b} y",
        "\u{0b}\u{0b}x",
        "\u{0c} y",
        "a\u{0b} \u{0b}b",
    ] {
        assert_matches_reference(text);
    }
}

#[test]
fn real_source_files_match_the_reference() {
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
