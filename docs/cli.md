# CLI

Every invocation, the JSON record, and the exit codes.

```console
rkgrep PATTERN [PATH]
```

`PATTERN` is ripgrep's, unchanged — its regex syntax applies. `PATH` is a file
or directory and defaults to the current one.

- [Flags](#flags)
- [Output](#output)
- [JSON](#json)
- [Stats](#stats)
- [Exit codes](#exit-codes)

## Flags

| Flag | Default | Meaning |
| --- | --- | --- |
| `-t, --max-tokens N` | 2000 | budget for the whole result set |
| `--max-per-file N` | 3 | cap spans from any one file |
| `-g, --glob GLOB` | — | restrict to matching files, repeatable |
| `-F, --fixed-strings` | off | treat the pattern as a literal string |
| `-w, --word-regexp` | off | match whole words only |
| `-i, --ignore-case` | off | case-insensitive matching |
| `--hidden` | off | search hidden files and directories |
| `--no-ignore` | off | do not respect `.gitignore` and friends |
| `-l, --anchors-only` | off | print anchors without source text |
| `--json` | off | emit JSON |
| `--color auto\|always\|never` | auto | colorize headers |
| `--stats` | off | timings and budget use, to stderr |
| `--threads N` | 0 | worker threads; 0 chooses automatically |

Tokens are counted as identifier and number runs — one unit per run, a single
byte scan. It is an estimate, close enough to a real tokenizer to budget
against.

## Output

```console
$ rkgrep -w estimate_tokens src -t 200
spans.rs:541-554 (fn estimate_tokens) [45 tok]
pub fn estimate_tokens(text: &str) -> usize {
    let mut count = 0usize;
    ...
}
```

The header is `path:start-end`, then the declaring kind and symbol when the
span has one, then its token cost. Paths are relative to the search root.

`-l` prints headers only, which is the form to pipe when the question is
*where* rather than *what*.

## JSON

`--json` emits one array of span objects, in rank order.

```json
[
  {
    "path": "spans.rs",
    "start_line": 468,
    "end_line": 519,
    "symbol": "declarations",
    "kind": "fn",
    "match_lines": [468, 488, 518],
    "is_declaration": true,
    "depth": 0,
    "tokens": 242,
    "score": 2.886294361119891,
    "text": "pub fn declarations(content: &str) -> Vec<Declaration> {\n..."
  }
]
```

| Field | Meaning |
| --- | --- |
| `path` | relative to the search root, forward slashes on every platform |
| `start_line`, `end_line` | 1-based, inclusive |
| `symbol`, `kind` | declaration the span came from; `null` for a fixed window |
| `match_lines` | every matched line inside the span, ascending |
| `is_declaration` | a match landed on the declaration line itself |
| `depth` | nesting depth; 0 is top level |
| `tokens` | what this span charged against the budget |
| `score` | ranking score; comparable within one result set, not across queries |
| `text` | the span's source |

An empty result is an empty array, never absent output.

## Stats

`--stats` writes two lines to stderr, leaving stdout clean for a pipe:

```console
$ rkgrep function ~/src --stats >/dev/null
rkgrep: 7 spans, 1998/2000 tokens, 50.0ms total
rkgrep: walk 31.2ms (15526 files matched), rank 4.0ms, extract 12.9ms (8 files read)
```

The two counts are the ones to read first: files matched versus files read. See
[Performance](performance.md#the-phase-model).

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | something matched |
| `1` | nothing matched |
| `2` | bad pattern, bad glob, or a path that does not exist |

As grep does, so `rkgrep -l TODO && echo found` works.
