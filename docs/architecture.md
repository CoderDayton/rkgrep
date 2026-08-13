# Architecture

The pipeline, and why each stage costs what it costs.

ripgrep answers "which lines match" better than anything else, and rkgrep links
its engine directly rather than shelling out: no process spawn, no
serialization round-trip, and line numbers arrive as integers rather than
parsed text.

- [The pipeline](#the-pipeline)
- [Two-phase search](#two-phase-search)
- [Comment scoping](#comment-scoping)
- [The candidate cut](#the-candidate-cut)
- [Ranking](#ranking)
- [Regions and merging](#regions-and-merging)
- [Several patterns](#several-patterns)
- [Packing](#packing)
- [Module layout](#module-layout)

## The pipeline

```text
walk      parallel   every file in the tree, searched, matching ones kept
  ↓
rank      serial     order candidates by what the walk already learned
  ↓
extract   parallel   read candidates, find declarations, resolve line numbers
  ↓
pack      serial     fill the token budget, best first
```

`--stats` reports every phase, plus the write that follows them:

```console
$ rkgrep function ~/src --stats
rkgrep: 8 spans, 1985/2000 tokens, 24.8ms total
rkgrep: walk 7.6ms (68 files matched), rank 0.1ms, extract 14.1ms (8 files read)
rkgrep: pack 2.6ms, render 0.0ms
```

68 files matched and 8 were read. That ratio is the whole design.

## Two-phase search

The walk deliberately learns as little as possible. Its sink records a match
count and one boolean — whether any matched line declares the query on its own
— and holds neither the file's text nor its matched line numbers.

The boolean tests the declared *name* against the pattern, the same rule
applied once the file is read. In `const a = store.createProject(...)` the
match sits on a declaration's first line, but the declaration is `a` — a local
binding that calls the symbol. Counting it would hand the signal to every file
full of callers, and a test file of them then outranks the file that declares
the name on match count alone. The pattern is applied unanchored, so under `-w`
the same test also separates `pub fn from_std(stream: net::TcpStream)` from a
query for `std`. The test stops after
the first 16 matched lines of a file: a file that declares the query declares it
in one of its first few matches, and scanning every match costs the walk a fifth
of its time on a term like `Result`, which sits on a declaration line in almost
every file and declares almost none of them.

Line numbers are the single most expensive option in the searcher, and most
matching files are never returned. They are resolved later, per candidate, over
the files that survive the cut.

This needs a hand-written `Sink`: `grep_searcher::sinks::UTF8` returns an error
when line numbers are disabled, which fails every file silently and produces an
empty result that still looks fast.

Two searcher settings are deliberate and measured:

- **No memory mapping.** Against a 45k-file tree it doubled search time, because
  per-file mmap setup dominates when most files are small.
- **`BinaryDetection::quit(0)`** stops at the first NUL rather than scanning a
  binary to the end.

The per-line declaration check is a hand-written scan rather than a regex. A
23-way alternation of qualifiers in front of a 17-way alternation of keywords
costs several times what reading the two or three words that decide the
question does; replacing it took 12–23% off a single-threaded search.

Each worker gets its own matcher clone. The regex engine draws a scratch cache
from a pool that is lock-free only for the thread that created it
([rust-lang/regex#934](https://github.com/rust-lang/regex/issues/934)); cloning
is cheap, since the compiled program sits behind an `Arc`.

## Comment scoping

`--comments` restricts matches to comments. The mask that hides comments during
extraction is inverted — everything but the comments is blanked, byte offsets
and newlines preserved — and the pattern is searched against that in place of
the file, so a line number resolved against it is the file's own.

It is a filter, not a different unit. A hit still expands to the declaration
the comment sits in, so a `TODO` comes back with the code it is about.

The walk pays for it: comments have to be cut out before the pattern sees the
file, which the engine cannot do for us, so the scout masks and matches a line
at a time instead of searching the file by path. Its match counts then rank
files by their comment matches alone. The declaration hint is always false,
because a file masked down to its comments declares nothing.

## The candidate cut

A common term matches tens of thousands of files and returns eight spans, so
ordering the rest is work thrown away.

Candidates are lifted out in passes of 256 by `select_nth_unstable_by`, which
is linear, and only those 256 are sorted. Another pass runs only if the budget
is still unfilled. The walk's output is left in whatever order it finished in —
the ordering is total, breaking ties on the path, so the result is
deterministic without paying to sort the whole set.

Extraction stops once `CANDIDATE_SLACK` (4×) the budget is on hand, and always
examines at least `MIN_CANDIDATE_FILES` (8). Ranking needs more material than it
returns, but not all of it.

Within a pass, candidates are read a batch at a time. With a budget the batch is
the 8 files the budget check cannot cut short, so a query reads at most 7 files
past where checking one at a time would have stopped — files that widen the
field ranking chooses from rather than work thrown away. With `--no-budget`
there is nothing to stop for and the batch is the whole pass.

## Ranking

Files are ordered before any of them is read, from signals the walk already
produced: whether a matched line declared the query, how many matches there
were, and how much the file's own name looks like the query.

Spans are then scored:

```text
score = 1.0 · ln(matches + 1)
      + 1.5 · fraction of query terms present in the span
      + 1.0 · how much the file name looks like the query
      − 1.0 · depth below a competing declaration of the same name
```

Query terms are recovered by stripping regex metacharacters and splitting what
is left the way an identifier splits, so a search for `validateToken` still
ranks a `validate_token` span.

The final sort puts **declarations first, unconditionally**, then score, then
path and start line. Letting a file that mentions X twenty times outrank the
one that defines it is how a ranker loses to a one-line grep.

A span counts as a declaration when the match lands on a declaration's first
line, *the pattern matches the declared name*, and the declaration is of a kind
that declares rather than re-opens. The first two are needed together: in
`const a = store.createProject(...)` the match is on a declaration's first
line, but the declaration is `a`, so the span is a caller and not an answer to
"where is `createProject`". Applying the same matcher to the name keeps this
right for regex patterns, and under `-w` stops `createProject` from claiming
`createProjectManager`.

The third excludes `impl`. `impl Repo` re-opens a type its `struct` declares,
and `impl Store for Repo` names a trait declared elsewhere again — and an impl
block names its type more often than the declaration does, so on match count it
wins every time it is allowed to compete, putting a block of methods above the
definition those methods belong to.

The depth term is charged only against a declaration that a shallower
declaration of the same name is competing with. Penalizing depth across the
board costs coverage without improving the answer — measured, see
[Quality](quality.md#what-the-depth-term-is-worth).

## Regions and merging

Each matched line expands to the declaration enclosing it, found by binary
search over the declaration table rather than a linear scan — the difference
between a fast query and a slow one in a file with hundreds of symbols.
Declarations end at their bodies rather than running to the next one, so they
do not tile the file: a line inside none of them gets a ±6-line window, as does
a match inside a declaration too large for the budget to admit. See
[Extraction](extraction.md#spans).

Matches landing in the same declaration collapse into one region, so a function
matched eight times is one span rather than eight. Overlapping regions then
merge: a match on an import line and a match in the function below it produce
windows that overlap, and emitting both spends the budget twice on the same
lines while showing the reader the same code under two anchors.

## Several patterns

`-e` is repeatable and every pattern shares one budget. The tree is still walked
once: the patterns are joined into one alternation for the scout, which only has
to decide which files are worth opening. Telling them apart is deferred to
extraction, where every pattern is tried against each line as it is read, so a
second pattern costs one more matcher call per line and no more reading.

Ranking is per pattern from there. Each keeps its own terms, so `claims.rs`
scores high for `Claims` and not for `refresh`; candidate ordering uses the best
score across patterns, since a file is worth reading if it answers any of them.
A span belongs to exactly one pattern — the one it declares, or failing that the
one with the most matched lines in it — so a function two patterns hit is
returned once rather than twice.

The extraction gate is per pattern for the same reason. It stops once every
pattern has its share of `CANDIDATE_SLACK`, so a common pattern reaching its
share does not end the search for a rare one.

## Packing

Spans are taken best-first until the budget is full, skipping any that does not
fit and continuing — so a large span near the top does not strand the remaining
budget. No file may contribute more than `--max-per-file` spans, which stops one
crowded module from taking everything.

With several patterns the order is round-robin: the top span for the first, then
the second, then back to the first, with an exhausted pattern dropping out of
the rotation. Score order alone would hand back three answers about whichever
symbol is most common, which is not what asking about three symbols means.

`--min-references` is met by giving up declarations, lowest-ranked first, and
refilling. Surrendering one is rarely enough on its own — the next declaration
takes the freed budget — so they go cumulatively, which leaves the best answer
the last thing given up. A query with no references to promote keeps every
declaration it had rather than emptying itself chasing the floor.

## Module layout

| File | Holds |
| --- | --- |
| `src/main.rs` | CLI, exit codes, stats output |
| `src/search/mod.rs` | the pipeline end to end, plus `Hit`, `Options`, timings |
| `src/search/query.rs` | one matcher per pattern, and one for the walk |
| `src/search/walk.rs` | the parallel pass over the tree and its scout sink |
| `src/search/region.rs` | matched lines to declaration-scoped regions, merged |
| `src/search/extract.rs` | reading candidates best-first, spans in parallel |
| `src/search/rank.rs` | the weights, and every ordering decision |
| `src/search/pack.rs` | round-robin across patterns, then the token budget |
| `src/pathset.rs` | `--files-from` and `--since`, as one list of paths |
| `src/fetch.rs` | returning spans by anchor, with no search |
| `src/spans/mod.rs` | `Declaration`, opening a source file, and `enclosing` |
| `src/spans/words.rs` | the keyword tables and byte sets the rules use |
| `src/spans/mask.rs` | comments and literals blanked out, a line at a time |
| `src/spans/scan.rs` | the four declaration shapes, one line at a time |
| `src/spans/body.rs` | where a body ends, and the table built as a file streams |
| `src/tokenizer/` | `o200k_base` token counting against a compile-time table |
| `build.rs` | lays that table out, so startup maps it instead of building it |
| `src/render.rs` | text and JSON output |
| `src/spans/tests/` | unit tests for extraction, one file per module |
| `tests/search.rs` | integration tests over the built binary |
| `tests/query_shaping.rs` | several patterns, selection, scoping, fetch, output |
| `benches/` | quality and scaling harnesses |
