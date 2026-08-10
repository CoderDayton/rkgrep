# Performance

What a query costs, which part of it scales, and what caps the rest.

- [Numbers](#numbers)
- [The phase model](#the-phase-model)
- [What scales](#what-scales)
- [What does not](#what-does-not)
- [Measured dead ends](#measured-dead-ends)
- [Methodology](#methodology)

## Numbers

68k files, warm page cache, 32 cores, median of seven runs:

| Pattern | 1 thread | 4 | 8 | 16 | 32 | `rg -c` at 16 |
| --- | --- | --- | --- | --- | --- | --- |
| `function` | 333 ms | 118 ms | 83 ms | 70 ms | 66 ms | 45 ms |
| `Result` | 329 ms | 118 ms | 85 ms | 67 ms | 65 ms | 43 ms |
| `config` | 321 ms | 103 ms | 70 ms | 53 ms | 54 ms | 42 ms |

Single-threaded, rkgrep and ripgrep are within 5% of each other, and on
`function` rkgrep is faster. At full width ripgrep pulls ahead by 25–55%,
because more of its query is parallel.

The curve is flat from 16 threads on a 32-core machine: past that the serial
fraction dominates and extra workers return nothing.

Span extraction is not the cost. `--max-tokens 1` measures the same as a full
budget, because spans are built only for files that could plausibly be
returned.

### What the token counter costs

`--max-tokens` is denominated in real `o200k_base` tokens, and a tokenizer
assembled at startup charges for that before answering anything: decoding 200k
ranks and building an encoder takes ~55 ms, and determinizing the split pattern
several more — every run, however small the query.

None of that survives into the binary. `build.rs` lays out both the vocabulary
and the automaton that cuts text into pieces, and the process reads them where
they sit: resolving the table is **31 ns**, and startup is validating the
automaton image, on a background thread while the walk runs. The first count of
a process costs **11 µs**.

Counting then runs at **105 MB/s**, split about evenly between cutting the
pieces and looking each one up. Extraction records a span without counting it,
and a span is counted only up to what the candidate budget still has room for,
so a query pays for the spans that reach the result set and not for the hundred
it discards.

The images cost 8.1 MB of the binary: 4 MiB of probe slots at a deliberate 0.5
load factor, 1.4 MB of token bytes, and 2.7 MB of automaton. Halving the slots
would halve the first and lengthen every probe run, which is the wrong trade
for the hottest lookup in the program.

## The phase model

```console
$ rkgrep function ~/src --stats
rkgrep: walk 31.2ms (15526 files matched), rank 4.0ms, extract 12.9ms (8 files read)
```

| Phase | Parallel | Work |
| --- | --- | --- |
| `walk` | yes | every file in the tree, searched |
| `rank` | no | every ordering decision: candidates, then the spans they yield |
| `extract` | yes, within a batch | read candidates, extract declarations, resolve line numbers |

The two numbers in parentheses are the ones to read first. 15,526 files matched
and 8 were read: if that ratio is close to 1, the query is not selective enough
for ranking to help, and narrowing the path or the glob will beat any tuning.

## What scales

The walk scales **6.9× across 32 cores**, matching ripgrep, and always did. It
is `ignore::WalkBuilder::build_parallel` with one searcher and one matcher per
worker, collecting through a channel rather than a shared `Vec` behind a mutex.

## What does not

Ranking and extraction are serial with respect to the walk, and at full width
they are roughly a third of a query. By Amdahl that caps overall scaling around
5–6× however many cores are added, against ripgrep's 7.6× on the same tree.

**Ranking** is 3–4 ms on 15k matching files and is no longer worth attacking.
`path_score` — which lowercases a file name and splits it into a `Vec<String>`
— is computed once per candidate rather than from inside a sort comparator that
runs O(n log n) times.

**Extraction** is the remaining floor. A batch is spread across one worker per
core, each pulling the next candidate from a shared cursor, so it costs what
its slowest worker costs rather than the sum. Never one thread per file: a
`--no-budget` query extracts every matching file in the tree, and spawning a
thread for each of several thousand small ones costs more than reading them
does.

What is left is the single largest file in a batch. On a JavaScript tree that
is routinely a megabyte of bundled output, and one file is one worker however
many cores are available. Closing that means either giving up on huge single
files — a size cut would change which spans come back — or splitting one file's
extraction across threads, which is four sequential passes (read, mask, scan,
search) over one buffer. Neither is obviously worth it while the walk is three
times larger.

### Without a budget

`--no-budget` inverts the design: every matching file is read, so extraction
stops being a rounding error on the walk and becomes the query. On a 92k-file
tree where `Config` matches 4,756 files, the walk is 74 ms and extraction is
245 ms across 32 cores. There is nothing for the budget check to stop for, so
the batch is a whole pass of 256 rather than 8, and the workers are spawned
once for it.

## Measured dead ends

Recorded so they are not tried again. Each was implemented and measured.

| Change | Result |
| --- | --- |
| Memory mapping (`MmapChoice::auto()`) | 2× slower — per-file setup dominates when most files are small |
| Reading each file once into a reused buffer | Slower — most files do not match, so full I/O plus UTF-8 validation for all of them costs more than re-reading the few that do |
| `Mutex<Vec<_>>` → `mpsc` channel for collection | No measurable change |
| Cloning the matcher per worker | No measurable change on this workload; kept, because it is correct and free |
| Raising the thread count past 16 | Flat to slightly worse |

Two of these were pursued on the theory that the scaling gap came from lock
contention inside the regex crate's cache pool. A threaded simulation refuted
it: drag at 32 threads was ~3× for the shared regex *and* for a plain byte
scanner, which makes it a machine effect rather than a lock. The gap was the
serial phases, which only phase-level instrumentation showed.

The one change that survived from that line of work — replacing the
declaration regex with a hand-written scan — is worth 12–23% single-threaded,
for reasons unrelated to threading.

## Methodology

The `scaling` benchmark warms the page cache, then times both tools as
subprocesses on identical input, so the numbers include process startup and are
what a user would time at a shell. It drives the release binary, so build that
first.

```console
cargo build --release
cargo bench --bench scaling -- /path/to/tree
cargo bench --bench scaling -- /path/to/tree --patterns Result --threads 1 16
```

Each cell is the median of `--repeats` trials with a 95% bootstrap confidence
interval, and each pattern reports the worst relative spread across its cells.
Median rather than mean because the distribution is one-sided — noise only ever
makes a run slower — and an interval rather than a bare best-of because a cell
whose spread is wide is not a measurement, and the table should say so.

Two things will produce nonsense numbers:

- **A tree larger than free page cache.** Single-threaded runs go I/O-bound
  while parallel ones hide the latency, which reports impossible speedups. A
  42 GB tree on a 61 GB machine showed 27 s at one thread and 0.16 s at four.
- **Comparing against `rg` without `-c`.** Formatting and writing matched lines
  is work rkgrep's scout does not do.
