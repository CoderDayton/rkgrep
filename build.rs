//! Turns the `o200k_base` vocabulary into the table the tokenizer memory-maps.
//!
//! Doing this here rather than at startup is the whole point: the released
//! binary carries a finished hash table, and the first token count costs a
//! bounds check and a load instead of decoding 200k base64 lines and building
//! a `HashMap`.

use std::io::Write;
use std::path::{Path, PathBuf};

use base64::Engine;

#[path = "src/tokenizer/table.rs"]
mod table;

/// The published `<base64-token> <rank>` ranks.
const VOCAB: &str = "src/o200k_base.tiktoken";

/// Where the built image lands, under `OUT_DIR`.
const IMAGE: &str = "o200k_base.bin";

fn main() {
    println!("cargo:rerun-if-changed={VOCAB}");
    println!("cargo:rerun-if-changed=src/tokenizer/table.rs");
    println!("cargo:rerun-if-changed=build.rs");

    let vocab = std::fs::read(VOCAB).unwrap_or_else(|e| panic!("reading {VOCAB}: {e}"));
    let entries = parse(&vocab);
    let image = build(&entries);

    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR")).join(IMAGE);
    write(&out, &image).unwrap_or_else(|e| panic!("writing {}: {e}", out.display()));
}

/// Every `<base64-token> <rank>` line, decoded.
///
/// Ordered by rank rather than by input order so the blob's layout is
/// deterministic and low-rank tokens — the common ones, and the only ones a
/// merge ever looks up repeatedly — sit together at the front of it.
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
        // Linear probing, inserting into the first free slot. The vocabulary
        // has no duplicate tokens, so no slot is ever overwritten.
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
