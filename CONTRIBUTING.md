# Contributing

Bug reports are welcome, as are benchmark numbers from other hardware and
declaration shapes for languages that currently fall through to fixed windows.

## Before a pull request

```console
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

All three have to pass. `cargo build --release` as well if you are running the
benchmarks, since both drive the release binary as a subprocess.

## Benchmarks

```console
cargo bench --bench quality -- <repo> --lang rust
cargo bench --bench scaling -- <tree>
```

Both need a repository to run against. The quality harness needs sources in a
language it has a grammar for (`python`, `typescript`, `javascript`, `go`, or
`rust`) and takes its ground truth from that grammar rather than from rkgrep's
own extractor, so the extractor is never scored against its own notion of a
declaration. The scaling harness needs `rg` on `PATH` for the baseline.

Any change to ranking or extraction should carry a quality run on more than one
language. The metrics move in opposite directions, and terms that are inert on
one language carry another, so a single corpus can hide both. `--show-failures
N` lists the queries where the baseline ranks the declaration higher.

## Read first

- [docs/architecture.md](docs/architecture.md): the pipeline, the candidate
  cut, and the ranking formula.
- [docs/extraction.md](docs/extraction.md): the four declaration shapes,
  nesting, and what they miss.
- Read [docs/performance.md#measured-dead-ends](docs/performance.md#measured-dead-ends)
  before optimizing the search path. mmap, buffer reuse, and
  channel-versus-mutex were each implemented and measured, and two were
  pessimizations.
- [docs/plans/](docs/plans): intent for work not yet started.

Diagnose a slow query with `--stats` rather than by guessing. It splits the
query into walk and rank/extract, and the two phases have opposite fixes.

## Invariants

These are load bearing and not obvious from the code.

- `KEYWORDS`, `MODIFIERS`, and `CONTROL_WORDS` in `src/spans.rs` are
  binary-searched, so they have to stay sorted. `tables_are_sorted_for_binary_search`
  enforces it.
- Masking runs before matching, and masked bytes become spaces rather than
  being removed, so line numbers taken from masked text stay valid against the
  original. See [Masking](docs/extraction.md#masking).
- The scout pass has its own `Sink` (`ScoutSink` in `src/search.rs`) because
  `grep_searcher::sinks::UTF8` errors when line numbers are disabled, which
  skips every file and looks fast rather than failing.

## License

Contributions are licensed under [MIT](LICENSE), the same as the project.
