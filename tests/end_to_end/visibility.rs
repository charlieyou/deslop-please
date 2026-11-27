//! What `over-public` may ask to narrow: not what a `macro_rules!` body
//! uses where it is invoked, not what an interface rustc checks names, and
//! not a field whose narrowing would make `field-leak` report its struct.
//! A struct's fields are one report, whose ceiling caps how many.

use super::{keys, private_pair, run, scratch_workspace};

fn over_public(keys: Vec<String>) -> Vec<String> {
    keys.into_iter()
        .filter(|key| key.starts_with("over-public "))
        .collect()
}

#[test]
fn what_a_macro_body_names_is_used_where_the_macro_is_invoked() {
    let lib = "//! Core.\nmod glob;\nmod mccfr;\nmod terminal;\nmod tree;\nmod user;\n\
               pub fn root_helper() -> u32 { 3 }\n\
               pub fn run() -> u64 {\n    \
               u64::from(terminal::triple::value() + terminal::quad::quad() + user::user())\n        \
               + tree::program::bake() + mccfr::entry::solve()\n}\n";
    let workspace = private_pair(
        "macro_bodies_over_public",
        lib,
        "fn main() { println!(\"{}\", core_t::run()); }\n",
        &[
            (
                "core/src/terminal/mod.rs",
                "pub(crate) mod pair;\npub(crate) mod quad;\npub(crate) mod triple;\n",
            ),
            // `quad`'s `twice!` is its own, and its body's `total` a local;
            // `$crate::root_helper` is the crate root's.
            (
                "core/src/terminal/quad.rs",
                "pub(crate) fn total(x: u32) -> u32 { x * 4 }\n\
                 macro_rules! twice { ($x:expr) => {{ let total = $x; total + total + $crate::root_helper() }} }\n\
                 pub(crate) fn quad() -> u32 { twice!(1) + total(1) }\n",
            ),
            // `pick!`'s `picked` is what `user` sees through its glob.
            (
                "core/src/glob/mod.rs",
                "pub(crate) mod defs;\npub(crate) mod mac;\n",
            ),
            (
                "core/src/glob/defs.rs",
                "pub(crate) fn picked() -> u32 { 5 }\npub(crate) fn twice() -> u32 { picked() * 2 }\n",
            ),
            (
                "core/src/glob/mac.rs",
                "macro_rules! pick { () => { picked() } }\npub(crate) use pick;\n",
            ),
            (
                "core/src/user.rs",
                "use crate::glob::defs::*;\n\
                 pub(crate) fn user() -> u32 { crate::glob::mac::pick!() + twice() }\n",
            ),
            // `lonely` is used only in `pair`, by a macro of its own: that
            // use stays inside, and it can still be private.
            (
                "core/src/terminal/pair.rs",
                "pub(super) fn fill(x: u32) -> u32 { x + 1 }\n\
                 pub(super) fn lonely(x: u32) -> u32 { x * 2 }\n\
                 macro_rules! twice { ($x:expr) => { lonely($x) + lonely($x) } }\n\
                 pub(super) fn pair_value() -> u32 { fill(1) + twice!(2) }\n",
            ),
            (
                "core/src/terminal/triple.rs",
                "use super::pair::{fill, pair_value};\n\
                 struct Filled { fill: u32 }\n\
                 macro_rules! filled { ($x:expr) => { Filled { fill: fill($x) }.fill } }\n\
                 pub(crate) fn value() -> u32 { filled!(3) + pair_value() }\n",
            ),
            (
                "core/src/tree/mod.rs",
                "pub(crate) mod program;\npub(crate) mod table;\n",
            ),
            // `widen!` is invoked in `program`, `here!` only in `table`.
            (
                "core/src/tree/table.rs",
                "pub(crate) trait Value: Copy { fn wide(self) -> u64; }\n\
                 impl Value for u32 { fn wide(self) -> u64 { u64::from(self) } }\n\
                 pub(crate) fn only_here() -> u64 { 1 }\n\
                 macro_rules! widen { ($x:expr) => { $crate::tree::table::Value::wide($x) } }\n\
                 pub(crate) use widen;\n\
                 macro_rules! here { () => { $crate::tree::table::only_here() } }\n\
                 pub(crate) fn deep() -> u64 { 2 }\n\
                 macro_rules! deepen { () => { $crate::tree::table::deep() } }\n\
                 macro_rules! outer { () => { deepen!() } }\n\
                 pub(crate) use outer;\n\
                 pub(crate) fn local() -> u64 { here!() }\n",
            ),
            (
                "core/src/tree/program.rs",
                "pub(crate) fn bake() -> u64 { crate::tree::table::widen!(7u32) + crate::tree::table::local() }\n",
            ),
            (
                "core/src/mccfr/mod.rs",
                "pub(crate) mod entry;\npub(crate) mod walk;\n",
            ),
            (
                "core/src/mccfr/walk.rs",
                "pub(super) fn walk<T: Default>(depth: u64) -> u64 { let _ = T::default(); depth }\n\
                 pub(super) fn warm() -> u64 { walk::<u64>(0) }\n",
            ),
            (
                "core/src/mccfr/entry.rs",
                "use super::walk::walk;\n\
                 pub(crate) fn solve() -> u64 {\n    \
                 macro_rules! dispatch { ($t:ty) => { walk::<$t>(1) } }\n    \
                 dispatch!(u8) + dispatch!(u16) + super::walk::warm() + crate::tree::table::outer!()\n}\n",
            ),
        ],
    );
    assert_eq!(
        over_public(keys(&workspace)),
        [
            "over-public core_t::root_helper",
            "over-public core_t::terminal::pair::lonely",
            "over-public core_t::terminal::quad::total",
            "over-public core_t::tree::table::only_here",
        ]
    );
}

#[test]
fn what_an_associated_type_of_a_trait_impl_names_is_as_visible_as_the_impl() {
    let lib = "//! Core.\nuse std::str::FromStr;\nmod adapter;\nmod parse;\nmod semantics;\n\
pub trait Storage { type Stored; fn store(&self) -> Self::Stored; }
pub struct Game;
pub struct Handle { pub world: u32 }
impl Storage for Game { type Stored = Handle; fn store(&self) -> Handle { Handle { world: 1 } } }
pub fn world(game: &Game) -> u32 { game.store().world + adapter::remap(&adapter::Adapter) }
pub struct Unknown;
pub enum Kind { A }
impl FromStr for Kind {
    type Err = Unknown;
    fn from_str(s: &str) -> Result<Kind, Unknown> { if s == \"a\" { Ok(Kind::A) } else { Err(Unknown) } }
}
pub struct UnknownColour;
pub mod errors { pub struct BadShade; }
macro_rules! vocabulary {
    ($name:ident) => {
        pub enum $name { One }
        impl FromStr for $name {
            type Err = UnknownColour;
            fn from_str(_: &str) -> Result<Self, Self::Err> { Ok(Self::One) }
        }
    };
}
macro_rules! shades {
    ($name:ident) => {
        pub enum $name { One }
        impl FromStr for $name {
            type Err = Box<$crate::errors::BadShade>;
            fn from_str(_: &str) -> Result<Self, Self::Err> { Ok(Self::One) }
        }
    };
}
vocabulary!(Colour);
shades!(Shade);
pub struct BadNumber;
pub enum Number { One }
macro_rules! parse_for {
    ($t:ty) => {
        impl FromStr for $t {
            type Err = BadNumber;
            fn from_str(_: &str) -> Result<Self, Self::Err> { Ok(Self::One) }
        }
    };
}
parse_for!(Number);
pub struct UnknownHue;
mod hues {
    macro_rules! hues {
        ($name:ident) => {
            pub enum $name { One }
            impl std::str::FromStr for $name {
                type Err = $crate::UnknownHue;
                fn from_str(_: &str) -> Result<Self, Self::Err> { Ok(Self::One) }
            }
        };
    }
    hues!(Hue);
}
pub use hues::Hue;
impl parse::FromStr for u16 { fn parsed(&self) -> u32 { 2 } }
pub struct Stray;
struct Hidden;
impl FromStr for Hidden { type Err = Stray; fn from_str(_: &str) -> Result<Hidden, Stray> { Err(Stray) } }
pub fn parse(s: &str) -> bool {
    s.parse::<Kind>().is_ok() && s.parse::<Colour>().is_ok() && s.parse::<Hidden>().is_ok()
        && s.parse::<Shade>().is_ok() && s.parse::<Hue>().is_ok() && s.parse::<Number>().is_ok()
        && parse::one() == 1
}
";
    let main = "fn stored<S: core_t::Storage>(storage: &S) -> S::Stored { storage.store() }\n\
                use core_t::errors as _;\n\
                fn main() {\n    \
                let _ = (core_t::Kind::A, core_t::Colour::One, core_t::Shade::One, core_t::Hue::One, core_t::Number::One, stored(&core_t::Game));\n    \
                println!(\"{} {}\", core_t::world(&core_t::Game), core_t::parse(\"a\"));\n}\n";
    // `RemapHandle`, used only in `adapter`, is the associated type of an
    // `impl` of a `pub(crate)` trait for a `pub(crate)` type.
    let workspace = private_pair(
        "associated_types_over_public",
        lib,
        main,
        &[
            // A trait of the crate's own named as std's: `impl FromStr`
            // in `lib.rs` is std's, which is `pub`.
            (
                "core/src/parse.rs",
                "pub(crate) trait FromStr { fn parsed(&self) -> u32; }\n\
                 impl FromStr for u8 { fn parsed(&self) -> u32 { 1 } }\n\
                 pub(crate) fn one() -> u32 { 1u8.parsed() }\n",
            ),
            (
                "core/src/semantics.rs",
                "pub(crate) trait Values { type Remap; fn remap(&self) -> Self::Remap; }\n",
            ),
            (
                "core/src/adapter/mod.rs",
                "mod game;\npub(crate) struct Adapter;\n\
                 pub(crate) struct RemapHandle { lane: u32 }\n\
                 pub(crate) fn remap(adapter: &Adapter) -> u32 {\n    \
                 use crate::semantics::Values;\n    adapter.remap().lane\n}\n",
            ),
            (
                "core/src/adapter/game.rs",
                "use super::{Adapter, RemapHandle};\n\
                 impl crate::semantics::Values for Adapter {\n    \
                 type Remap = RemapHandle;\n    \
                 fn remap(&self) -> RemapHandle { RemapHandle { lane: 2 } }\n}\n",
            ),
        ],
    );
    // A private type's `impl` asks nothing of what it names, and what the
    // macro's names is used, `$crate::errors::BadShade` in a `Box` too, while
    // `errors` itself may narrow. The crate's own `FromStr`, implemented in
    // `lib.rs` too, is not the one `Kind`'s block names. `Handle` stays
    // `pub`, but its field need not:
    // that report's key is no `Handle`'s, which an entry for it would allow.
    let visibility = |keys: Vec<String>| -> Vec<String> {
        keys.into_iter()
            .filter(|key| key.starts_with("over-public ") || key.starts_with("unused-pub "))
            .collect()
    };
    assert_eq!(
        visibility(keys(&workspace)),
        [
            "over-public core_t::Handle.fields",
            "over-public core_t::Stray",
            "over-public core_t::errors",
        ]
    );
}

#[test]
fn over_public_asks_no_narrowing_that_makes_a_field_leak_and_counts_a_structs_fields() {
    let workspace = private_pair(
        "fields_over_public",
        "//! Core.\nmod walk;\n\
         pub fn solve() -> u32 { walk::run(walk::Ctx::new()) + walk::pair(walk::Pair::new()) }\n",
        "fn main() { println!(\"{}\", core_t::solve()); }\n",
        &[
            // `seat` is used only in `walk`, and `lanes` and `tau` in
            // `walk::child` too.
            (
                "core/src/walk/mod.rs",
                "mod child;\n\
                 pub(crate) struct Ctx { pub(crate) seat: u32, pub(crate) lanes: Vec<u32>, pub(crate) tau: Vec<u32> }\n\
                 impl Ctx {\n    \
                 pub(crate) fn new() -> Ctx { Ctx { seat: 1, lanes: vec![1], tau: Vec::new() } }\n    \
                 fn total(&self) -> u32 { self.seat + self.lanes.len() as u32 + self.tau.len() as u32 }\n}\n\
                 pub(crate) fn run(mut ctx: Ctx) -> u32 { ctx.seat += 1; child::grow(&mut ctx); ctx.total() }\n\
                 pub(crate) struct Pair { pub(crate) a: Vec<u32>, pub(crate) b: Vec<u32> }\n\
                 impl Pair {\n    \
                 pub(crate) fn new() -> Pair { Pair { a: Vec::new(), b: Vec::new() } }\n    \
                 fn len(&self) -> usize { self.a.len() + self.b.len() }\n}\n\
                 pub(crate) fn pair(mut p: Pair) -> u32 { p.a.push(1); p.b.push(2); p.len() as u32 }\n",
            ),
            (
                "core/src/walk/child.rs",
                "pub(super) fn grow(ctx: &mut super::Ctx) { ctx.lanes.push(1); ctx.tau.push(2); }\n",
            ),
        ],
    );
    let path = workspace.with_extension("allow");
    let allow = path.to_str().unwrap();
    let measured = || {
        let _ = std::fs::remove_file(&path);
        assert_eq!(run(&workspace, &["--update-allow", allow]).code, Some(0));
        over_public(
            std::fs::read_to_string(&path)
                .unwrap()
                .lines()
                .map(str::to_owned)
                .collect(),
        )
    };
    std::fs::write(
        workspace.join("deslop-please.toml"),
        "[allow]\nceilings = true\n",
    )
    .unwrap();
    assert_eq!(
        measured(),
        [
            "over-public core_t::walk::Ctx.fields <= 3",
            "over-public core_t::walk::Pair.fields <= 2"
        ]
    );
    let report = run(&workspace, &[]).stdout;
    assert!(
        report.contains("`Ctx` has 3 fields more visible than their uses need"),
        "{report}"
    );
    // Private, `seat` would be `walk`'s own, so `Ctx` would keep a field and
    // `lanes` and `tau`, which `child` pushes to, would leak. Private one at
    // a time, `a` or `b` would leave the other leaking, but together `Pair`
    // keeps both.
    std::fs::write(
        workspace.join("deslop-please.toml"),
        "[allow]\nceilings = true\n[field-leak]\nhome-module = true\n",
    )
    .unwrap();
    assert_eq!(
        measured(),
        [
            "over-public core_t::walk::Ctx.fields <= 2",
            "over-public core_t::walk::Pair.fields <= 2"
        ]
    );
    assert!(
        !keys(&workspace)
            .iter()
            .any(|key| key.starts_with("field-leak ")),
    );
}

#[test]
fn a_struct_literal_with_a_base_uses_every_field_of_its_type_there() {
    let workspace = scratch_workspace(
        "struct_update_uses",
        &[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"app\", \"core\"]\nexclude = [\"gpu\"]\nresolver = \"3\"\n",
            ),
            (
                "core/Cargo.toml",
                "[package]\nname = \"core_t\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n",
            ),
            (
                "core/src/lib.rs",
                "//! Ops.\npub mod ops {\n    \
                 #[derive(Default)]\n    pub struct DecisionOp { pub slot: u32, pub width: u32 }\n    \
                 #[derive(Default)]\n    pub struct ChanceOp { pub deal: u32, pub weight: u32 }\n    \
                 pub fn run(op: &DecisionOp, chance: &ChanceOp) -> u32 { op.slot + chance.deal }\n}\n",
            ),
            (
                "app/Cargo.toml",
                "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\
                 [dependencies]\ncore_t = { path = \"../core\" }\n",
            ),
            (
                "app/src/main.rs",
                "use core_t::ops::{ChanceOp, DecisionOp, run};\n\
                 fn main() {\n    let op = DecisionOp { slot: 1, ..Default::default() };\n    \
                 println!(\"{}\", run(&op, &ChanceOp::default()));\n}\n",
            ),
            (
                "gpu/Cargo.toml",
                "[package]\nname = \"gpu\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\
                 [workspace]\n[dependencies]\ncore_t = { path = \"../core\" }\n",
            ),
            (
                "gpu/src/lib.rs",
                "use core_t::ops::{ChanceOp, DecisionOp, run};\n\
                 pub fn plan() -> u32 {\n    \
                 run(&DecisionOp::default(), &ChanceOp { deal: 1, ..ChanceOp::default() })\n}\n",
            ),
        ],
    );
    let visibility = |keys: Vec<String>| -> Vec<String> {
        keys.into_iter()
            .filter(|key| key.starts_with("over-public ") || key.starts_with("unused-pub "))
            .collect()
    };
    // `app`'s literal moves `width` from its base: it uses both fields of
    // `DecisionOp` from another crate. Nothing in the workspace builds a
    // `ChanceOp` from fields.
    assert_eq!(
        visibility(keys(&workspace)),
        [
            "over-public core_t::ops::ChanceOp.fields",
            "unused-pub core_t::ops::ChanceOp::weight",
        ]
    );
    // The consumer's literal with a base does, so it uses `weight` too.
    std::fs::write(
        workspace.join("deslop-please.toml"),
        "consumers = [\"gpu\"]\n",
    )
    .unwrap();
    assert_eq!(visibility(keys(&workspace)), Vec::<String>::new());
}
