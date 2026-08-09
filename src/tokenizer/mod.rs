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
use std::sync::LazyLock;

use regex_automata::dfa::{dense, Automaton};
use regex_automata::util::primitives::StateID;
use regex_automata::{Anchored, Input};

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
///
/// A finished transition table, shared read-only by every thread: no lazy
/// state building, no per-thread cache, and nothing to warm. Validating the
/// image is the whole of its setup, and [`prewarm`] moves that off the
/// critical path.
static SPLITTER: LazyLock<dense::DFA<&'static [u32]>> = LazyLock::new(|| {
    let (dfa, read) = dense::DFA::from_bytes(&DFA_IMAGE.0)
        .expect("the embedded automaton is this target's, built by build.rs");
    assert_eq!(read, DFA_IMAGE.0.len(), "the embedded automaton is padded");
    dfa
});

/// The state every piece starts in.
///
/// A DFA can have one start state per look-behind context, so the general
/// search resolves it per call. `pattern::SPLIT_PATTERN` has no assertion that reads
/// what came before, which makes the four contexts one state -- checked by
/// `the_anchored_start_state_does_not_depend_on_context` -- so it is resolved
/// once and a piece costs only its bytes.
static ANCHORED_START: LazyLock<StateID> = LazyLock::new(|| {
    let input = Input::new("").anchored(Anchored::Yes);
    SPLITTER
        .start_state_forward(&input)
        .expect("an anchored DFA has an anchored start state")
});

thread_local! {
    /// The merge buffer, one per thread, reused across pieces so a text of a
    /// thousand pieces allocates once.
    static SCRATCH: RefCell<Vec<Part>> = const { RefCell::new(Vec::new()) };
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
    count_capped(text, usize::MAX)
}

/// The number of `o200k_base` tokens in `text`, or `limit` if there are more.
///
/// A caller comparing a count against a threshold does not need the part of
/// the count above it, and a span is routinely many times the whole budget.
/// Stopping at `limit` makes that comparison cost the threshold rather than
/// the span.
pub fn count_capped(text: &str, limit: usize) -> usize {
    let vocab = &*VOCAB;
    SCRATCH.with_borrow_mut(|parts| {
        let mut total = 0;
        split(text, |piece| {
            total += merge_count(piece.as_bytes(), vocab, parts);
            total < limit
        });
        total.min(limit)
    })
}

/// Call `emit` with each piece of `text`, in order, until it returns `false`.
///
/// The pieces are exactly the ones `o200k_base`'s pattern produces. Everything
/// but its `\s+(?!\S)` alternative comes from [`SPLITTER`]; that one is applied
/// here, to the only match it can ever disagree with.
fn split<'t>(text: &'t str, mut emit: impl FnMut(&'t str) -> bool) {
    let splitter = &*SPLITTER;
    let start = *ANCHORED_START;
    let mut cursor = 0;
    while cursor < text.len() {
        // Every character belongs to some alternative -- a letter, a digit,
        // whitespace, or the run of everything else -- so a position with no
        // match is a pattern change, not an input the count should drop bytes
        // over.
        let Some(found_end) = piece_end(splitter, start, text.as_bytes(), cursor) else {
            let _ = emit(&text[cursor..]);
            return;
        };
        let piece = &text[cursor..found_end];
        if piece.is_empty() {
            // An empty match cannot advance the cursor; step a character past
            // it so the loop always terminates.
            let Some(next) = text[cursor..].chars().next() else {
                return;
            };
            if !emit(&text[cursor..cursor + next.len_utf8()]) {
                return;
            }
            cursor += next.len_utf8();
            continue;
        }

        // `\s+(?!\S)` sits ahead of the bare `\s+` and takes a run of blanks
        // one character short unless the run ends the text -- the character it
        // leaves behind opens the following piece as its optional prefix. A run
        // carrying a line break was already claimed by `\s*[\r\n]+`, and no
        // other alternative matches blanks alone.
        // Whether every character is a blank is only worth asking once the
        // first one is: the whole-piece walk would otherwise run over every
        // identifier and every run of punctuation in the text, none of which
        // this rule can reach.
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
///
/// The cheap half of the rule above: a piece that fails this cannot be a run
/// of blanks, and almost none of them can.
#[inline]
fn starts_blank(piece: &str) -> bool {
    match piece.as_bytes().first() {
        // Spelled out rather than `is_ascii_whitespace`, which omits the
        // vertical tab that `\s` and `char::is_whitespace` both accept.
        Some(&byte) if byte.is_ascii() => matches!(byte, b'\t' | 0x0b | 0x0c | b' '),
        // A non-ASCII lead byte needs the character to answer; unicode has
        // blanks of its own, and they are rare enough to decode here.
        Some(_) => piece.chars().next().is_some_and(char::is_whitespace),
        None => false,
    }
}

/// Where the piece starting at `at` ends, or `None` if none starts there.
///
/// The automaton's own search is a general one: it sets up a search context,
/// checks for a prefilter and for acceleration, and reports a `Match` -- per
/// call, and a piece averages four bytes, so that setup costs more than the
/// bytes do. This is the same walk with the generality removed, taking a
/// transition per byte and remembering the last accepting position, which is
/// the leftmost-first end the DFA was built to report.
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
                // A match state is entered one byte after the match ends.
                last = Some(cursor - 1);
            } else if dfa.is_dead_state(state) || dfa.is_quit_state(state) {
                return last;
            }
        }
    }
    // The end of the text is itself a transition: a piece running to the last
    // byte is only accepted here.
    if dfa.is_match_state(dfa.next_eoi_state(state)) {
        last = Some(haystack.len());
    }
    last
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

/// Read one byte from every page of the embedded images.
///
/// Resolving them only slices bytes the binary already carries; the pages
/// behind those bytes are still unmapped, and the first probe faults them in
/// one at a time. Together they are megabytes and both are touched at random,
/// so on a cold binary that lands as tens of milliseconds inside whichever
/// count comes first.
fn fault_in() {
    // The smallest page any supported target uses: a larger one is touched
    // more often than it needs to be, never less.
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
    // Or the loops are dead code and fault nothing in.
    std::hint::black_box(seen);
}

/// Prepare the counter off the critical path.
///
/// Startup is a regex to compile, an automaton to build, and an image to fault
/// in -- together more than a small query's whole search. Compiling and
/// warming are one chain; faulting in is independent, so it gets its own
/// thread. Both run while the parallel walk does, and the serial phase after
/// it waits for neither.
pub fn prewarm() {
    std::thread::spawn(|| LazyLock::force(&SPLITTER));
    std::thread::spawn(fault_in);
}

#[cfg(test)]
mod tests;
