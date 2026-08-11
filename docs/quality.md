# Quality

Whether the ranking is any good, measured against ground truth none of the
systems being measured produced.

- [Ground truth](#ground-truth)
- [Metrics](#metrics)
- [Results](#results)
- [Where ranking is weakest](#where-ranking-is-weakest)
- [What the depth term is worth](#what-the-depth-term-is-worth)
- [Running it](#running-it)

## Ground truth

Queries and answers come from a tree-sitter grammar for the language under
test, not from rkgrep's extractor and not from ripgrep. Nothing here scores
itself, which is the only reason the numbers mean anything: a retrieval system
evaluated against its own notion of a declaration will report whatever its
extractor happens to believe.

For each query symbol the truth is two things:

- **the definition** — the file whose parse tree declares the name at the top
  level
- **the neighborhood** — the files that mention the name

Queries are the symbols with exactly one declaration site that are also
referenced elsewhere, sampled at a fixed seed so runs are comparable. A file
the grammar cannot parse cleanly is excluded from truth rather than guessed at.

Python, Go, Rust, JavaScript and TypeScript have grammars wired up. Adding a
language is a `Language` entry in `benches/support/truth.rs`: the node kinds
that declare a name, the node kinds that mention one, and a regex matching a
declaration line for the ripgrep baseline.

## Metrics

| Metric | Question | Trustworthy |
| --- | --- | --- |
| MRR (definition) | How highly is the declaring file ranked? | yes |
| Definition in budget | Does the declaring file survive the token budget? | yes |
| Neighborhood in budget | What fraction of referencing files survive it? | **no** — see below |

The neighborhood is defined as files that textually reference the name, which
is exactly what a grep computes. A system that returns more files scores higher
on it whether or not those files help, and a system returning larger, more
complete spans scores lower because fewer fit. It is reported because a large
drop signals that the budget is being spent badly, not because a small
difference means anything.

MRR and the definition rate are the two derived from the parser, and they are
the ones to read.

Both are measured over the ranked list before budgeting, then after, so a
system cannot win by returning everything.

## Results

120 queries per language, 2000-token budget, against `rg -C 10` with
declarations ranked first — the honest baseline, charged for the lines it
actually returns.

| Corpus | System | MRR (definition) | Definition in budget | Neighborhood in budget |
| --- | --- | --- | --- | --- |
| Python, 168 files | `rg -C 10` | 0.986 | 95.8% | 80.3% |
| | **rkgrep** | **0.996** | **100.0%** | **86.6%** |
| TypeScript, 164 files | `rg -C 10` | 0.956 | 95.0% | 84.6% |
| | **rkgrep** | **0.960** | **100.0%** | **95.6%** |
| Go, 902 files | `rg -C 10` | 0.957 | 94.2% | 84.2% |
| | **rkgrep** | 0.954 | **97.5%** | **87.1%** |
| Rust, 547 files | `rg -C 10` | 0.908 | 88.3% | 68.9% |
| | **rkgrep** | **0.947** | **98.3%** | **80.2%** |

On ranking rkgrep leads on Python, TypeScript and Rust, and ties on Go. On what
survives the budget it leads on all four, by 3.3 to 10 points — the budget is
denominated in `o200k_base` tokens and charged to both systems the same way, so
what separates them is how much of it each spends on lines that answer the
query.

Query latency is reported alongside the table by the harness, but what a query
costs against ripgrep is [performance](performance.md)'s question, measured
there on a tree large enough for the answer to mean something.

## Where ranking is weakest

Go is the one corpus where the baseline still ranks the declaring file higher,
by 0.003.

Rust exercises the extractor hardest. Its declarations carry the most text
between the line's first word and the name — `pub(crate) unsafe trait`,
`impl<T: Clone> Trait for Type` — and its methods live one level down inside
`impl` blocks, so a name is more often declared at two depths at once. That is
also why the depth term below is worth more on Rust than anywhere else.

What it still loses there is ambiguity no text scan can settle. `mod barrier;`
is written in two files, and only one is the answer: the other is inside a
`cfg_sync! { … }` block, which the grammar reads as a macro call rather than a
module, so it never enters the ground truth. Both lines declare a module named
`barrier` and nothing in either line says which.

The signature shape, which recognizes a declaration from a name followed by a
parameter list, is the one that trades precision for reach. Its three guards
are in [extraction](extraction.md#what-it-misses).

## What the depth term is worth

Nesting depth separates a module-level `save` from the `save` method an
unrelated class happens to have. It is charged only where a shallower
declaration *of the same name* is competing, which is what `W_DEPTH` in
`src/search/rank.rs` weights.

Setting that weight to zero, same corpora and queries:

| Corpus | MRR with the penalty | MRR without |
| --- | --- | --- |
| Python | 0.996 | 0.988 |
| TypeScript | **0.960** | 0.926 |
| Go | 0.954 | 0.954 |
| Rust | **0.947** | 0.902 |

Definition-in-budget and neighborhood-in-budget do not move on any of the four.

The term earns its place on Rust and TypeScript, barely registers on Python and
is inert on Go.
It fires only when one name is declared at two different depths, which `impl`
blocks and nested arrow functions make routine and which the Python and Go
corpora barely produce — so a change to it will look free unless Rust or
TypeScript is one of the languages measured.

Charging depth unconditionally would also demote methods that nothing competes
with, and since a top-level span is the larger of the two, that spends budget
without answering the question any better. Raising the weight above 1.0 changes
nothing, because by then the competing declaration has already lost.

## Running it

The benchmark drives the release binary, so build it first.

```console
cargo build --release
cargo bench --bench quality -- <repo>
cargo bench --bench quality -- <repo> --lang go --queries 300 --max-tokens 8000
cargo bench --bench quality -- <repo> --lang rust --show-failures 10
```

`--show-failures` lists the queries where the baseline ranks the declaring file
higher, with what each system put on top. A mean says a change helped; this
says which declaration shape it still misreads, and it is where the last three
extraction fixes came from.

`--lang` selects the grammar and defaults to `python`; the corpus is every file
under `<repo>` with that language's extension, walked by the same ignore rules
rkgrep itself applies.

Run it on any change to ranking or extraction, on more than one language. The
metrics move in opposite directions under some changes — the depth table above
is the example — and a number is the only way to see it.
