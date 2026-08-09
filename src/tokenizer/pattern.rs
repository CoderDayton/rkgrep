//! The pattern `o200k_base` splits on, shared by the build and the binary.
//!
//! `build.rs` compiles it into a DFA and the tokenizer searches with that DFA,
//! so the two have to be reading the same pattern -- this file is the one copy
//! of it.

/// The pattern `o200k_base` splits on, with the one alternative a DFA cannot
/// express removed.
///
/// The original's third whitespace alternative is `\s+(?!\S)`, and negative
/// lookahead is exactly what a finite automaton trades away for linear time.
/// Dropping it here and applying its rule in `split` keeps the automaton.
pub const SPLIT_PATTERN: &str = concat!(
    r"[^\r\n\p{L}\p{N}]?[\p{Lu}\p{Lt}\p{Lm}\p{Lo}\p{M}]*[\p{Ll}\p{Lm}\p{Lo}\p{M}]+(?i:'s|'t|'re|'ve|'m|'ll|'d)?|",
    r"[^\r\n\p{L}\p{N}]?[\p{Lu}\p{Lt}\p{Lm}\p{Lo}\p{M}]+[\p{Ll}\p{Lm}\p{Lo}\p{M}]*(?i:'s|'t|'re|'ve|'m|'ll|'d)?|",
    r"\p{N}{1,3}| ?[^\s\p{L}\p{N}]+[\r\n/]*|\s*[\r\n]+|\s+",
);
