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
