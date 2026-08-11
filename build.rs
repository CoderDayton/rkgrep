//! Turns the `o200k_base` vocabulary and split pattern into the images the
//! tokenizer reads straight out of the binary.
//!
//! Doing this here rather than at startup is the whole point: the released
//! binary carries a finished hash table and a finished automaton, and the
//! first token count costs a bounds check and a load instead of decoding 200k
//! base64 lines, building a `HashMap`, and determinizing a unicode pattern.

use std::io::Write;
use std::path::{Path, PathBuf};

use base64::Engine;
use regex_automata::dfa::{dense, Automaton};
use regex_automata::nfa::thompson;
use regex_automata::util::start::Config as StartConfig;
use regex_automata::{Anchored, MatchKind};

#[path = "src/tokenizer/table.rs"]
mod table;

#[path = "src/tokenizer/pattern.rs"]
mod pattern;

/// The published `<base64-token> <rank>` ranks.
const VOCAB: &str = "src/tokenizer/o200k_base.tiktoken";

/// Where the built vocabulary image lands, under `OUT_DIR`.
const IMAGE: &str = "o200k_base.bin";

/// Where the built automaton lands, under `OUT_DIR`.
const DFA_IMAGE: &str = "split_dfa.bin";

fn main() {
    println!("cargo:rerun-if-changed={VOCAB}");
    println!("cargo:rerun-if-changed=src/tokenizer/table.rs");
    println!("cargo:rerun-if-changed=src/tokenizer/pattern.rs");
    println!("cargo:rerun-if-changed=build.rs");

    let vocab = std::fs::read(VOCAB).unwrap_or_else(|e| panic!("reading {VOCAB}: {e}"));
    let entries = parse(&vocab);
    let image = build(&entries);

    let dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    let out = dir.join(IMAGE);
    write(&out, &image).unwrap_or_else(|e| panic!("writing {}: {e}", out.display()));

    let out = dir.join(DFA_IMAGE);
    write(&out, &split_dfa()).unwrap_or_else(|e| panic!("writing {}: {e}", out.display()));
}

/// The split pattern, determinized and serialized.
///
/// Anchored only: `split` asks whether a piece starts at the cursor, never
/// where the next one begins, and dropping the unanchored start states takes
/// the image down with them. Leftmost-first because the pattern's alternatives
/// are ordered -- the first one that matches is the piece, which is what
/// tiktoken's engine does.
///
/// The bytes are written in the *target's* endianness, not the host's, so a
/// cross-compiled binary reads its own image rather than panicking on every
/// count.
fn split_dfa() -> Vec<u8> {
    let dfa = dense::Builder::new()
        .configure(
            dense::Config::new()
                .match_kind(MatchKind::LeftmostFirst)
                .start_kind(regex_automata::dfa::StartKind::Anchored),
        )
        .thompson(thompson::Config::new().shrink(true))
        .build(pattern::SPLIT_PATTERN)
        .expect("the split pattern is a literal constant");
    // A DFA that cannot start anchored would search every position instead of
    // the cursor, which is a correctness difference, not a slow path.
    dfa.start_state(&StartConfig::new().anchored(Anchored::Yes))
        .expect("the anchored start state is the one this DFA was built for");

    let target_endian =
        std::env::var("CARGO_CFG_TARGET_ENDIAN").expect("cargo sets CARGO_CFG_TARGET_ENDIAN");
    let (bytes, padding) = match target_endian.as_str() {
        "big" => dfa.to_bytes_big_endian(),
        "little" => dfa.to_bytes_little_endian(),
        other => panic!("unknown target endianness {other}"),
    };
    bytes[padding..].to_vec()
}

/// Every `<base64-token> <rank>` line, decoded.
fn parse(vocab: &[u8]) -> Vec<(Vec<u8>, u32)> {
    let mut entries: Vec<(Vec<u8>, u32)> = Vec::with_capacity(200_000);
    for (n, line) in vocab.split(|&b| b == b'\n').enumerate() {
        if line.is_empty() {
            continue;
        }
        let mut cols = line.splitn(2, |&b| b == b' ');
        let (Some(token_b64), Some(rank_ascii)) = (cols.next(), cols.next()) else {
            panic!("{VOCAB}:{}: not `<base64-token> <rank>`", n + 1);
        };
        let token = base64::engine::general_purpose::STANDARD
            .decode(token_b64)
            .unwrap_or_else(|e| panic!("{VOCAB}:{}: bad base64: {e}", n + 1));
        let rank: u32 = std::str::from_utf8(rank_ascii)
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or_else(|| panic!("{VOCAB}:{}: bad rank", n + 1));

        assert!(
            !token.is_empty() && token.len() <= table::MAX_TOKEN_LEN,
            "{VOCAB}:{}: token of {} bytes does not fit the len field",
            n + 1,
            token.len()
        );
        assert!(
            rank <= table::MAX_RANK,
            "{VOCAB}:{}: rank {rank} does not fit the rank field",
            n + 1
        );
        entries.push((token, rank));
    }
    assert!(!entries.is_empty(), "{VOCAB} is empty");
    entries.sort_unstable_by_key(|(_, rank)| *rank);
    entries
}

/// Header, then the probe table, then the token bytes.
fn build(entries: &[(Vec<u8>, u32)]) -> Vec<u8> {
    let slot_count = table::slot_count_for(entries.len());
    let mask = slot_count - 1;

    let mut blob: Vec<u8> = Vec::with_capacity(1 << 21);
    let mut slots = vec![table::EMPTY; slot_count];

    for (token, rank) in entries {
        let offset = blob.len();
        assert!(
            offset + token.len() <= table::MAX_BLOB_LEN,
            "token blob outgrew the offset field"
        );
        blob.extend_from_slice(token);

        let hash = table::hash_token(token);
        let mut at = (hash as usize) & mask;
        while slots[at] != table::EMPTY {
            assert!(
                !token_at(&blob, slots[at]).eq(token.as_slice()),
                "duplicate token in {VOCAB}"
            );
            at = (at + 1) & mask;
        }
        slots[at] = table::pack_slot(hash, token.len(), offset, *rank);
    }

    let mut image = Vec::with_capacity(table::HEADER_BYTES + slots.len() * 8 + blob.len());
    image.extend_from_slice(&table::MAGIC.to_le_bytes());
    image.extend_from_slice(&table::VERSION.to_le_bytes());
    image.extend_from_slice(&(slot_count as u32).to_le_bytes());
    image.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    image.extend_from_slice(&(blob.len() as u32).to_le_bytes());
    image.extend_from_slice(&[0u8; 12]);
    debug_assert_eq!(image.len(), table::HEADER_BYTES);
    for slot in &slots {
        image.extend_from_slice(&slot.to_le_bytes());
    }
    image.extend_from_slice(&blob);
    image
}

/// The token a slot points at, for the duplicate check during insertion.
fn token_at(blob: &[u8], slot: u64) -> &[u8] {
    let at = table::slot_offset(slot);
    &blob[at..at + table::slot_len(slot)]
}

/// Write only when the bytes changed, so an unchanged vocabulary does not
/// restamp the file and relink every dependent target.
fn write(path: &Path, image: &[u8]) -> std::io::Result<()> {
    if std::fs::read(path).is_ok_and(|existing| existing == image) {
        return Ok(());
    }
    let mut file = std::fs::File::create(path)?;
    file.write_all(image)
}
