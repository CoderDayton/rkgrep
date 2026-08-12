# Extraction

How a declaration is recognized, and what that recognition is worth.

The unit rkgrep returns is a declaration, not a fixed window of N lines: a
function is something a reader understands, `±10 lines` is an arbitrary cut
through one. Finding declarations without a parser per language is what makes
that unit available on anything ripgrep can match.

- [Masking](#masking)
- [The four shapes](#the-four-shapes)
- [Precedence](#precedence)
- [Nesting](#nesting)
- [Spans](#spans)
- [What it misses](#what-it-misses)

## Masking

Comments and string literals are blanked before any matching happens, so a
declaration written inside either is never reported. Without it, a Python
docstring that mentions `class Ghost` produces a declaration, and that one
bogus entry swallows every real declaration after it — the span it opens runs
until the next recognized declaration.

Masked bytes become spaces rather than being removed. Byte offsets and newline
positions survive, so line numbers taken from the masked text are valid against
the original.

The scanner handles `"`, `'`, `"""`, `'''`, `/* */`, `//`, and `#`.
Single-quoted forms stop at a newline, so an apostrophe in prose cannot swallow
the rest of the file. Triple-quoted forms are matched before single ones,
because mispairing them masks the code *after* a docstring instead of the
docstring itself.

`#` is the ambiguous marker: it opens a comment in shell, Python, Ruby and
YAML, and opens a Rust attribute, a C preprocessor directive or a CSS color
elsewhere. It is read as a comment unless what follows it is `[`, `![`, one of
the thirteen preprocessor directives, or 3, 4, 6, or 8 hex digits. A shebang
counts as a comment; `#![allow(..)]` does not.

Inverting the mask — blanking everything *except* the comments — is what
`--comments` searches against. Offsets are preserved the same way, so a line
number resolved against it is the line number in the file.

It is a scanner rather than a regex because pairing triple quotes correctly
needs lookahead the `regex` crate does not offer. Quotes and comment markers are
ASCII and a UTF-8 continuation byte is never equal to one, so scanning bytes
cannot begin a literal mid-character.

## The four shapes

| Shape | Example | Recognized as |
| --- | --- | --- |
| Keyword, after optional qualifiers | `pub fn search(p: &str)`, `export default class A` | the keyword |
| Receiver before the name | `func (r *Repo) Save(ctx) error` | `func` |
| Signature opening a body | `int main(void) {`, `async loadUser(id) {`, `deploy() {` | `function` |
| Callable bound to a name | `handler = lambda req: …`, `Api.prototype.load = function () {` | `function` |

**Keyword** is the common case: zero or more qualifiers (`pub`, `async`,
`export`, `static`, …) followed by a declaring keyword (`fn`, `def`, `class`,
`struct`, `interface`, `mod`, `var`, …) followed by the name. Qualifiers are consumed
greedily and then given back one at a time, because `const` and `static` are
both qualifier and keyword — `const static x` declares `x`, not `static`.

The keyword must open its line. Otherwise `from enum import Enum` reads as
declaring a symbol named `import`.

A qualifier may carry a parenthesized scope: `pub(crate) fn`, `pub(super)
struct`, `pub(in crate::util) fn`. The group is skipped and the run continues,
because ending it at the `(` would reject the whole line. Only a qualifier gets
this — skipping parentheses after any word would read `if (ready(x)) {` as a
declaration.

A keyword may carry generic parameters the same way: `impl<T> Repo<T>`,
`impl<'a, F: Fn() -> u32> Runner<F>`. The bracketed group is skipped, and a `>`
following `-` or `=` is stepped over rather than counted, so an arrow inside a
bound does not close the list early. Only a keyword gets this, so `if a<b &&
c>(d) {` stays a comparison.

**Receiver** exists because Go puts the receiver between the keyword and the
name, so the keyword rule — which wants the name next — sees nothing at all.

**Signature** is most of C, C++, Java, C#, shell, and every method body in a
JavaScript class: a name, a parameter list that closes, and a brace opening the
body on the same line. The brace is what separates the definition
`compute(x) {` from the call `compute(x);` one line below it. Three guards keep
it honest:

- The name may not be a control-flow word, so `if (ready(x)) {` declares
  nothing.
- Nothing in front of the name may be a control-flow word or a character that
  implies a call — a dot, an equals sign, a brace. `promise.then(function () {`
  is rejected on the dot.
- The parameter list must close on the same line. `callback(err, function () {`
  opens two groups and closes one, so it is a call with a callback in it.

**Callable bound to a name** takes its name from the left of an assignment and
its evidence from the right: `function`, `func`, `fn`, `lambda`, `def`,
`async`, or an arrow. `total = compute(x)` is not a declaration, because
`compute(x)` is a call rather than a callable.

## Precedence

Keyword first, then receiver if the keyword rule found nothing, then signature,
then assignment.

Signature overrides a keyword match when the two disagree on the name. The
keyword rule reads `static int compute_hash(const char *s) {` as declaring
`int` — it cannot tell a return type from a name — and the signature the line
actually opens can. When both agree, the keyword wins and keeps its kind, so
`class Repo(db: Db) {` stays a class rather than becoming a function.

The override applies only to a name found *behind* the keyword's own. The name
a line declares is the last one it offers, and a signature in front of that is
part of what the keyword rule already consumed: the qualifier in `pub(crate)
trait Kill {`, the bound in `impl<F: Fn() -> u32> Runner<F> {`. Without that,
every crate-visible Rust type is a declaration named `pub` and every generic
impl declares the trait in its bound. `static void open(int fd) {` and `static
int compute_hash(…) {` both name theirs behind the keyword, so the signature
still wins, and a qualifier-shaped name with no keyword on the line is still a
signature — C's `open(int fd) {` keeps its name.

## Nesting

Depth comes from indentation: the enclosing declarations are the ones still
open at a shallower indent, which a stack of open indents resolves in one pass.

```python
def top_level():        # depth 0
class Repo:             # depth 0
    def save(self):     # depth 1
        def inner():    # depth 2
```

An `impl` block is in the table — nesting depth is read from it, and the methods
inside one are reached through it — but it does not *declare* the name it
carries. `impl Repo` re-opens a type its `struct` declares, and `impl Store for
Repo` names a trait declared somewhere else again; an impl block names its type
more often than the declaration does, so it wins on match count whenever it is
allowed to compete. See [Architecture](architecture.md#ranking).

Depth is what separates the `save` a module declares from the `save` some
unrelated class happens to have. Both declare the name; only nesting tells
them apart. Ranking charges for it — but only against a declaration that a
shallower declaration *of the same name* is competing with, since penalizing
depth unconditionally also demotes methods nothing competes with, and a
top-level span is the larger of the two. See
[Architecture](architecture.md#ranking).

A language that does not indent its bodies reports everything at depth 0, and
nothing is reordered. C functions all start at column zero.

## Spans

A declaration's span ends at whichever comes first: the end of the body it
opens, or the line before the first declaration nested inside it.

The body is the run of lines indented deeper than the declaration itself, plus
a closing line — `}`, `};`, `]),`, or `end` — sitting back at its own indent. A
blank line neither ends a body nor extends one, so a paragraph break inside a
function does not close it and a span does not trail off into the gap below its
last statement.

Every declaration's body is resolved in one pass over the file — a stack of
still-open indents, read backwards — rather than one forward scan per
declaration. A file whose indentation only ever deepens would otherwise make
each scan walk nearly the whole remainder, squaring the cost with the file's
length.

Stopping at the first nested declaration is what keeps a class to its header.
The body of a class is its methods, and each of those is already a span in its
own right, so returning the class whole returns them a second time and spends a
whole budget on one file.

The two rules answer different failure modes. Without the body rule, the last
declaration in a file runs to the end of it, and a module whose tail is a data
literal produces one span larger than any budget — which the packer drops,
leaving a query that matched reporting nothing at all. Without the nesting
rule, every container is returned in full.

A declaration whose body is still larger than the budget could admit is
reported as a ±6-line window into it rather than dropped. A match on its own
first line keeps the name, so "where is this declared" still ranks as a
declaration.

A match outside every declaration — an import, a top-level constant, a class
constant written below the last method — gets the same ±6-line window.

Files above 8 MiB are passed over rather than read. Extraction holds a file
whole, and one worker per core does it at once, so the ceiling on a file is
also the ceiling on memory. A run that skipped any says so on stderr:

```console
rkgrep: skipped 3 files over 8 MiB
```

## What it misses

- **A body opened in Allman style.** The signature shape needs the brace on the
  same line as the name, so `void Repo::save(int x)` followed by `{` on its own
  line is read as ordinary code.
- **Anonymous callables.** `export default function () {` declares nothing,
  correctly — there is no name — but so does a default export that a reader
  would name after its file.
- **C++ member initializer lists.** `Foo::Foo(int x) : base_(x) {` has
  parentheses between the parameter list and the brace, which the signature
  rule cannot read.
- **Scope beyond indentation.** Two same-named methods in different files are
  two declarations at the same depth, and nothing distinguishes them. Depth
  separates a module-level definition from a method; it does not separate two
  methods.
- **Kinds are the declaring keyword, not a type system.** The signature and
  assignment shapes report `function` for everything they find, including C++
  methods and JavaScript getters.
- **A lifetime reads as a string literal.** Masking cannot tell Rust's `'a`
  from a single-quoted string without knowing the language, and unmasking it
  would leave every Python and JavaScript string open. `impl<'a> Repo<'a>` is
  masked past its name and reports no declaration; `impl<T> Repo<T>` is found.
- **A nested function ends its parent early.** `def outer` containing `def
  inner` stops at `inner`, so the lines of `outer` below `inner` belong to
  neither and fall back to a window.
- **The C preprocessor.** `#define MAX_RETRIES 5` survives masking, but
  `define` is not a declaration keyword, so a header of macros has no
  declarations.

Every one of these is a recognition gap, not a correctness bug: a missed
declaration falls back to a fixed window, which is what a plain `rg -C` would
have given.
