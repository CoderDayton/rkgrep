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

/// The pattern itself is `build.rs`'s input, not the binary's -- the binary
/// carries the automaton built from it. The tests read it to check the two
/// against the published one.
#[cfg(test)]
mod pattern;
mod table;

use std::cell::RefCell;
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::sync::LazyLock;

use regex_automata::dfa::{dense, Automaton};
use regex_automata::util::primitives::StateID;
use regex_automata::{Anchored, Input};

/// The table `build.rs` wrote, embedded.
#[repr(C, align(8))]
struct Aligned<T: ?Sized>(T);

static IMAGE: &Aligned<[u8]> =
    &Aligned(*include_bytes!(concat!(env!("OUT_DIR"), "/o200k_base.bin")));

/// The automaton `build.rs` determinized from `pattern::SPLIT_PATTERN`,
/// embedded.
static DFA_IMAGE: &Aligned<[u8]> =
    &Aligned(*include_bytes!(concat!(env!("OUT_DIR"), "/split_dfa.bin")));

/// The published pattern, kept beside ours so the difference between the two
/// is one visible alternative rather than a claim in a comment. Only the tests
/// compile it, and only a backtracking engine can.
#[cfg(test)]
const SPLIT_PATTERN_REFERENCE: &str = concat!(
    r"[^\r\n\p{L}\p{N}]?[\p{Lu}\p{Lt}\p{Lm}\p{Lo}\p{M}]*[\p{Ll}\p{Lm}\p{Lo}\p{M}]+(?i:'s|'t|'re|'ve|'m|'ll|'d)?|",
    r"[^\r\n\p{L}\p{N}]?[\p{Lu}\p{Lt}\p{Lm}\p{Lo}\p{M}]+[\p{Ll}\p{Lm}\p{Lo}\p{M}]*(?i:'s|'t|'re|'ve|'m|'ll|'d)?|",
    r"\p{N}{1,3}| ?[^\s\p{L}\p{N}]+[\r\n/]*|\s*[\r\n]+|\s+(?!\S)|\s+",
);

/// The splitter, read out of [`DFA_IMAGE`].
static SPLITTER: LazyLock<dense::DFA<&'static [u32]>> = LazyLock::new(|| {
    let (dfa, read) = dense::DFA::from_bytes(&DFA_IMAGE.0)
        .expect("the embedded automaton is this target's, built by build.rs");
    assert_eq!(read, DFA_IMAGE.0.len(), "the embedded automaton is padded");
    dfa
});

/// The state every piece starts in.
static ANCHORED_START: LazyLock<StateID> = LazyLock::new(|| {
    let input = Input::new("").anchored(Anchored::Yes);
    SPLITTER
        .start_state_forward(&input)
        .expect("an anchored DFA has an anchored start state")
});

thread_local! {
    /// The merge working set, one per thread, reused across pieces so a text
    /// of a thousand pieces allocates once.
    static SCRATCH: RefCell<Merges> = const { RefCell::new(Merges::new()) };
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
    count_capped(text, usize::MAX)
}

/// The number of `o200k_base` tokens in `text`, or `limit` if there are more.
pub fn count_capped(text: &str, limit: usize) -> usize {
    let vocab = &*VOCAB;
    SCRATCH.with_borrow_mut(|work| {
        let mut total = 0;
        split(text, |piece| {
            total += merge_count(piece.as_bytes(), vocab, work);
            total < limit
        });
        total.min(limit)
    })
}

/// Call `emit` with each piece of `text`, in order, until it returns `false`.
fn split<'t>(text: &'t str, mut emit: impl FnMut(&'t str) -> bool) {
    let splitter = &*SPLITTER;
    let start = *ANCHORED_START;
    let mut cursor = 0;
    while cursor < text.len() {
        let Some(found_end) = piece_end(splitter, start, text.as_bytes(), cursor) else {
            let _ = emit(&text[cursor..]);
            return;
        };
        let piece = &text[cursor..found_end];
        if piece.is_empty() {
            let Some(next) = text[cursor..].chars().next() else {
                return;
            };
            if !emit(&text[cursor..cursor + next.len_utf8()]) {
                return;
            }
            cursor += next.len_utf8();
            continue;
        }

        let blanks = starts_blank(piece)
            && piece
                .chars()
                .all(|c| c.is_whitespace() && c != '\r' && c != '\n');
        let last = if blanks {
            piece.chars().next_back().map_or(0, char::len_utf8)
        } else {
            0
        };
        if blanks && piece.len() > last && found_end < text.len() {
            if !emit(&piece[..piece.len() - last]) {
                return;
            }
            cursor = found_end - last;
        } else {
            if !emit(piece) {
                return;
            }
            cursor = found_end;
        }
    }
}

/// Whether `piece` opens with a blank -- whitespace that is not a line break.
#[inline]
fn starts_blank(piece: &str) -> bool {
    match piece.as_bytes().first() {
        Some(&byte) if byte.is_ascii() => matches!(byte, b'\t' | 0x0b | 0x0c | b' '),
        Some(_) => piece.chars().next().is_some_and(char::is_whitespace),
        None => false,
    }
}

/// Where the piece starting at `at` ends, or `None` if none starts there.
#[inline]
fn piece_end(
    dfa: &dense::DFA<&'static [u32]>,
    start: StateID,
    haystack: &[u8],
    at: usize,
) -> Option<usize> {
    let mut state = start;
    let mut last = None;
    let mut cursor = at;
    while cursor < haystack.len() {
        state = dfa.next_state(state, haystack[cursor]);
        cursor += 1;
        if dfa.is_special_state(state) {
            if dfa.is_match_state(state) {
                last = Some(cursor - 1);
            } else if dfa.is_dead_state(state) || dfa.is_quit_state(state) {
                return last;
            }
        }
    }
    if dfa.is_match_state(dfa.next_eoi_state(state)) {
        last = Some(haystack.len());
    }
    last
}

/// The working set for one piece's merge.
///
/// A boundary is a byte offset into the piece. They are linked, so dropping one
/// costs nothing but two writes, and queued by rank, so the next merge is a pop
/// rather than a scan of every boundary still standing. Together those make a
/// piece cost `n log n` instead of `n²`, which is what keeps one long
/// unbroken run of letters — a minified bundle, a base64 blob — from stalling
/// a whole query.
struct Merges {
    /// The next surviving boundary after each one, or [`Merges::NONE`].
    next: Vec<u32>,
    /// The previous surviving boundary, or [`Merges::NONE`].
    prev: Vec<u32>,
    /// The rank of the pair each boundary would form with the one after it.
    rank: Vec<u32>,
    /// Merges still to consider, lowest rank first and ties to the leftmost.
    /// An entry is left behind rather than removed when a rank changes, so a
    /// pop counts only when it still agrees with [`Merges::rank`].
    queue: BinaryHeap<Reverse<(u32, u32)>>,
}

impl Merges {
    /// No such boundary: the ends of the piece, and everything dropped.
    const NONE: u32 = u32::MAX;

    const fn new() -> Self {
        Self {
            next: Vec::new(),
            prev: Vec::new(),
            rank: Vec::new(),
            queue: BinaryHeap::new(),
        }
    }

    /// One boundary per byte of a piece of `len`, plus one past the end.
    fn reset(&mut self, len: usize) {
        self.next.clear();
        self.prev.clear();
        self.rank.clear();
        self.queue.clear();
        self.next.extend((0..=len).map(|at| match at == len {
            true => Self::NONE,
            false => at as u32 + 1,
        }));
        self.prev.extend((0..=len).map(|at| match at {
            0 => Self::NONE,
            _ => at as u32 - 1,
        }));
        self.rank.resize(len + 1, u32::MAX);
    }

    /// The rank of the pair `at` opens, or [`u32::MAX`] if it opens none.
    fn pair_rank(&self, piece: &[u8], vocab: &Vocabulary, at: u32) -> u32 {
        let middle = self.next[at as usize];
        if middle == Self::NONE {
            return u32::MAX;
        }
        let end = self.next[middle as usize];
        if end == Self::NONE {
            return u32::MAX;
        }
        vocab
            .rank(&piece[at as usize..end as usize])
            .unwrap_or(u32::MAX)
    }

    /// Record what `at` is worth now, queueing it while a merge is possible.
    fn set_rank(&mut self, at: u32, rank: u32) {
        self.rank[at as usize] = rank;
        if rank != u32::MAX {
            self.queue.push(Reverse((rank, at)));
        }
    }

    /// Drop the boundary after `at`, joining the two tokens it separated.
    fn unlink_after(&mut self, at: u32) {
        let dropped = self.next[at as usize];
        let after = self.next[dropped as usize];
        self.next[at as usize] = after;
        if after != Self::NONE {
            self.prev[after as usize] = at;
        }
        self.rank[dropped as usize] = u32::MAX;
        self.next[dropped as usize] = Self::NONE;
    }
}

/// Tokens `piece` merges into.
///
/// The lowest-ranked pair merges first and ties go to the leftmost, which is
/// what makes the count tiktoken's for the same input.
fn merge_count(piece: &[u8], vocab: &Vocabulary, work: &mut Merges) -> usize {
    if piece.len() <= 1 || vocab.rank(piece).is_some() {
        return 1;
    }
    work.reset(piece.len());
    for at in 0..=piece.len() as u32 {
        let rank = work.pair_rank(piece, vocab, at);
        work.set_rank(at, rank);
    }

    let mut boundaries = piece.len() + 1;
    while boundaries > 1 {
        let Some(Reverse((rank, at))) = work.queue.pop() else {
            break;
        };
        // Stale: `at`'s pair changed, or `at` was itself dropped, after this
        // entry was queued. Either way a live entry for it is queued too.
        if work.rank[at as usize] != rank {
            continue;
        }
        work.unlink_after(at);
        boundaries -= 1;

        let rank = work.pair_rank(piece, vocab, at);
        work.set_rank(at, rank);
        let before = work.prev[at as usize];
        if before != Merges::NONE {
            let rank = work.pair_rank(piece, vocab, before);
            work.set_rank(before, rank);
        }
    }
    boundaries - 1
}

/// Read one byte from every page of the embedded images.
fn fault_in() {
    const PAGE_BYTES: usize = 4096;
    let vocab = &*VOCAB;
    let mut seen = 0u64;
    for slot in vocab.slots.iter().step_by(PAGE_BYTES / size_of::<u64>()) {
        seen ^= *slot;
    }
    for byte in vocab.blob.iter().step_by(PAGE_BYTES) {
        seen ^= u64::from(*byte);
    }
    for byte in DFA_IMAGE.0.iter().step_by(PAGE_BYTES) {
        seen ^= u64::from(*byte);
    }
    std::hint::black_box(seen);
}

/// Prepare the counter off the critical path.
pub fn prewarm() {
    std::thread::spawn(|| LazyLock::force(&SPLITTER));
    std::thread::spawn(fault_in);
}

#[cfg(test)]
mod tests;
