# rkgrep

Rust CLI. Ranked, span-scoped, budget-packed code search over ripgrep's engine,
linked in-process.

The pipeline and the ranking formula: `docs/architecture.md`. Declaration
shapes and what they miss: `docs/extraction.md`.

## Structure

`src/`: `main.rs` (CLI), `search.rs` (walk, ranking, regions, packing),
`spans.rs` (masking, the four declaration shapes, nesting), `render.rs`.
Tests in `src/spans_tests.rs` and `tests/search.rs`; harnesses in `bench/`.

## Rules

- Any change to ranking or extraction runs `python3 bench/quality.py <repo>`
  before it lands. The metrics move in opposite directions — depth ranking
  bought MRR and cost coverage — and a number is the only way to see it.
- Read `docs/performance.md#measured-dead-ends` before optimizing the search
  path. mmap, buffer reuse, and channel-vs-mutex were each implemented and
  measured; two were pessimizations.
- Diagnose slowness with `--stats`, never by guessing. It splits a query into
  walk (parallel) and rank/extract (serial); the phases have opposite fixes.
- `grep_searcher::sinks::UTF8` errors when line numbers are disabled, which
  skips every file and looks fast. The scout has its own `Sink` for that
  reason — do not swap it back.
- `KEYWORDS`, `MODIFIERS`, `CONTROL_WORDS` are binary-searched, so they stay
  sorted. `tables_are_sorted_for_binary_search` enforces it.
- Masking runs before matching, and masked bytes become spaces, never nothing:
  line numbers from masked text must stay valid against the original.
- Both harnesses need a target repository; `quality.py` also needs Python
  sources and the prototype directory holding `bench_rg.py`.
- Before commit: `cargo test` (38, 0 fail), `cargo clippy --all-targets --
  -D warnings`, `cargo fmt --check`. Commits simple, no AI attribution; push
  when asked.
