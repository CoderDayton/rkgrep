# CLI

Every invocation, the JSON record, and the exit codes.

```console
rkgrep PATTERN [PATH]
rkgrep -e PATTERN -e PATTERN [PATH]
```

`PATTERN` is ripgrep's, unchanged — its regex syntax applies. `PATH` is a file
or directory and defaults to the current one.

- [Flags](#flags)
- [Several patterns](#several-patterns)
- [Choosing what comes back](#choosing-what-comes-back)
- [Choosing where to look](#choosing-where-to-look)
- [Fetching by anchor](#fetching-by-anchor)
- [Output](#output)
- [JSON](#json)
- [Stats](#stats)
- [Exit codes](#exit-codes)

## Flags

`--help` groups these the same way.

| Flag | Default | Meaning |
| --- | --- | --- |
| `-e, --regexp PATTERN` | — | a pattern; repeat to ask about several at once |
| **Search** | | |
| `-F, --fixed-strings` | off | treat the pattern as a literal string |
| `-w, --word-regexp` | off | match whole words only |
| `-i, --ignore-case` | off | case-insensitive matching |
| `-C, --comments` | off | match only inside comments, in any language |
| **Scoping** | | |
| `-g, --glob GLOB` | — | restrict to matching files, repeatable |
| `--files-from FILE` | — | search only the files listed in `FILE`, or stdin for `-` |
| `-0, --null` | off | `--files-from` entries are NUL-separated |
| `--since REF` | — | search only what changed since `REF` |
| `--hidden` | off | search hidden files and directories |
| `--no-ignore` | off | do not respect `.gitignore` and friends |
| **Selection** | | |
| `-d, --declarations` | off | only spans that declare the symbol |
| `-r, --references` | off | only spans that do not |
| `--min-references N` | 0 | keep budget for at least N non-declaration spans |
| `--kind KIND` | — | only these declaring kinds, comma-separated; implies `-d` |
| **Budget** | | |
| `-t, --max-tokens N` | 2000 | budget for the whole result set; no default under `--vimgrep` |
| `-A, --no-budget` | off | return every ranked span, no budget and no per-file cap |
| `--max-per-file N` | 3 | cap spans from any one file; 0 for no cap |
| **Output** | | |
| `-l, --anchors-only` | off | print anchors without source text |
| `-n, --line-numbers` | off | number every line, marking the ones that matched |
| `--vimgrep` | off | one line per matched line as `path:line:col:text` |
| `--json` | off | emit JSON |
| `--color auto\|always\|never` | auto | colorize headers and matched lines |
| `--fetch ANCHOR` | — | return the lines an anchor names; `-` reads stdin |
| **Performance** | | |
| `--stats` | off | timings and budget use, to stderr |
| `--threads N` | 0 | worker threads; 0 chooses automatically |

Tokens are `o200k_base` tokens, so a budget is denominated in the same unit the
context window it is meant for charges. The counts are byte-identical to
tiktoken's for the same text, which the test suite checks against a reference
implementation over the whole vocabulary and every source file in the tree.

## Several patterns

`-e` is repeatable, and every pattern shares one budget.

```console
rkgrep -e validate_token -e Claims -e refresh src/ -t 4000
```

The tree is walked once, whatever the number of patterns: they are joined into
one alternation for the walk, and told apart again on the files that survive
ranking. `-F` escapes each pattern rather than the joined text, and `-w` wraps
the whole alternation.

The budget is spent round-robin, best first: the top span for the first
pattern, then the second, then the third, then back to the first. A pattern
that runs out drops from the rotation and the others take its share. Asking
about three symbols answers all three before it answers any of them twice.

Each span belongs to exactly one pattern — the one it declares, or failing that
the one with the most matched lines inside it, ties going to the earliest `-e` —
so a function both patterns hit comes back once rather than twice under two
headings. The header names it, and
only when more than one pattern was given:

```console
$ rkgrep -e Claims -e refresh src -t 400 -l
src/auth.py:9-10 (class Claims) [7 tok] for Claims
src/auth.py:5-6 (def refresh) [10 tok] for refresh
```

A pattern that matched nothing is named on stderr, because with several of them
a typo in one is otherwise invisible behind the answers to the others.

## Choosing what comes back

`-d` returns only spans that declare a queried symbol, and `-r` only spans that
do not: *where is this defined* and *who uses this*, which is what the ranked
mix is usually being read for one of. `-d` is also cheaper, because
non-declaration spans never enter the budget.

`--kind fn,class` keeps only those declaring kinds and implies `-d`, since a
non-declaration span has no kind. The kind is the declaring keyword, not a type
system: the signature and assignment shapes report `function` for everything
they recognize, so `--kind class` finds a Python `class` and misses a C++ one
written as a signature. See [Extraction](extraction.md#the-four-shapes).

`--min-references N` guarantees at least N non-declaration spans survive, for
when a large definition would otherwise take the whole budget. It is met by
giving up declarations, lowest-ranked first, so the best answer is the last
thing surrendered; a query with no references to promote keeps every
declaration it had.

`--no-budget` turns the budget off entirely: every ranked span comes back, and
extraction runs on every matching file rather than only on the ones that could
fit. Use it when rkgrep is standing in for `rg` and dropping a match is worse
than a long result — ranking, span expansion and merging all still apply, so
the output is ordered and deduplicated rather than raw. It also lifts the
per-file cap, which exists only to stop one crowded module taking a budget;
passing `--max-per-file N` alongside it puts a cap back. `--no-budget` and
`-t` are mutually exclusive, so a run can never claim both.

## Choosing where to look

`--files-from` searches only the paths listed in a file, or on stdin for `-`.
`-0` switches the separator to NUL for paths containing newlines. An empty list
searches nothing.

```console
git ls-files '*.rs' | rkgrep --files-from - Options
```

`--since REF` fills the same list from git: everything this branch changed since
it left `REF`, plus files git does not track yet. The working tree is compared
against the point the two histories diverged rather than against `REF` itself,
so commits made on `REF` after the split stay out of the list. `--since HEAD` is
uncommitted work and `--since main` is the branch.

```console
rkgrep --since main -C TODO         comments in what this branch changed
```

An explicitly listed path is searched whether or not `.gitignore` covers it:
the caller has already chosen the files. Globs still apply.

## Fetching by anchor

`--fetch` returns the lines an anchor names, with no pattern and no search. The
anchors are the ones rkgrep prints, so a cheap survey and a paid-for read
compose:

```console
rkgrep -l TODO | rkgrep --fetch -
rkgrep --fetch src/auth/service.rs:142-159
```

That is two hundred tokens to decide how to spend four thousand. Anchors are
relative to the root they were surveyed under, so the same path follows them
back: `rkgrep -l TODO src | rkgrep --fetch - src`. `--json` and `-t` apply, and
the lines come back as asked for rather than re-snapped to declaration bounds.

An anchor naming a file outside that root is refused, whether it climbs out
with `..`, arrives already absolute, or comes in on stdin. Only the range asked
for is held, so fetching six lines costs six lines however large the file
holding them.

## Output

```console
$ rkgrep -w declaration_name src -t 200
spans/scan.rs:359-363 (fn declaration_name) [45 tok]
pub fn declaration_name(line: &str) -> Option<&str> {
    scan_declaration(line)
        .filter(|(kind, _)| kind_declares(kind))
        .map(|(_, name)| name)
}
...
```

The header is `path:start-end`, then the declaring kind and symbol when the
span has one, then its token cost. Paths are relative to the search root.

`-l` prints headers only, which is the form to pipe when the question is
*where* rather than *what*.

When output is a terminal, the lines that matched are highlighted. No character
is added for it, because rkgrep output gets pasted into editors and into
context windows and a marker in the margin would corrupt every paste.

`-n` is the opt-in for a gutter. It numbers every line and separates a matched
line with `:` and any other with `-`, as grep does:

```console
$ rkgrep -n -d -w declaration_name src
spans/scan.rs:359-363 (fn declaration_name) [45 tok]
   359:pub fn declaration_name(line: &str) -> Option<&str> {
   360-    scan_declaration(line)
   361-        .filter(|(kind, _)| kind_declares(kind))
   362-        .map(|(_, name)| name)
   363-}
```

`--vimgrep` prints one line per matched line as `path:line:col:text`, which is
what quickfix, fzf and every editor's grep integration read. It is a different
unit from a span on purpose; the column costs a matcher call per matched line
and is resolved only when this flag asks for it.

```console
$ rkgrep --vimgrep -w Region src | head -2
src/search/region.rs:20:19:pub(super) struct Region {
src/search/region.rs:33:6:impl Region {
```

Being a line unit rather than a span unit changes three things:

- **No budget.** Enumerating matches and filling a context window are different
  questions, and a budget would answer the second one silently. `-t` still
  applies a budget when asked for explicitly, and `--max-per-file` still caps.
- **Paths as typed.** A span header is relative to the search root, which is
  what lets `-l` output feed `--fetch`. Quickfix knows no such root, so
  `--vimgrep` prefixes the path the search was given: `rkgrep --vimgrep x src`
  prints `src/…`, openable from the shell that ran it.
- **Ordered by path, then line**, rather than by rank, so a file arrives as one
  ascending run.

One line per matched *line*, not per match: a line matching twice is printed
once, at its first column. `rg --vimgrep` prints it twice.

## JSON

`--json` emits one array of span objects, in rank order.

```json
[
  {
    "path": "spans/mod.rs",
    "start_line": 65,
    "end_line": 71,
    "symbol": "declarations",
    "kind": "fn",
    "match_lines": [
      65
    ],
    "is_declaration": true,
    "depth": 0,
    "query": "declarations",
    "tokens": 75,
    "score": 2.1931471805599454,
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
| `match_columns` | 1-based column of each of those matches; present only under `--vimgrep` |
| `is_declaration` | a match landed on the declaration line itself |
| `depth` | nesting depth; 0 is top level |
| `query` | the pattern this span answers, as it was written |
| `tokens` | what this span charged against the budget |
| `score` | ranking score; comparable within one result set, not across queries |
| `text` | the span's source |

An empty result is an empty array, never absent output.

## Stats

`--stats` writes three lines to stderr, leaving stdout clean for a pipe:

```console
$ rkgrep function ~/src --stats >/dev/null
rkgrep: 8 spans, 1985/2000 tokens, 24.8ms total
rkgrep: walk 7.6ms (68 files matched), rank 0.1ms, extract 14.1ms (8 files read)
rkgrep: pack 2.6ms, render 0.0ms
```

The two counts are the ones to read first: files matched versus files read. The
five phases account for the whole query, so a total far above their sum is
startup, not search. See [Performance](performance.md#the-phase-model).

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | something matched |
| `1` | nothing matched |
| `2` | bad pattern, bad glob, bad anchor, an anchor outside the root, or a path that does not exist |

A closed pipe is not a failure: `rkgrep … \| head` exits on what it matched. Any
other write that fails is reported and exits `2`, so a truncated file never
passes for a complete one.

As grep does, so `rkgrep -l TODO && echo found` works.
