//! Which files the walk keeps, beyond the ignore rules and the globs.
//!
//! Both filters are answered from the path alone, before a file is opened, so
//! narrowing a query to a language or away from tests costs nothing per byte.

use std::ffi::OsStr;
use std::path::Path;

use super::Options;

/// The extensions each `--lang` name selects. Binary-searched, so the table
/// stays sorted by name; `language_table_is_sorted` enforces it.
const LANGUAGES: &[(&str, &[&str])] = &[
    ("c", &["c", "h"]),
    ("cpp", &["cc", "cpp", "cxx", "hh", "hpp", "hxx"]),
    ("cs", &["cs"]),
    ("css", &["css", "sass", "scss"]),
    ("elixir", &["ex", "exs"]),
    ("go", &["go"]),
    ("haskell", &["hs"]),
    ("html", &["htm", "html"]),
    ("java", &["java"]),
    ("js", &["cjs", "js", "jsx", "mjs"]),
    ("json", &["json"]),
    ("kotlin", &["kt", "kts"]),
    ("lua", &["lua"]),
    ("md", &["markdown", "md"]),
    ("php", &["php"]),
    ("py", &["py", "pyi"]),
    ("python", &["py", "pyi"]),
    ("r", &["r"]),
    ("rb", &["rb"]),
    ("rs", &["rs"]),
    ("ruby", &["rb"]),
    ("rust", &["rs"]),
    ("scala", &["sc", "scala"]),
    ("sh", &["bash", "sh", "zsh"]),
    ("sql", &["sql"]),
    ("swift", &["swift"]),
    ("toml", &["toml"]),
    ("ts", &["cts", "mts", "ts", "tsx"]),
    ("typescript", &["cts", "mts", "ts", "tsx"]),
    ("yaml", &["yaml", "yml"]),
    ("zig", &["zig"]),
];

/// Directory names that hold tests by convention. Binary-searched, so this
/// table stays sorted too.
const TEST_DIRS: &[&str] = &["__tests__", "spec", "specs", "test", "testing", "tests"];

/// Stem endings that name a test file. Each carries its own separator, so
/// `latest.py` is not one.
const TEST_ENDINGS: &[&str] = &[
    "-spec", "-specs", "-test", "-tests", ".spec", ".specs", ".test", ".tests", "_spec", "_specs",
    "_test", "_tests",
];

/// Stem beginnings, the same idea from the other end.
const TEST_STARTS: &[&str] = &["spec_", "test_"];

/// Endings on the stem as it was written, for the languages that name a test
/// after the thing it covers: `AuthTest.java`, `AuthTests.cs`.
const TEST_CAMEL: &[&str] = &["Spec", "Test", "Tests"];

/// Stems that are a test file whole, with nothing to separate.
const TEST_STEMS: &[&str] = &["spec", "test"];

/// What a run does with the test files it walks over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tests {
    #[default]
    Include,
    Exclude,
    Only,
}

/// The extensions `name` selects, or `None` if it names no language.
pub fn extensions_for(name: &str) -> Option<&'static [&'static str]> {
    LANGUAGES
        .binary_search_by_key(&name, |(language, _)| *language)
        .ok()
        .map(|at| LANGUAGES[at].1)
}

/// Every name `--lang` accepts, for the error a wrong one earns.
pub fn language_names() -> impl Iterator<Item = &'static str> {
    LANGUAGES.iter().map(|(language, _)| *language)
}

/// Whether a path names a test file, by the conventions above.
pub fn looks_like_test(path: &str) -> bool {
    let (dirs, name) = path.rsplit_once('/').unwrap_or(("", path));
    let in_test_dir = dirs.split('/').any(|dir| {
        TEST_DIRS
            .binary_search(&dir.to_ascii_lowercase().as_str())
            .is_ok()
    });
    if in_test_dir {
        return true;
    }
    // Only the final extension comes off, so `api.test.ts` still ends in
    // `.test` where `api.py` ends in nothing.
    let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
    let lowered = stem.to_ascii_lowercase();
    TEST_STEMS.contains(&lowered.as_str())
        || TEST_ENDINGS.iter().any(|end| lowered.ends_with(end))
        || TEST_STARTS.iter().any(|start| lowered.starts_with(start))
        || TEST_CAMEL.iter().any(|end| stem.ends_with(end))
}

/// The path-only filters one run applies to every file the walk offers it.
pub(super) struct Filter {
    /// Extensions to keep; empty keeps every extension.
    extensions: Vec<&'static str>,
    tests: Tests,
}

impl Filter {
    pub(super) fn for_run(opts: &Options) -> Self {
        Self {
            extensions: opts.extensions.clone(),
            tests: opts.tests,
        }
    }

    /// Whether a file is worth searching.
    ///
    /// Only the test filter needs the path relative to the root — the root's
    /// own directories are not the repository's — so it is passed as a closure
    /// and a run that does not filter tests never builds one.
    pub(super) fn keeps(&self, path: &Path, relative: impl FnOnce() -> String) -> bool {
        if !self.extensions.is_empty() {
            let extension = path
                .extension()
                .and_then(OsStr::to_str)
                .unwrap_or_default()
                .to_ascii_lowercase();
            if !self.extensions.contains(&extension.as_str()) {
                return false;
            }
        }
        match self.tests {
            Tests::Include => true,
            Tests::Exclude => !looks_like_test(&relative()),
            Tests::Only => looks_like_test(&relative()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_table_is_sorted() {
        assert!(LANGUAGES.windows(2).all(|pair| pair[0].0 < pair[1].0));
        assert!(TEST_DIRS.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn a_language_selects_every_extension_that_writes_it() {
        assert_eq!(extensions_for("py"), extensions_for("python"));
        assert!(extensions_for("ts").unwrap().contains(&"tsx"));
        assert_eq!(extensions_for("cobol"), None);
    }

    #[test]
    fn test_files_are_told_from_the_files_they_cover() {
        for path in [
            "tests/query_shaping.rs",
            "src/__tests__/api.js",
            "spec/models/user_spec.rb",
            "auth_test.go",
            "test_auth.py",
            "src/api.test.ts",
            "src/AuthTest.java",
            "src/AuthTests.cs",
            "test.py",
        ] {
            assert!(looks_like_test(path), "{path:?}");
        }
        for path in [
            "src/latest.py",
            "src/auth.py",
            "src/contest.rs",
            "src/protest/greatest.go",
        ] {
            assert!(!looks_like_test(path), "{path:?}");
        }
    }
}
