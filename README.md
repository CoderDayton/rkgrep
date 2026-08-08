# rkgrep

[![CI](https://github.com/CoderDayton/rkgrep/actions/workflows/ci.yml/badge.svg)](https://github.com/CoderDayton/rkgrep/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-2021-orange.svg)](https://www.rust-lang.org/)

Ranked, span-scoped, budget-packed code search. ripgrep answers *which lines
match*; rkgrep answers *what should be read*.

It links ripgrep's own engine in-process (`grep-searcher`, `grep-regex`,
`ignore`), so the pattern and its regex syntax are ripgrep's, unchanged. What
it adds is a ranking over the hits, an expansion from each match to the
declaration around it, and a token budget the result set has to fit. On 120
queries with ground truth taken from Python's `ast` module, it puts the
declaring file inside a 2000-token budget **100% of the time** against
`rg -C 10`'s 99.2%, at equal MRR — see [Quality](#quality). It is slower than
ripgrep at full width on a large tree, and it is meant to leave matches out;
if either matters, see [when not to use this](#when-not-to-use-this).

```console
$ rkgrep validate_token
src/auth/service.rs:142-159 (fn validate_token) [88 tok]
pub fn validate_token(token: &str) -> Result<Claims> {
    let claims = decode(token)?;
    ...
}

src/api/middleware.rs:61-74 (fn require_auth) [64 tok]
async fn require_auth(req: Request) -> Result<Response> {
    let claims = validate_token(req.header("authorization")?)?;
    ...
}
```

Docs: [CLI](docs/cli.md) · [Architecture](docs/architecture.md) ·
[Extraction](docs/extraction.md) · [Performance](docs/performance.md) ·
[Quality](docs/quality.md)

## Features

- **Declarations rank first, unconditionally.** A match on a declaration line
  is what "where is X" asks for; a file mentioning X twenty times does not
  outrank the one defining it.
- **A match expands to its declaration, not to ±N lines.** Overlapping regions
  merge, so the same lines are never sent twice.
- **The result set has a size.** Spans pack under `--max-tokens` best-first,
  capped per file so one crowded module cannot take the budget.
- **Four declaration shapes, no parser per language** — keyword, receiver,
  signature, and callable-bound-to-a-name, covering Rust, Go, Python, C, C++,
  Java, C#, JavaScript, TypeScript, Ruby, PHP, and shell. Comments and string
  literals are masked first, length-preserving, so a declaration written inside
  a docstring is never reported. See [Extraction](docs/extraction.md).
- **Nesting from indentation.** A declaration knows its depth, which is what
  separates the `save` a module declares from the `save` an unrelated class
  happens to have.
- **Two-phase search.** A scout pass runs without line numbers and records only
  what ranking needs; exact positions are resolved on the handful of files that
  can actually be returned.
- **`--stats` reports per phase**, so a slow query says whether it is short of
  cores or short of a better ranking cut.

## Usage

```console
cargo install --path .
```

Then:

```console
rkgrep PATTERN [PATH]
```

| Flag | Meaning |
| --- | --- |
| `-t, --max-tokens N` | budget for the whole result set (default 2000) |
| `--max-per-file N` | cap spans from any one file (default 3) |
| `-g, --glob GLOB` | restrict to matching files, repeatable |
| `-w`, `-i`, `-F` | whole words, ignore case, literal string |
| `--hidden`, `--no-ignore` | search hidden files; ignore `.gitignore` |
| `-l, --anchors-only` | anchors without source text |
| `--json` | machine-readable output |
| `--stats` | spans, tokens, and per-phase timings, to stderr |
| `--threads N` | worker threads (0 chooses automatically) |

```console
rkgrep -w handle_request src/        whole words, under src/
rkgrep -t 8000 -g '*.py' Config      bigger budget, Python only
rkgrep --json parse_url | jq .       structured output
rkgrep -l TODO                       anchors only
```

Exit status follows grep: `0` matched, `1` did not, `2` error. Every span leads
with `path:start-end`, so a result can be opened directly rather than searched
for again. [docs/cli.md](docs/cli.md) has every flag and the JSON schema.

## When not to use this

| Situation | Use instead | Why |
| --- | --- | --- |
| You need every match | `rg` | rkgrep fills a budget and stops. Leaving matches out is the feature. |
| Counting, listing, piping to a script | `rg -c`, `rg -l` | Ranking and span extraction do work a count throws away. |
| Prose, logs, config, data | `rg -C` | Extraction looks for declarations. A file with none falls back to fixed windows, which is what `rg -C` already gives you. |
| A tree far larger than the answer | `rg` to scope, then rkgrep | The directory walk dominates; narrowing the path is worth more than any ranking. |

## Performance

45k files, 1.2 GB, warm page cache, 32 cores, best of five:

| Pattern | rkgrep, 1 thread | rkgrep, 16 threads | `rg -c`, 16 threads |
| --- | --- | --- | --- |
| `function` | 214 ms | 53 ms | 34 ms |
| `Result` | 216 ms | 67 ms | 30 ms |
| `config` | 221 ms | 70 ms | 30 ms |

The parallel walk scales 6.9× across 32 cores, matching ripgrep. Ranking and
extraction are serial and about a third of a wide query, which caps overall
scaling near 4× however many cores are added. Reproduce with
`bench/scaling.py`; the phase model and where the remaining serial time goes
are in [docs/performance.md](docs/performance.md).

## Quality

98-file Python repository, 120 queries, ground truth from Python's `ast`
module rather than from rkgrep's own extractor:

| | MRR (definition) | Definition in budget | Neighborhood in budget |
| --- | --- | --- | --- |
| `rg -l`, by match count | 0.487 | 32.5% | 27.7% |
| `rg -l`, declarations first | 0.985 | 58.3% | 27.9% |
| `rg -C 10`, declarations first | 0.985 | 99.2% | 94.2% |
| **rkgrep** | **0.985** | **100.0%** | **97.6%** |

Reproduce with `python3 bench/quality.py <repo>`. Read the last column with
suspicion — it counts files that textually reference the name, which is what a
grep computes, so it rewards returning more files rather than better ones.
[docs/quality.md](docs/quality.md) has the ground-truth construction and what
each metric can and cannot show.

## Development

```console
cargo test                              # 38 tests: 25 unit, 13 integration
cargo clippy --all-targets -- -D warnings
cargo fmt --check
python3 bench/quality.py <repo>         # retrieval quality against AST truth
python3 bench/scaling.py <tree>         # thread scaling against ripgrep
```

Both benchmarks want a repository to run against, and the quality one wants
Python sources in it. Changes to ranking or extraction should carry a quality
run in the pull request: the metrics move in opposite directions under some
changes, and a number is the only way to see it.

## License

[MIT](LICENSE)
