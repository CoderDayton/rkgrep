//! The tables the rules are looked up in, and identifier splitting.

use crate::spans::identifier_tokens;
use crate::spans::words::{CONTROL_WORDS, KEYWORDS, MODIFIERS, PREPROCESSOR_DIRECTIVES};

#[test]
fn tables_are_sorted_for_binary_search() {
    for table in [KEYWORDS, MODIFIERS, CONTROL_WORDS, PREPROCESSOR_DIRECTIVES] {
        assert!(table.windows(2).all(|w| w[0] < w[1]), "{table:?}");
    }
}

#[test]
fn identifier_tokens_split_every_common_case() {
    assert_eq!(identifier_tokens("validateToken"), ["validate", "token"]);
    assert_eq!(identifier_tokens("validate_token"), ["validate", "token"]);
    assert_eq!(identifier_tokens("ValidateToken"), ["validate", "token"]);
    assert_eq!(identifier_tokens("HTTPServer"), ["http", "server"]);
    assert_eq!(identifier_tokens("parse2Json"), ["parse", "2", "json"]);
}
