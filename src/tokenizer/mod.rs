//! `o200k_base` token counting against a table built at compile time.
//!
//! `--max-tokens` is denominated in the tokens a context window charges, so the
//! counts here are byte-identical to `OpenAI`'s tiktoken for the same input.
//! What differs is when the work happens: [`build.rs`](../../build.rs) decodes
//! the published ranks and lays out the probe table, and this module reads that
//! image straight out of the binary. Nothing is parsed, allocated or hashed at
//! startup.
//!
//! Counting is the two halves tiktoken uses:
//!
//! 1. [`split`] cuts the text into pieces at the boundaries `o200k_base`
//!    defines — no token ever spans two pieces
//! 2. [`merge_count`] byte-pair-merges each piece against the table
//!
//! Only the count is produced, never the token ids, which is why the table
//! carries no rank-to-bytes direction: decoding is the half rkgrep never does.

mod table;

use std::cell::RefCell;
use std::sync::LazyLock;

use regex_automata::meta::{Cache, Regex};
use regex_automata::Input;

/// The table `build.rs` wrote, embedded.
///
/// `include_bytes!` yields a `[u8]` with no alignment guarantee, and the slots
/// are read as `u64`. Wrapping the bytes in an 8-aligned type moves that
/// guarantee to compile time, where it costs nothing, instead of copying the
/// image into an aligned buffer at startup.
#[repr(C, align(8))]
struct Aligned<T: ?Sized>(T);

static IMAGE: &Aligned<[u8]> =
    &Aligned(*include_bytes!(concat!(env!("OUT_DIR"), "/o200k_base.bin")));

/// The pattern `o200k_base` splits on, with the one alternative the `regex`
/// crate cannot express removed.
///
/// The original's third whitespace alternative is `\s+(?!\S)`, and negative
/// lookahead is exactly what `regex` trades away for linear-time matching.
/// Dropping it here and applying its rule in [`split`] keeps the linear-time
/// engine — measured five times faster than the backtracking one that accepts
/// the lookahead directly.
const SPLIT_PATTERN: &str = concat!(
    r"[^\r\n\p{L}\p{N}]?[\p{Lu}\p{Lt}\p{Lm}\p{Lo}\p{M}]*[\p{Ll}\p{Lm}\p{Lo}\p{M}]+(?i:'s|'t|'re|'ve|'m|'ll|'d)?|",
    r"[^\r\n\p{L}\p{N}]?[\p{Lu}\p{Lt}\p{Lm}\p{Lo}\p{M}]+[\p{Ll}\p{Lm}\p{Lo}\p{M}]*(?i:'s|'t|'re|'ve|'m|'ll|'d)?|",
    r"\p{N}{1,3}| ?[^\s\p{L}\p{N}]+[\r\n/]*|\s*[\r\n]+|\s+",
);

/// The published pattern, kept beside ours so the difference between the two
/// is one visible alternative rather than a claim in a comment. Only the tests
/// compile it, and only a backtracking engine can.
#[cfg(test)]
const SPLIT_PATTERN_REFERENCE: &str = concat!(
    r"[^\r\n\p{L}\p{N}]?[\p{Lu}\p{Lt}\p{Lm}\p{Lo}\p{M}]*[\p{Ll}\p{Lm}\p{Lo}\p{M}]+(?i:'s|'t|'re|'ve|'m|'ll|'d)?|",
    r"[^\r\n\p{L}\p{N}]?[\p{Lu}\p{Lt}\p{Lm}\p{Lo}\p{M}]+[\p{Ll}\p{Lm}\p{Lo}\p{M}]*(?i:'s|'t|'re|'ve|'m|'ll|'d)?|",
    r"\p{N}{1,3}| ?[^\s\p{L}\p{N}]+[\r\n/]*|\s*[\r\n]+|\s+(?!\S)|\s+",
);

/// The compiled splitter. The only startup cost left, and the reason
/// [`prewarm`] still exists.
static SPLITTER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(SPLIT_PATTERN).expect("the split pattern is a literal constant"));

thread_local! {
    /// The engine's mutable scratch, and the merge buffer, one set per thread.
    ///
    /// `regex`'s own `Regex::find_iter` draws scratch from a pool that is
    /// lock-free only for the thread that created it, and every other thread
    /// takes a mutex ([rust-lang/regex#934]). Extraction runs a batch of files
    /// across threads and counts a span at a time, so that pool would be
    /// contended on every span. Owning the scratch here also removes the
    /// per-call setup, which measured 2.5x the cost of the counting itself.
    ///
    /// [rust-lang/regex#934]: https://github.com/rust-lang/regex/issues/934
    static SCRATCH: RefCell<(Cache, Vec<Part>)> =
        RefCell::new((SPLITTER.create_cache(), Vec::new()));
}

/// The probe table, resolved from [`IMAGE`] once.
static VOCAB: LazyLock<Vocabulary> = LazyLock::new(Vocabulary::load);

/// A token's rank, looked up by its bytes.
struct Vocabulary {
    /// One packed entry per slot; see [`table`].
    slots: &'static [u64],
    /// Every token's bytes, concatenated.
    blob: &'static [u8],
    /// `slots.len() - 1`, so a hash becomes an index with an `and`.
    mask: usize,
}

impl Vocabulary {
    /// Resolve the embedded image into slices.
    ///
    /// The header is checked rather than trusted: a stale `OUT_DIR` image from
    /// an older layout has to fail loudly here, not silently return wrong
    /// counts. Everything the check passes is then a borrow of static bytes.
    fn load() -> Self {
        let bytes = &IMAGE.0;
        let word = |at: usize| {
            u32::from_le_bytes(
                bytes[at..at + 4]
                    .try_into()
                    .expect("a 4-byte window is 4 bytes"),
            ) as usize
        };
        assert!(
            bytes.len() >= table::HEADER_BYTES,
            "vocabulary image is cut short"
        );
        assert_eq!(
            word(0),
            table::MAGIC as usize,
            "vocabulary image is not one"
        );
        assert_eq!(
            word(4),
            table::VERSION as usize,
            "vocabulary image is stale"
        );

        let slot_count = word(8);
        let blob_len = word(16);
        assert!(
            slot_count.is_power_of_two(),
            "slot count must be a power of two"
        );
        let slots_end = table::HEADER_BYTES + slot_count * 8;
        assert_eq!(
            bytes.len(),
            slots_end + blob_len,
            "vocabulary image is the wrong length"
        );

        // SAFETY: `IMAGE` is 8-aligned by `Aligned`, `HEADER_BYTES` is a
        // multiple of 8 so the slot region is too, and the length was just
        // checked to cover exactly `slot_count` slots. `u64` has no invalid bit
        // patterns, so every readable byte is a valid value.
        let slots = unsafe {
            std::slice::from_raw_parts(
                bytes.as_ptr().add(table::HEADER_BYTES).cast::<u64>(),
                slot_count,
            )
        };
        Self {
            slots,
            blob: &bytes[slots_end..],
            mask: slot_count - 1,
        }
    }

    /// The rank of `token`, or `None` if the vocabulary does not have it.
    ///
    /// Linear probing over one-word slots: a miss on the tag byte rejects a
    /// slot without reading the token bytes at all, so a probe run stays inside
    /// the cache lines it already pulled in and only a tag collision — one in
    /// 256 — pays for a comparison against the blob.
    #[inline]
    fn rank(&self, token: &[u8]) -> Option<u32> {
        let hash = table::hash_token(token);
        let tag = table::hash_tag(hash);
        let mut at = (hash as usize) & self.mask;
        loop {
            let slot = self.slots[at];
            if slot == table::EMPTY {
                return None;
            }
            if table::slot_tag(slot) == tag && table::slot_len(slot) == token.len() {
                let start = table::slot_offset(slot);
                if &self.blob[start..start + token.len()] == token {
                    return Some(table::slot_rank(slot));
                }
            }
            at = (at + 1) & self.mask;
        }
    }
}

/// The number of `o200k_base` tokens in `text`.
pub fn count(text: &str) -> usize {
    let vocab = &*VOCAB;
    SCRATCH.with_borrow_mut(|(cache, parts)| {
        let mut total = 0;
        split(text, cache, |piece| {
            total += merge_count(piece.as_bytes(), vocab, parts);
        });
        total
    })
}

/// Call `emit` with each piece of `text`, in order.
///
/// The pieces are exactly the ones `o200k_base`'s pattern produces. Everything
/// but its `\s+(?!\S)` alternative comes from [`SPLITTER`]; that one is applied
/// here, to the only match it can ever disagree with.
fn split(text: &str, cache: &mut Cache, mut emit: impl FnMut(&str)) {
    let splitter = &*SPLITTER;
    let mut cursor = 0;
    while cursor < text.len() {
        let Some(found) = splitter.search_with(cache, &Input::new(text).range(cursor..)) else {
            // Nothing matches in what is left: it is not a piece, but its
            // bytes still cost tokens.
            emit(&text[cursor..]);
            return;
        };
        // The pattern matches wherever a piece can start, so a gap means the
        // text held something no alternative accepts; pass it through rather
        // than dropping bytes out of the count.
        if found.start() > cursor {
            emit(&text[cursor..found.start()]);
        }
        let piece = &text[found.start()..found.end()];
        if piece.is_empty() {
            // An empty match cannot advance the cursor; step a character past
            // it so the loop always terminates. Stepping from the match rather
            // than from `cursor` keeps any gap already emitted above from
            // being emitted a second time.
            let at = found.start();
            let Some(next) = text[at..].chars().next() else {
                return;
            };
            emit(&text[at..at + next.len_utf8()]);
            cursor = at + next.len_utf8();
            continue;
        }

        // `\s+(?!\S)` sits ahead of the bare `\s+` and takes a run of blanks
        // one character short unless the run ends the text -- the character it
        // leaves behind opens the following piece as its optional prefix. A run
        // carrying a line break was already claimed by `\s*[\r\n]+`, and no
        // other alternative matches blanks alone.
        let blanks = piece
            .chars()
            .all(|c| c.is_whitespace() && c != '\r' && c != '\n');
        let last = piece.chars().next_back().map_or(0, char::len_utf8);
        if blanks && piece.len() > last && found.end() < text.len() {
            emit(&piece[..piece.len() - last]);
            cursor = found.end() - last;
        } else {
            emit(piece);
            cursor = found.end();
        }
    }
}

/// One boundary in the piece being merged: where it starts, and the rank of the
/// pair that would form if it merged with the boundary after it.
///
/// Two `u32`s rather than `usize`s so a pair is eight bytes and the scan for
/// the lowest rank walks half as much memory. A piece is at most a few dozen
/// bytes, so the narrower index cannot overflow.
#[derive(Clone, Copy)]
struct Part {
    start: u32,
    rank: u32,
}

/// Tokens `piece` merges into.
///
/// tiktoken's byte-pair merge, counting only: boundaries start between every
/// byte and the lowest-ranked adjacent pair merges until no pair is in the
/// vocabulary. `parts` is the caller's scratch buffer, reused across pieces so
/// a text of a thousand pieces allocates once.
fn merge_count(piece: &[u8], vocab: &Vocabulary, parts: &mut Vec<Part>) -> usize {
    // Nearly every piece is a whole token -- that is what a vocabulary is for
    // -- and answering those with one probe skips the merge entirely.
    if piece.len() <= 1 || vocab.rank(piece).is_some() {
        return 1;
    }

    parts.clear();
    parts.reserve(piece.len() + 1);
    for start in 0..=piece.len() {
        parts.push(Part {
            start: start as u32,
            rank: u32::MAX,
        });
    }

    // The pair opening at boundary `i` runs to boundary `i + 2` -- one token on
    // each side -- and is absent once `i + 2` is past the end sentinel.
    let pair_rank = |parts: &[Part], i: usize| -> u32 {
        parts
            .get(i + 2)
            .and_then(|end| vocab.rank(&piece[parts[i].start as usize..end.start as usize]))
            .unwrap_or(u32::MAX)
    };
    for i in 0..parts.len().saturating_sub(2) {
        parts[i].rank = pair_rank(parts, i);
    }

    loop {
        if parts.len() == 1 {
            break;
        }
        let mut best = u32::MAX;
        let mut best_at = 0;
        for (i, part) in parts[..parts.len() - 1].iter().enumerate() {
            if part.rank < best {
                best = part.rank;
                best_at = i;
            }
        }
        if best == u32::MAX {
            break;
        }
        // Merging at `best_at` removes the boundary after it, which changes the
        // pair opening there and the one opening at the boundary before it.
        parts.remove(best_at + 1);
        parts[best_at].rank = pair_rank(parts, best_at);
        if best_at > 0 {
            parts[best_at - 1].rank = pair_rank(parts, best_at - 1);
        }
    }
    parts.len() - 1
}

/// Compile the splitter off the critical path.
///
/// The table needs no preparation, so this is the whole of startup: one regex,
/// built on a background thread while the parallel walk runs, so the serial
/// extract phase never waits for it. Idempotent -- a racing caller blocks until
/// the same regex is ready.
pub fn prewarm() {
    std::thread::spawn(|| {
        LazyLock::force(&SPLITTER);
        LazyLock::force(&VOCAB);
    });
}

#[cfg(test)]
mod tests;
