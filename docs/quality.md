# Quality

Whether the ranking is any good, measured against ground truth none of the
systems being measured produced.

- [Ground truth](#ground-truth)
- [Metrics](#metrics)
- [Results](#results)
- [What the depth term is worth](#what-the-depth-term-is-worth)
- [Running it](#running-it)

## Ground truth

Queries and answers come from Python's own `ast` module, not from rkgrep's
extractor and not from ripgrep. Nothing here scores itself, which is the only
reason the numbers mean anything: a retrieval system evaluated against its own
notion of a declaration will report whatever its extractor happens to believe.

For each query symbol the truth is two things:

- **the definition** — the file whose AST declares the name at module level
- **the neighborhood** — the files that reference the name

Queries are the symbols with exactly one definition site that are also
referenced elsewhere, sampled at a fixed seed so runs are comparable.

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

98-file Python repository, 120 queries, 2000-token budget:

| System | MRR (definition) | Definition in budget | Neighborhood in budget | Latency |
| --- | --- | --- | --- | --- |
| `rg -l`, by match count | 0.487 | 32.5% | 27.7% | — |
| `rg -l`, declarations first | 0.985 | 58.3% | 27.9% | — |
| `rg -C 10`, declarations first | 0.985 | 99.2% | 94.2% | 38 ms |
| **rkgrep** | **0.985** | **100.0%** | **97.6%** | **16 ms** |

The first two rows are what a whole-file baseline costs: ranking is fine, and
then the budget is spent on entire files, so two thirds of the definitions do
not survive it. The third row is the honest baseline — ripgrep's matched
regions, declarations first, charged for the lines it actually returns — and
it is a strong one.

Against it, rkgrep ties on ranking and wins on what survives the budget, at
less than half the latency.

## What the depth term is worth

Nesting depth was added to separate a module-level `save` from an unrelated
class's `save` method. Three ways of using it, same 120 queries:

| Depth handling | MRR | Definition in budget | Neighborhood in budget |
| --- | --- | --- | --- |
| None | 0.976 | 100.0% | 97.4% |
| Ordered before score | 0.996 | 100.0% | 96.9% |
| Penalty on every nested declaration (0.5) | 0.992 | 100.0% | 96.9% |
| **Penalty only where names compete (1.0)** | **0.985** | **100.0%** | **97.6%** |

Ordering by depth outright gives the best MRR and costs coverage, because
top-level spans are larger and crowd files out of the budget. Charging only
where a shallower declaration *of the same name* is competing addresses the
actual defect and improves every column at once, which is why it is what ships.

Raising that penalty from 1.0 to 3.0 changes nothing — by then the competing
declaration has already lost — so the smaller value is the one used.

## Running it

```console
python3 bench/quality.py <repo>
python3 bench/quality.py <repo> --queries 300 --max-tokens 8000
```

The harness needs the prototype directory that holds `bench_rg.py`, which
supplies the corpus, the ground truth, and the ripgrep baselines; point
`--prototype` at it if it is not in the default location.

Run it on any change to ranking or extraction. The metrics move in opposite
directions under some changes — the depth table above is the example — and a
number is the only way to see it.
