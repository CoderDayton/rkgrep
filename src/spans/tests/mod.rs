//! Unit tests for declaration extraction and masking, one file per module.

mod body;
mod mask;
mod scan;
mod words;

use super::declarations;

/// The names a source declares, in file order — what most of these tests are
/// really asserting about.
fn names(src: &str) -> Vec<String> {
    declarations(src).into_iter().map(|d| d.name).collect()
}
