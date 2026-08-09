# rkgrep

[![CI](https://github.com/CoderDayton/rkgrep/actions/workflows/ci.yml/badge.svg)](https://github.com/CoderDayton/rkgrep/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-2021-orange.svg)](https://www.rust-lang.org/)

Ranked, span-scoped, budget-packed code search. ripgrep answers *which lines
match*; rkgrep answers *what should be read*.

Every match expands to the declaration containing it, the declarations are
ranked, and the result set is packed to fit a token budget — so the output is
whole functions in a size you choose, for reading unfamiliar code or filling a
model's context window. It links ripgrep's own engine in-process
(`grep-searcher`, `grep-regex`, `ignore`), so the pattern and its regex syntax
are ripgrep's, unchanged. Against `rg -C 10` charged for the lines it returns,
rkgrep fits the declaring file into a 2000-token budget as often or more often
on all four measured languages, and fits more of the files that reference the
symbol on every one of them, by up to 8 points — see [Quality](#quality). It is
slower than ripgrep on a wide query over a large tree, and it drops matches by
design; both are structural, not tuning — see
[when not to use this](#when-not-to-use-this).

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

- Declarations rank first: a file that declares the symbol outranks one that
  mentions it twenty times.
- A match expands to its declaration rather than ±N lines, and overlapping
  regions merge, so no line is sent twice.
- Spans pack under `--max-tokens` best-first — 2000 by default, at most 3 per
  file, so one crowded module cannot take the budget.
- Four declaration shapes and no parser per language — keyword, receiver,
  signature, and callable-bound-to-a-name — cover Rust, Go, Python, C, C++,
  Java, C#, JavaScript, TypeScript, Ruby, PHP, and shell. Comments and string
  literals are masked first, so a declaration written inside a docstring is
  never reported. See [Extraction](docs/extraction.md).
- Declarations carry a nesting depth taken from indentation, which separates
  the `save` a module declares from the `save` an unrelated class has.

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
| `-A, --no-budget` | every ranked span: no budget, no per-file cap |
| `--max-per-file N` | cap spans from any one file (default 3, 0 for no cap) |
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
| You need every match | `rkgrep --no-budget`, or `rg` | The default fills a budget and stops; `--no-budget` returns every ranked span instead. `rg` is still faster if ordering and span expansion buy you nothing. |
| Counting, listing, piping to a script | `rg -c`, `rg -l` | Ranking and span extraction do work a count throws away. |
| Prose, logs, config, data | `rg -C` | Extraction looks for declarations. A file with none falls back to fixed windows, which is what `rg -C` already gives you. |
| "Go to definition" for a name declared in several places | an editor's LSP, or an index (`rust-analyzer`, `gopls`, `ctags`) | Ranking is lexical and there is no symbol table. Depth separates a module-level `save` from a method; nothing separates two methods named `save` in different files. |

Each of these follows from the current design: ranking whole declarations under
a budget, with no index and no parser.

## Performance

68k files, warm page cache, 32 cores, median of seven:

| Pattern | rkgrep, 1 thread | rkgrep, 16 threads | `rg -c`, 16 threads |
| --- | --- | --- | --- |
| `function` | 333 ms | 70 ms | 45 ms |
| `Result` | 329 ms | 67 ms | 43 ms |
| `config` | 321 ms | 53 ms | 42 ms |

The parallel walk matches ripgrep. Ranking and extraction are serial and about
a third of a wide query, which caps overall scaling around 5–6× against
ripgrep's 7.6× on the same tree. `--stats` splits any query the same way, so a
slow one says whether it is short of cores or short of a better ranking cut. On
a tree far larger than the answer the walk dominates everything else, and
scoping the path is worth more than any ranking.

Reproduce with `cargo bench --bench scaling -- <tree>`; the phase model and
where the remaining serial time goes are in
[docs/performance.md](docs/performance.md).

## Quality

120 queries per language, 2000-token budget, ground truth from tree-sitter
grammars rather than from rkgrep's own extractor, against `rg -C 10` with
declarations ranked first and charged for the lines it returns:

| Corpus | | MRR (definition) | Definition in budget | Neighborhood in budget |
| --- | --- | --- | --- | --- |
| Python | `rg -C 10` | 0.996 | 100.0% | 92.6% |
| | **rkgrep** | **1.000** | **100.0%** | **96.4%** |
| TypeScript | `rg -C 10` | 0.962 | 96.7% | 92.8% |
| | **rkgrep** | **0.976** | **100.0%** | **97.7%** |
| Go | `rg -C 10` | 0.957 | 97.5% | 91.2% |
| | **rkgrep** | 0.954 | **98.3%** | **93.0%** |
| Rust | `rg -C 10` | **0.908** | 96.7% | 81.4% |
| | **rkgrep** | 0.887 | 96.7% | **89.0%** |

Reproduce with `cargo bench --bench quality -- <repo> --lang <lang>`. The last
column counts files that textually reference the name, so it rewards returning
more files rather than better ones. [docs/quality.md](docs/quality.md) has the
ground-truth construction and the limits of each metric.

## Development

```console
cargo test                              # 46 tests: 28 unit, 18 integration
cargo clippy --all-targets -- -D warnings
cargo fmt --check
cargo build --release                   # both benchmarks drive this binary
cargo bench --bench quality -- <repo> --lang rust
cargo bench --bench scaling -- <tree>
```

Both benchmarks need a repository to run against; the quality one needs sources
in `python`, `typescript`, `javascript`, `go`, or `rust`. Run it on more than
one language for any change to ranking or extraction — the metrics move in
opposite directions, and terms that are inert on one language carry another.
`--show-failures N` lists the queries where the baseline ranks the declaration
higher.

## License

[MIT](LICENSE)
