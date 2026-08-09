//! Shared machinery for the `quality` and `scaling` benchmarks.
//!
//! Both benchmarks are separate binaries that include this tree with
//! `#[path]`, so each one compiles items the other uses and nothing else
//! would suppress the resulting warnings.
#![allow(dead_code)]

pub mod corpus;
pub mod metrics;
pub mod stats;
pub mod truth;

/// rkgrep's own token counter, included from `src/` rather than copied.
///
/// rkgrep builds only a binary target, so a benchmark cannot link against it.
/// Including the module keeps both sides of every comparison charged by the
/// same rule -- the whole point of the cost column.
/// The benchmarks bring their own harness, so `#[test]` functions are compiled
/// but never collected: the module's own test imports resolve to nothing here.
#[allow(unused_imports)]
#[path = "../../src/tokenizer/mod.rs"]
pub mod tokenizer;
