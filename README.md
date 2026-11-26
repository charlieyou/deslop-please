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

# Crates outside the workspace that use it, by their directories relative to
# the workspace's root: an excluded member, or a crate of another repository.
# Each is indexed too (for the host, with default features), and what its
# code uses counts as a use from another crate, its tests' as a test's.
consumers = ["crates/gpu", "../app/crates/app-solver"]

# Features that build code only for tests. Set, even empty, it makes an item
# that a `cfg` attribute builds only for `test` or one of these features test
# code, as `#[cfg(test)] mod` makes a module: `#[cfg(any(test, feature =
# "test-utils"))] pub fn fixture()` is no candidate for `test-only-pub`, and
# what it uses, tests use.
test-features = ["test-utils"]

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
# below the lowest layer named, the default of 15 holds.
most-by-layer = { core = 20, cli = 25 }
# A use of another crate's module counts as one of its top module,
# `crate::top_module`; default "module".
other-crates = "top-module"

[field-leak]
# A struct with no private field is plain data, which guards nothing:
# `shared-mutable-bag` judges it, not `field-leak`.
plain-data = true
# Code in a type's module is inside it for its private fields, as rustc
# lets it be; a private field it alone uses keeps the struct guarding.
home-module = true

[long-file]
skip-test-modules = true # an inline `#[cfg(test)] mod tests { .. }` counts no lines
skip-generated = true    # a file marked `@generated` in its first 5 lines is not measured
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
