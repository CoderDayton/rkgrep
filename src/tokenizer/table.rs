//! The on-disk shape of the pre-built vocabulary table.
//!
//! `build.rs` includes this file textually and the crate compiles it as a
//! module, so both sides derive the same hash and the same bit layout from one
//! definition. Nothing here may reference the crate or any dependency: the
//! build script has neither.
//!
//! The image is three regions laid out back to back, each aligned by
//! construction:
//!
//! ```text
//! 0                      32                    32 + 8·slots         end
//! | header (32 bytes)     | slots (u64 each)    | token bytes        |
//! ```
//!
//! A slot is one `u64` rather than an index into a side table, so a probe
//! touches one cache line and resolves a miss without following a pointer:
//!
//! ```text
//!  63      56 55      48 47                  24 23                   0
//! |   tag   |   len    |        offset        |        rank         |
//! ```
//!
//! `tag` is the hash's top byte, compared before the token bytes are read at
//! all. `offset`/`len` locate the token in the blob; `rank` is its BPE rank.
//! [`EMPTY`] is all ones, which no real slot can be because the highest rank in
//! `o200k_base` is 199,997 and the rank field would have to hold 16,777,215.

//! Half of what follows is the writing side, which only `build.rs` calls; the
//! binary compiles the same file to read what that side wrote.
#![allow(dead_code)]

/// Identifies the image and the layout it was written with.
pub const MAGIC: u32 = 0x524b_544b;

/// Bumped whenever the layout below changes, so a stale `OUT_DIR` image is
/// rejected rather than misread.
pub const VERSION: u32 = 1;

/// magic, version, slot count, entry count, blob length, and padding to 8.
pub const HEADER_BYTES: usize = 32;

/// An unoccupied slot.
pub const EMPTY: u64 = u64::MAX;

/// Widest token the layout can hold, from the `len` field.
pub const MAX_TOKEN_LEN: usize = 0xff;

/// Largest blob the layout can address, from the `offset` field.
pub const MAX_BLOB_LEN: usize = 0x00ff_ffff;

/// Largest rank the layout can hold. One below the field's maximum, which is
/// reserved so [`EMPTY`] cannot collide with a real slot.
pub const MAX_RANK: u32 = 0x00ff_fffe;

/// Occupancy the table is built to. Linear probing degrades sharply past ~0.8;
/// at 0.5 the average successful lookup touches ~1.5 slots, and the table costs
/// 4 MiB of the binary either way.
pub const LOAD_NUMERATOR: usize = 1;
pub const LOAD_DENOMINATOR: usize = 2;

/// Multiplier from `xxHash`'s 64-bit primes, chosen for avalanche rather than
/// for speed: the low bits pick the slot and the top byte becomes the tag, so
/// the two must be independent.
const MIX: u64 = 0x9e37_79b1_85eb_ca87;

/// Hash of a token's bytes.
pub fn hash_token(key: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut chunks = key.chunks_exact(8);
    for chunk in &mut chunks {
        let word = u64::from_le_bytes(chunk.try_into().expect("chunks_exact(8) yields 8 bytes"));
        h = (h.rotate_left(23) ^ word).wrapping_mul(MIX);
    }
    let tail = chunks.remainder();
    if !tail.is_empty() {
        let mut padded = [0u8; 8];
        padded[..tail.len()].copy_from_slice(tail);
        h = (h.rotate_left(23) ^ u64::from_le_bytes(padded)).wrapping_mul(MIX);
    }
    h = (h.rotate_left(23) ^ key.len() as u64).wrapping_mul(MIX);
    h ^ (h >> 29)
}

/// Pack one entry. `tag` comes from [`hash_token`]'s top byte.
pub const fn pack_slot(hash: u64, len: usize, offset: usize, rank: u32) -> u64 {
    (hash & 0xff00_0000_0000_0000)
        | ((len as u64) << 48)
        | ((offset as u64) << 24)
        | (rank as u64 & 0x00ff_ffff)
}

/// The byte a probe compares before touching the blob.
pub const fn slot_tag(slot: u64) -> u8 {
    (slot >> 56) as u8
}

pub const fn slot_len(slot: u64) -> usize {
    ((slot >> 48) & 0xff) as usize
}

pub const fn slot_offset(slot: u64) -> usize {
    ((slot >> 24) & 0x00ff_ffff) as usize
}

pub const fn slot_rank(slot: u64) -> u32 {
    (slot & 0x00ff_ffff) as u32
}

/// The tag a probe compares, taken from the same top byte [`pack_slot`] keeps.
pub const fn hash_tag(hash: u64) -> u8 {
    (hash >> 56) as u8
}

/// Smallest power of two that holds `entries` at the load factor above.
pub fn slot_count_for(entries: usize) -> usize {
    let wanted = entries * LOAD_DENOMINATOR / LOAD_NUMERATOR;
    wanted.next_power_of_two()
}
