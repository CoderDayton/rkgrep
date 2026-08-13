# rkgrep

[![CI](https://github.com/CoderDayton/rkgrep/actions/workflows/ci.yml/badge.svg)](https://github.com/CoderDayton/rkgrep/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-2021-orange.svg)](https://www.rust-lang.org/)

Ranked, span-scoped, budget-packed code search. ripgrep answers *which lines
match*; rkgrep answers *what should be read*.

Point it at a symbol and you get whole declarations back: the function or class
each match sits in, ranked best first and packed into a token budget you set. A
context window's worth, or less. Underneath it is ripgrep, linked in-process,
so the pattern and its regex syntax are unchanged.

In a 2000-token budget it fits the file that declares your symbol as reliably
as `rg -C 10` does, and more of the files that use it (see
[Quality](#quality)). It is slower than plain ripgrep on a wide search, and it
leaves matches out on purpose; [when not to use this](#when-not-to-use-this)
covers both.

```console
$ rkgrep validate_token
src/auth/service.rs:142-159 (fn validate_token) [187 tok]
pub fn validate_token(token: &str) -> Result<Claims> {
    let claims = decode(token)?;
    ...
}

src/api/middleware.rs:61-74 (fn require_auth) [142 tok]
async fn require_auth(req: Request) -> Result<Response> {
    let claims = validate_token(req.header("authorization")?)?;
    ...
}
```

Docs: [CLI](docs/cli.md) · [Architecture](docs/architecture.md) ·
[Extraction](docs/extraction.md) · [Performance](docs/performance.md) ·
[Quality](docs/quality.md) · [Contributing](CONTRIBUTING.md)

## Features

- Declarations rank first: a file that declares the symbol outranks one that
  mentions it twenty times.
- A match expands to its declaration rather than ±N lines, and overlapping
  regions merge, so no line is sent twice.
- Spans pack under `--max-tokens` best-first: 2000 by default, at most 3 per
  file, so one crowded module cannot take the budget. The budget is counted in
  `o200k_base` tokens from a bundled vocabulary — the same unit the context
  window it is meant for charges.
- Four declaration shapes (keyword, receiver, signature, and
  callable-bound-to-a-name) cover Rust, Go, Python, C, C++, Java, C#,
  JavaScript, TypeScript, Ruby, PHP, and shell, with no parser per language.
  Comments and string literals are masked first, so a declaration written
  inside a docstring is never reported. See [Extraction](docs/extraction.md).
- `-C, --comments` inverts that mask and scopes a search to comments in any
  language. It filters rather than changing the unit, so a `TODO` arrives with
  the declaration it sits in.
- Declarations carry a nesting depth taken from indentation, which separates
  the `save` a module declares from the `save` an unrelated class has.
- `-e` is repeatable: several symbols, one walk, one budget. The budget is
  spent round-robin, so asking about three symbols answers all three before it
  answers any of them twice, and a span two patterns hit comes back once.
- `-d` answers *where is this defined* and `-r` answers *who uses this*, which
  is what a ranked mix is usually being read for one of.
- `--since main` searches only what a branch changed; `--files-from -` takes
  the file list on stdin.
- `-l` surveys anchors cheaply and `--fetch -` reads back the ones worth
  paying for, so a couple of hundred tokens decides how to spend a few
  thousand.

## Usage

```console
cargo install --git https://github.com/CoderDayton/rkgrep rkgrep
```

Then:

```console
rkgrep PATTERN [PATH]
```

| Flag | Meaning |
| --- | --- |
| `-e, --regexp PATTERN` | a pattern; repeat to ask about several at once |
| `-t, --max-tokens N` | budget for the whole result set (default 2000) |
| `-A, --no-budget` | every ranked span: no budget, no per-file cap |
| `--max-per-file N` | cap spans from any one file (default 3, 0 for no cap) |
| `-d`, `-r` | only declarations, only references |
| `--kind fn,class` | only these declaring kinds (implies `-d`) |
| `--min-references N` | keep budget for at least N non-declaration spans |
| `-g, --glob GLOB` | restrict to matching files, repeatable |
| `--files-from FILE` | search only these paths; `-` reads stdin |
| `--since REF` | search only what changed since `REF` |
| `-C, --comments` | match only inside comments, in any language |
| `-w`, `-i`, `-F` | whole words, ignore case, literal string |
| `--hidden`, `--no-ignore` | search hidden files; ignore `.gitignore` |
| `-l, --anchors-only` | anchors without source text |
| `-n, --line-numbers` | number the lines, marking the ones that matched |
| `--fetch ANCHOR` | return the lines an anchor names, from under the root; `-` reads stdin |
| `--vimgrep` | one line per matched line as `path:line:col:text`, every match |
| `--json` | machine-readable output |
| `--color auto\|always\|never` | colorize headers and matched lines |
| `--stats` | spans, tokens, and per-phase timings, to stderr |
| `--threads N` | worker threads (0 chooses automatically) |

```console
rkgrep -w handle_request src/        whole words, under src/
rkgrep -e Claims -e refresh          two symbols, one budget, answers alternating
rkgrep -d validate_token             only where it is declared
rkgrep -t 8000 -g '*.py' Config      bigger budget, Python only
rkgrep --since main -C TODO          comments in what this branch changed
rkgrep -l TODO | rkgrep --fetch -    survey cheaply, then read what matters
rkgrep --vimgrep parse_url           one jumpable line per matching line
rkgrep --json parse_url | jq .       structured output
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
| Python | `rg -C 10` | 0.986 | 95.8% | 80.3% |
| | **rkgrep** | **0.996** | **100.0%** | **86.6%** |
| TypeScript | `rg -C 10` | 0.956 | 95.0% | 84.6% |
| | **rkgrep** | **0.960** | **100.0%** | **95.6%** |
| Go | `rg -C 10` | 0.957 | 94.2% | 84.2% |
| | **rkgrep** | 0.954 | **97.5%** | **87.1%** |
| Rust | `rg -C 10` | 0.908 | 88.3% | 68.9% |
| | **rkgrep** | **0.947** | **98.3%** | **80.2%** |

Reproduce with `cargo bench --bench quality -- <repo> --lang <lang>`. The last
column counts files that textually reference the name, so it rewards returning
more files rather than better ones. [docs/quality.md](docs/quality.md) has the
ground-truth construction and the limits of each metric.

## Contributing

```console
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
cargo build --release                   # both benchmarks drive this binary
cargo bench --bench quality -- <repo> --lang rust
cargo bench --bench scaling -- <tree>
```

The first three have to pass before a pull request, and a change to ranking or
extraction should carry a quality run on more than one language.
[CONTRIBUTING.md](CONTRIBUTING.md) has what the benchmarks need and the
invariants that are not obvious from the code.

## License

[MIT](LICENSE)
