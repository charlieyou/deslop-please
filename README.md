# deslop-please

[![slop](https://img.shields.io/endpoint?url=https%3A%2F%2Fraw.githubusercontent.com%2Fdeterministic-machines%2Fdeslop-please%2Fbadges%2Fslop.json)](https://github.com/deterministic-machines/deslop-please/actions/workflows/ci.yml?query=branch%3Amain)
[![test](https://github.com/deterministic-machines/deslop-please/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/deterministic-machines/deslop-please/actions/workflows/ci.yml?query=branch%3Amain)
[![crates.io](https://img.shields.io/crates/v/deslop-please.svg)](https://crates.io/crates/deslop-please)

An opinionated linter to remove sloppy code. It follows [*A Philosophy of Software Design*](https://www.amazon.com/Philosophy-Software-Design-John-Ousterhout/dp/1732102201): 
**deep** modules, **narrow** interfaces. 


```bash
cargo install deslop-please
```

```bash
> deslop-please

error[field-leak]: `RunQueue::items` is known outside `RunQueue`: 4 places in 2 modules
  --> src/sched/policy.rs:7:1
  = note: src/sched/policy.rs:7:1: `runqueue::sched::policy::admit`
  = note: src/sched/policy.rs:12:1: `runqueue::sched::policy::next`
  = note: src/sched/report.rs:4:1: `runqueue::sched::report::backlog`
  = note: src/sched/report.rs:8:1: `runqueue::sched::report::drained`
    runqueue::queue::ring::RunQueue
      items ◀━┳━ 2  runqueue::sched::policy
              ┗━ 2  runqueue::sched::report
      also known outside: head
      kept: limit

deslop-please: 65535 errors, well done!
```

### It's not my fault 

Sure, let's just mute them:

```bash
deslop-please --update-allow .deslop-please.allow  
deslop-please --allow .deslop-please.allow 
```

The allowlist is a ratchet, not a rubber stamp. Each line allows one
report, `<lint> <key>`, and may cap it and say why:

```text
# Comment lines just above an entry belong to it, and move with it.
long-function core::plan::plan <= 609  # one walk over the plan; splitting it hides the order
tangle core::a::X,core::b::Y          # settled: the two are one state machine
over-public core::fixtures::* <= 12    # a group: up to 12 reports under this module
```

- `<= N` is a ceiling on what the report measures: the lines of a function,
  the parameters of a signature, the modules a module uses, the places that
  know a field, the fields of a struct that `over-public` reports as one,
  `<type>.fields`. A report above it is an error, and no update raises it.
  A circle (`tangle`, `module-cycle`, `duplicate-code`, `duplicate-struct`)
  is allowed while its members are among the entry's.
- `prefix::*` groups `over-public` or `test-only-pub` reports under a module,
  and needs a ceiling on how many there are, each field of a `.fields`
  report counted.
- `--update-allow` writes the current reports into the file, keeping its
  comments, reasons and ceilings, prints each entry it adds, and fails if a
  report is above its ceiling. With `[allow] ceilings = true` it writes each
  new entry with what it measures as its ceiling.
- `--prune-allow` only takes out: entries that allow nothing, ceilings above
  what they allow, entries that moved, and lines that list a key twice. It
  adds nothing.
- `--allow ... --strict-allow` fails on entries that allow nothing, ceilings
  above what they allow, entries that moved, and a key listed twice, whatever
  the ceilings and reasons of its lines. A circle that shrank still
  passes, though `--prune-allow` rewrites its key.
- With a crate or module, `deslop-please my-crate --allow ...` and the other
  allowlist flags read and change only the entries in its scope.
- `--format tsv` prints `lint<TAB>key<TAB>file:line` for each report, the key
  an entry needs.

So that parallel branches add entries without conflicts, let git merge the
file by union, in `.gitattributes`; `--strict-allow` and `--prune-allow` then
catch an entry that a merge brings back, or brings in twice:

```text
.deslop-please.allow merge=union
```

### Configuration

`deslop-please.toml` at the workspace's root tells the lints about the
codebase. Every key is optional, and without the file every lint keeps its
default. A key the tool does not know is an error, so a misspelt policy never
goes unnoticed.

```toml
# The crates by layer, lowest first: a normal dependency must go to a lower
# layer (`layer-violation`).
layers = [
  ["cards", "ids"],
  ["core"],
  ["cli"],
]
# A member in no layer is an error, not a crate whose dependencies go
# unchecked.
every-member-layered = true

# Crates outside the workspace that use it, by their directories relative to
# the workspace's root: an excluded member, or a crate of another repository.
# Each is indexed too (for the host, with default features), and what its
# code uses counts as a use from another crate, its tests' as a test's.
consumers = ["crates/gpu", "../app/crates/app-solver"]

# Lints that report nothing unless named here:
# - `rename-getter`: a method whose body is only one of its type's fields,
#   `self.f`, `&self.f` or `&self.f[..]`, under the field's name or another,
#   that adds nothing to reading the field: only tests call it, or nothing;
#   every caller can read the field, `pub` or in reach of its module; or its
#   type is a record, whose other fields are as visible as the getter and
#   whose only methods are getters.
# - `one-use-args`: a struct built in one place and read, field by field,
#   only by the one function it is built for.
# - `inner-split`: a private function called by one other, which takes no
#   type parameters and whose body is only that call.
# - `redundant-reexport`: a `pub use` in a public module of an item that
#   another public path of the crate already reaches: where it is defined,
#   or the shallowest `pub use` that names it there.
opt-in = ["rename-getter", "redundant-reexport"]

# Features that build code only for tests. Set, even empty, it makes an item
# that a `cfg` attribute builds only for `test` or one of these features test
# code, as `#[cfg(test)] mod` makes a module: `#[cfg(any(test, feature =
# "test-utils"))] pub fn fixture()` is no candidate for `test-only-pub`, and
# what it uses, tests use.
test-features = ["test-utils"]

[rename-getter]
# Report every getter of a private field, also a read-only accessor of a
# field that its type's constructors and methods maintain. Default false.
all-private-fields = true

# Hot code that numeric work makes wide and long, held to finite limits of
# its own: no lint is off for it. A path, relative to the workspace's root,
# names a file or a directory, and its last segment names `<segment>.rs`
# too; a `*` stands for any characters in a segment. A limit left out keeps
# its default.
[kernel]
paths = ["crates/simd/src/kernel", "crates/core/src/regret_*"]
most-params = 14       # wide-signature; default 7
most-lines = 300       # long-function, in code lines; default 150
most-file-lines = 6000 # long-file; default 2000
positional-trap = "bools" # report bool flags only, not runs of numbers; default "all"

# What tests may widen. By default a test's use never justifies visibility.
# With "justify", a test's use does, but only for an item that production
# code uses too, and only in the crates listed. Test fixtures belong in a
# module named `tests`, `test`, `test_*` or `*_tests`, which is test code.
[test-only-pub]
crates = ["core", "ids"]
external-tests = "justify" # tests of other packages, of `tests/` and `benches/`, and consumers', for `pub`
                           # "own-package": only the package's own `tests/` and `benches/`
crate-tests = "justify"    # tests elsewhere in the crate, for `pub(crate)`

# Where `Result<_, String>` is fine. By default no function may return it.
[string-error]
boundary-only = true                 # report only functions visible outside their module
application-crates = ["cli", "server"] # here report only `pub` functions

[high-fan-out]
# No `god-hub`: a module that `high-fan-out` reports notes how many modules
# depend on it, and the modules below it are no part of its fan-out.
fold-god-hub = true
# The most modules one module may use, from a crate's layer of `layers` up;
# below the lowest layer named, the default of 15 holds. With
# `fold-god-hub`, a hub, a module that 10 or more modules depend on, keeps
# the default under a looser limit.
most-by-layer = { core = 20, cli = 25 }
# The limit below the lowest layer named, and the one a hub keeps; default 15.
most-modules = 18
# A use of another crate's module counts as one of its top module,
# `crate::top_module`; default "module".
other-crates = "top-module"
# A module counts as its parent's when the parent is no crate root and holds
# no part of the module that uses it: `a::b::x` and `a::b::y` count as one,
# `a::b`. Default false.
fold-siblings = true

[field-leak]
# A struct whose fields are all `pub` is plain data, which guards nothing:
# `shared-mutable-bag` judges it, not `field-leak`. A `pub(crate)` or
# `pub(super)` field still guards it from others.
plain-data = true
# Code in a type's module is inside it for its private fields, as rustc
# lets it be; a private field it alone uses keeps the struct guarding.
home-module = true

[shared-mutable-bag]
# Code that mutates a value its function owns shares it with no one. A
# variable owns its value when its type, as rust-analyzer infers it, borrows
# nothing (no `&`, `*` or lifetime in it) and is no `Rc`, `Arc`, `RefMut` or
# guard: `let mut config = Config::new()` does, `s: &mut S` does not.
owned-locals = true
# A type deriving `Deserialize` is a record read from a file, whose fields
# its readers set one by one on purpose.
deserialized-records = true

[wide-struct]
passive-records = true # a struct with no method of its own that takes `self` may be wide
most-passive-fields = 24 # but no wider than this; unset, any width; `most-fields` or less, none
clap-derives = true    # so may one deriving clap's `Parser`, `Args` or `Subcommand`
most-fields = 20       # the most fields any other struct may hold; default 15

[wide-type]
most-functions = 20    # the most working functions a type may show; default 15

[wide-enum]
most-variants = 30     # default 25

[wide-signature]
most-params = 8        # outside the kernel; default 7
# A private function called from one place, not counting its calls of
# itself, may take any number: a struct of its parameters would be built
# once, for that one call.
single-call-site = true

[positional-trap]
# Outside a kernel set to "bools", a run of this many adjacent parameters
# of one type, whatever the type, is a trap too: `a: Vec<u32>, b: Vec<u32>,
# c: Vec<u32>`. Unset, only `bool`s and runs of 4 of one primitive are.
same-type-run = 3

[duplicate-code]
# Copies that each are `#[inline(always)]` or `#[target_feature]` and whose
# bodies call the CPU's intrinsics (`_mm*`, `arch::`) are SIMD helpers each
# kernel module keeps its own of. Copies in different crates are still
# reported: they could share one helper.
simd = true

[long-file]
skip-test-modules = true # an inline `#[cfg(test)] mod tests { .. }` counts no lines
skip-generated = true    # a file marked `@generated` in its first 5 lines is not measured

[allow]
ceilings = true     # `--update-allow` caps each new entry at what it measures
# A stale entry allows a new report that nothing else does with its lint,
# crate, last name and, for a member, type, or a circle no bigger that
# shares most of its members: the same debt, moved. A file's or
# directory's entry (`long-file`, `wide-directory`) follows no move.
# `--prune-allow` rewrites the entry, keeping its reason, and
# `--strict-allow` fails until then.
follow-moves = true
```

### Add to your `AGENTS.md`

```markdown
-  Run [`deslop-please`](https://github.com/deterministic-machines/deslop-please) to check for sloppy code structure.
-  Errors flagged by `deslop-please` must be fixed from first principles, don't do mechanical moves to hide them.
    - `module-cycle` & `tangle`: what is the indented layer of abstraction? Which module should be at which layer?
    - `field-leak` & `shallow-module`: Why we have these fields/functions at all? What are the patterns of use? Can they be hidden behind a narrow interface? Don't fix each leak one at a time, fix them holistically.
    - Draw a diagram to help user understand the before and proposed after structure.
```

## Why they are sloppy 

They are really just one single idea: **deep** modules, **narrow** interfaces.

Deep modules hide the attention for you and your agents, so you only need to care about the narrow interface.

### module-cycle

![module-cycle: before, modules kinds and socket use each other, though listen, Stream::new and BACKLOG_MOST only form a chain; after, BACKLOG_MOST moves into kinds and only socket uses kinds](docs/module-cycle.svg)


### tangle

![tangle: before, Tasks in kernel and Timers in its child module timers know each other, with no module cycle; after, tick in kernel calls both and Timers no longer knows Tasks](docs/tangle.svg)

### field-leak

![field-leak: before, functions in sched push to, index and bump the items and head of RunQueue; after, they call push, pop and backlog, and only RunQueue works on its fields](docs/field-leak.svg)

### shallow-module


![shallow-module: before, app calls 9 of the 15 functions of store; after, it calls new, load and summary, and the rest are hidden behind them](docs/shallow-module.svg)

## License

MIT
