//! What `deslop-please.toml`'s policies let through, and that without them
//! every lint keeps its default.

use super::{keys, layered_workspace, private_pair, run, scratch_workspace};

#[test]
fn kernel_paths_are_held_to_finite_limits_of_their_own_and_only_when_configured() {
    let params = |name: &str, count: usize| -> String {
        let list: Vec<String> = (0..count).map(|at| format!("p{at}: Vec<u8>")).collect();
        let sum: Vec<String> = (0..count).map(|at| format!("p{at}.len()")).collect();
        format!(
            "pub fn {name}({}) -> usize {{ {} }}\n",
            list.join(", "),
            sum.join(" + ")
        )
    };
    let long = |name: &str, lines: usize| -> String {
        let body: String = (0..lines - 2)
            .map(|at| format!("    let _v{at} = {at};\n"))
            .collect();
        format!("pub fn {name}() {{\n{body}}}\n")
    };
    let traps = "pub fn numbers(a: u32, b: u32, c: u32, d: u32) -> u32 { a + b * c - d }\n\
                 pub fn flags(a: bool, b: bool) -> bool { a && !b }\n";
    let padding =
        |lines: usize| -> String { (0..lines).map(|at| format!("// Line {at}.\n")).collect() };
    // The same code in the kernel, and in a file whose name only starts
    // like it.
    let code = format!(
        "{}{}{}{}{traps}",
        params("nine", 9),
        params("ten", 10),
        long("lines_160", 160),
        long("lines_161", 161),
    );
    let lib = "//! Kernels.\npub mod kernel;\npub mod kernels;\n";
    let workspace = scratch_workspace(
        "kernel_limits",
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"hot\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[workspace]\n",
            ),
            ("src/lib.rs", lib),
            ("src/kernel/mod.rs", &format!("pub mod table;\n{code}")),
            ("src/kernel/table.rs", &padding(2_050)),
            ("src/kernels.rs", &format!("{code}{}", padding(2_050))),
        ],
    );
    let sized = |keys: Vec<String>| -> Vec<String> {
        keys.into_iter()
            .filter(|key| {
                [
                    "long-function",
                    "wide-signature",
                    "positional-trap",
                    "long-file",
                ]
                .iter()
                .any(|lint| key.starts_with(&format!("{lint} ")))
            })
            .collect()
    };
    let everywhere = |path: &str| -> Vec<String> {
        let mut found = vec![
            format!("long-function hot::{path}::lines_160"),
            format!("long-function hot::{path}::lines_161"),
            format!("positional-trap hot::{path}::flags"),
            format!("positional-trap hot::{path}::numbers"),
            format!("wide-signature hot::{path}::nine"),
            format!("wide-signature hot::{path}::ten"),
        ];
        found.sort();
        found
    };
    let mut strict = vec![
        "long-file src/kernel/table.rs".to_owned(),
        "long-file src/kernels.rs".to_owned(),
    ];
    strict.extend(everywhere("kernel"));
    strict.extend(everywhere("kernels"));
    strict.sort();
    assert_eq!(sized(keys(&workspace)), strict);

    std::fs::write(
        workspace.join("deslop-please.toml"),
        "[kernel]\npaths = [\"src/kern*l\"]\nmost-params = 9\nmost-lines = 160\n\
         most-file-lines = 2100\npositional-trap = \"bools\"\n",
    )
    .unwrap();
    let mut relaxed = vec![
        "long-file src/kernels.rs".to_owned(),
        "long-function hot::kernel::lines_161".to_owned(),
        "positional-trap hot::kernel::flags".to_owned(),
        "wide-signature hot::kernel::ten".to_owned(),
    ];
    relaxed.extend(everywhere("kernels"));
    relaxed.sort();
    assert_eq!(sized(keys(&workspace)), relaxed);
}

#[test]
fn tests_justify_what_production_uses_too_and_only_in_the_crates_configured() {
    let lib = |name: &str| -> String {
        format!(
            "//! The {name} crate.\n\
             pub fn shared() -> u32 {{ 1 }}\n\
             pub fn tested() -> u32 {{ 2 }}\n\
             pub fn tests_only() -> u32 {{ 3 }}\n\
             pub fn inside_only() -> u32 {{ 4 }}\n\
             pub fn checked() -> u32 {{ 6 }}\n\
             pub mod a {{\n    pub(crate) fn helper() -> u32 {{ 5 }}\n    \
                 pub fn run() -> u32 {{ helper() + crate::tested() + crate::inside_only() + crate::checked() }}\n}}\n\
             pub mod b {{\n    #[cfg(test)]\n    mod tests {{\n        \
                 #[test]\n        fn helper() {{ assert_eq!(crate::a::helper(), 5); }}\n    }}\n}}\n"
        )
    };
    let it = |name: &str| -> String {
        format!(
            "#[test]\nfn uses() {{ assert_eq!({name}::tested() + {name}::tests_only(), 5); }}\n"
        )
    };
    let package = |name: &str| -> String {
        format!(
            "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n"
        )
    };
    let workspace = scratch_workspace(
        "test_policy",
        &[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"app\", \"core\", \"other\"]\nresolver = \"3\"\n",
            ),
            ("core/Cargo.toml", &package("core_t")),
            ("core/src/lib.rs", &lib("core_t")),
            ("core/tests/it.rs", &it("core_t")),
            (
                "core/examples/demo.rs",
                "fn main() {}\n#[cfg(test)]\nmod tests {\n    #[test]\n    \
                 fn demo() { assert_eq!(core_t::checked(), 6); }\n}\n",
            ),
            (
                "other/Cargo.toml",
                &format!(
                    "{}[dev-dependencies]\ncore_t = {{ path = \"../core\" }}\n",
                    package("other_t")
                ),
            ),
            ("other/src/lib.rs", &lib("other_t")),
            (
                "other/tests/it.rs",
                &format!(
                    "{}#[test]\nfn across() {{ assert_eq!(core_t::checked(), 6); }}\n",
                    it("other_t")
                ),
            ),
            (
                "app/Cargo.toml",
                &format!(
                    "{}[dependencies]\ncore_t = {{ path = \"../core\" }}\nother_t = {{ path = \"../other\" }}\n",
                    package("app")
                ),
            ),
            (
                "app/src/main.rs",
                "fn main() {\n    println!(\"{}\", core_t::shared() + core_t::a::run() + \
                 other_t::shared() + other_t::a::run());\n}\n",
            ),
        ],
    );
    let surface = |keys: Vec<String>| -> Vec<String> {
        keys.into_iter()
            .filter(|key| key.starts_with("test-only-pub ") || key.starts_with("over-public "))
            .collect()
    };
    let strict = |name: &str| -> Vec<String> {
        vec![
            format!("over-public {name}::inside_only"),
            format!("test-only-pub {name}::a::helper"),
            format!("test-only-pub {name}::tested"),
            format!("test-only-pub {name}::tests_only"),
        ]
    };
    // Only another package's tests use `core_t::checked` besides its crate.
    let mut everywhere = strict("core_t");
    everywhere.push("test-only-pub core_t::checked".to_owned());
    everywhere.extend(strict("other_t"));
    everywhere.push("over-public other_t::checked".to_owned());
    everywhere.sort();
    assert_eq!(surface(keys(&workspace)), everywhere);

    std::fs::write(
        workspace.join("deslop-please.toml"),
        "[test-only-pub]\ncrates = [\"core-t\"]\nexternal-tests = \"justify\"\n\
         crate-tests = \"justify\"\n",
    )
    .unwrap();
    // A test justifies what production uses too, not what only tests use.
    let mut configured = vec![
        "over-public core_t::inside_only".to_owned(),
        "test-only-pub core_t::tests_only".to_owned(),
    ];
    configured.extend(strict("other_t"));
    configured.push("over-public other_t::checked".to_owned());
    configured.sort();
    assert_eq!(surface(keys(&workspace)), configured);

    std::fs::write(
        workspace.join("deslop-please.toml"),
        "[test-only-pub]\ncrates = [\"core-t\"]\nexternal-tests = \"own-package\"\n",
    )
    .unwrap();
    // Its own `tests/` justify `tested`; neither its example's test nor
    // another package's justifies `checked`, and tests in the crate no
    // `pub(crate)` item.
    configured.push("test-only-pub core_t::a::helper".to_owned());
    configured.push("test-only-pub core_t::checked".to_owned());
    configured.sort();
    assert_eq!(surface(keys(&workspace)), configured);
}

#[test]
fn a_string_error_is_reported_at_a_boundary_when_configured_and_everywhere_by_default() {
    let lib = "//! Errors.\n\
               pub fn open(x: u32) -> Result<u32, String> { checked(x).and_then(inner) }\n\
               pub(crate) fn checked(x: u32) -> Result<u32, String> { inner(x) }\n\
               fn inner(x: u32) -> Result<u32, String> { if x > 0 { Ok(x) } else { Err(\"zero\".into()) } }\n";
    let main = "//! An application.\n\
                pub(crate) fn parse(x: &str) -> Result<u32, String> { x.parse().map_err(|_| read(x).unwrap_err()) }\n\
                fn read(x: &str) -> Result<u32, String> { Err(format!(\"bad {x}\")) }\n\
                fn main() { println!(\"{:?}\", parse(\"1\").and_then(core_t::open)); }\n";
    let workspace = private_pair("string_error_boundary", lib, main, &[]);
    let errors = |keys: Vec<String>| -> Vec<String> {
        keys.into_iter()
            .filter(|key| key.starts_with("string-error "))
            .collect()
    };
    assert_eq!(
        errors(keys(&workspace)),
        [
            "string-error app::parse",
            "string-error app::read",
            "string-error core_t::checked",
            "string-error core_t::inner",
            "string-error core_t::open",
        ]
    );
    std::fs::write(
        workspace.join("deslop-please.toml"),
        "[string-error]\nboundary-only = true\napplication-crates = [\"app\"]\n",
    )
    .unwrap();
    assert_eq!(
        errors(keys(&workspace)),
        ["string-error core_t::checked", "string-error core_t::open"]
    );
}

/// `pub mod {name} { pub fn f() -> u32 { 1 } }` for each name.
fn leaves(names: impl IntoIterator<Item = String>) -> String {
    names
        .into_iter()
        .map(|name| format!("pub mod {name} {{ pub fn f() -> u32 {{ 1 }} }}\n"))
        .collect()
}

/// A module `name` whose `g` calls `f` of each of `paths`, holding `inner`.
fn user(name: &str, paths: impl IntoIterator<Item = String>, inner: &str) -> String {
    let calls: String = paths
        .into_iter()
        .map(|path| format!(" + {path}::f()"))
        .collect();
    format!("pub mod {name} {{\n    pub fn g() -> u32 {{ 0{calls} }}\n    {inner}\n}}\n")
}

fn numbered(prefix: &str, count: usize) -> impl Iterator<Item = String> {
    (1..=count).map(move |at| format!("{prefix}{at}"))
}

#[test]
fn a_god_hub_folds_into_high_fan_out_and_what_is_below_a_module_is_no_part_of_its_fan_out() {
    let mut lib = format!("//! Hubs.\n{}", leaves(numbered("m", 16)));
    let fifteen = || numbered("crate::m", 15);
    // `hub` uses 15 modules and one below it; `wide` 16 beside it.
    lib += &user(
        "hub",
        fifteen().chain(["inner::deep".to_owned()]),
        "pub mod inner { pub mod deep { pub fn f() -> u32 { 2 } } }",
    );
    lib += &user("wide", fifteen().chain(["crate::m16".to_owned()]), "");
    for at in 1..=10 {
        lib += &format!(
            "pub mod c{at} {{ pub fn call() -> u32 {{ crate::hub::g() + crate::wide::g() }} }}\n"
        );
    }
    let workspace = scratch_workspace(
        "fold_god_hub",
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"hubs\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[workspace]\n",
            ),
            ("src/lib.rs", &lib),
        ],
    );
    let coupled = |keys: Vec<String>| -> Vec<String> {
        keys.into_iter()
            .filter(|key| key.starts_with("high-fan-out ") || key.starts_with("god-hub "))
            .collect()
    };
    assert_eq!(
        coupled(keys(&workspace)),
        [
            "god-hub hubs::hub",
            "god-hub hubs::wide",
            "high-fan-out hubs::hub",
            "high-fan-out hubs::wide",
        ]
    );
    std::fs::write(
        workspace.join("deslop-please.toml"),
        "[high-fan-out]\nfold-god-hub = true\n",
    )
    .unwrap();
    assert_eq!(coupled(keys(&workspace)), ["high-fan-out hubs::wide"]);
    let report = run(&workspace, &[]);
    assert!(
        report
            .stdout
            .contains("10 modules depend on it: hubs::c1, hubs::c10"),
        "{}",
        report.stdout
    );
}

#[test]
fn a_layer_sets_the_fan_out_limit_from_it_up_and_another_crate_counts_by_its_top_modules() {
    // `low::l1` uses 16 of its crate's modules, 9 of them in `a`.
    let mut low = format!(
        "//! Low.\npub mod a {{\n{}}}\n{}",
        leaves(numbered("x", 9)),
        leaves(numbered("y", 7))
    );
    low += &user(
        "l1",
        numbered("crate::a::x", 9).chain(numbered("crate::y", 7)),
        "",
    );
    let mut high = format!("//! High.\n{}", leaves(numbered("n", 18)));
    // `h1` uses the 9 modules in `low::a` and 10 of its own: 19 modules, or
    // 11 when `low::a` counts once.
    high += &user(
        "h1",
        numbered("low::a::x", 9).chain(numbered("crate::n", 10)),
        "",
    );
    high += &user("h2", numbered("crate::n", 17), "");
    high += &user("h3", numbered("crate::n", 18), "");
    // `hub` and `edge` each use 16; 10 modules use `hub`, 9 use `edge`.
    high += &user("hub", numbered("crate::n", 16), "");
    high += &user("edge", numbered("crate::n", 16), "");
    for at in 1..=10 {
        let edge = if at < 10 { " + crate::edge::g()" } else { "" };
        high += &format!("pub mod c{at} {{ pub fn call() -> u32 {{ crate::hub::g(){edge} }} }}\n");
    }
    let workspace = scratch_workspace(
        "fan_out_by_layer",
        &[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"low\", \"high\"]\nresolver = \"3\"\n",
            ),
            (
                "low/Cargo.toml",
                "[package]\nname = \"low\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
            ),
            ("low/src/lib.rs", &low),
            (
                "high/Cargo.toml",
                "[package]\nname = \"high\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\
                 [dependencies]\nlow = { path = \"../low\" }\n",
            ),
            ("high/src/lib.rs", &high),
        ],
    );
    let fanned = |keys: Vec<String>| -> Vec<String> {
        keys.into_iter()
            .filter(|key| key.starts_with("high-fan-out "))
            .collect()
    };
    assert_eq!(
        fanned(keys(&workspace)),
        [
            "high-fan-out high::edge",
            "high-fan-out high::h1",
            "high-fan-out high::h2",
            "high-fan-out high::h3",
            "high-fan-out high::hub",
            "high-fan-out low::l1",
        ]
    );
    let config = "layers = [[\"low\"], [\"high\"]]\n[high-fan-out]\nmost-by-layer = { high = 17 }\n\
                  other-crates = \"top-module\"\n";
    std::fs::write(workspace.join("deslop-please.toml"), config).unwrap();
    // Unfolded, `god-hub` reports `hub` on its own.
    assert_eq!(
        fanned(keys(&workspace)),
        ["high-fan-out high::h3", "high-fan-out low::l1"]
    );
    std::fs::write(
        workspace.join("deslop-please.toml"),
        format!("{config}fold-god-hub = true\n"),
    )
    .unwrap();
    // Folded, a hub keeps the default limit of 15 under its layer's looser one.
    assert_eq!(
        fanned(keys(&workspace)),
        [
            "high-fan-out high::h3",
            "high-fan-out high::hub",
            "high-fan-out low::l1"
        ]
    );
    let report = run(&workspace, &[]).stdout;
    assert!(
        report.contains(
            "= note: held to the default limit, not its layer's: 10 or more modules depend on it\n"
        ),
        "{report}"
    );
}

#[test]
fn plain_data_is_a_bag_and_a_types_module_is_inside_it_for_its_private_fields_when_configured() {
    let lib = "//! Data.\n\
pub mod data {
    pub struct Plain { pub a: Vec<u32>, pub b: Vec<u32>, pub c: Vec<u32> }
    impl Plain {
        pub fn new() -> Plain { Plain { a: Vec::new(), b: Vec::new(), c: Vec::new() } }
        pub fn total(&self) -> usize { self.c.len() }
    }
    pub struct Kept { pub d: Vec<u32>, e: Vec<u32>, f: Vec<u32> }
    impl Kept {
        pub fn new() -> Kept { Kept { d: Vec::new(), e: Vec::new(), f: Vec::new() } }
        pub fn total(&self) -> usize { self.f.len() }
    }
    pub fn poke(kept: &mut Kept) { kept.d.push(1); kept.e.push(2); }
    pub struct Crated { pub g: Vec<u32>, pub(crate) h: Vec<u32>, pub i: Vec<u32> }
    impl Crated {
        pub fn new() -> Crated { Crated { g: Vec::new(), h: Vec::new(), i: Vec::new() } }
        pub fn total(&self) -> usize { self.i.len() }
    }
}
pub mod x {
    pub fn grow(plain: &mut crate::data::Plain) { plain.a.push(1); }
    pub fn fill(crated: &mut crate::data::Crated) { crated.g.push(1); }
}
pub mod y {
    pub fn grow(plain: &mut crate::data::Plain) { plain.b.push(1); }
    pub fn fill(crated: &mut crate::data::Crated) { crated.h.push(1); }
}
";
    let workspace = scratch_workspace(
        "plain_data",
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"bags\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[workspace]\n",
            ),
            ("src/lib.rs", lib),
        ],
    );
    let leaks = |keys: Vec<String>| -> Vec<String> {
        keys.into_iter()
            .filter(|key| key.starts_with("field-leak ") || key.starts_with("shared-mutable-bag "))
            .collect()
    };
    assert_eq!(
        leaks(keys(&workspace)),
        [
            "field-leak bags::data::Crated::g",
            "field-leak bags::data::Crated::h",
            "field-leak bags::data::Kept::d",
            "field-leak bags::data::Kept::e",
            "field-leak bags::data::Plain::a",
            "field-leak bags::data::Plain::b",
        ]
    );
    std::fs::write(
        workspace.join("deslop-please.toml"),
        "[field-leak]\nplain-data = true\nhome-module = true\n",
    )
    .unwrap();
    // `poke` may use the private `e`, not the `pub` `d`; a `pub(crate)`
    // field still guards `Crated`, which is no plain data.
    assert_eq!(
        leaks(keys(&workspace)),
        [
            "field-leak bags::data::Crated::g",
            "field-leak bags::data::Crated::h",
            "field-leak bags::data::Kept::d",
            "shared-mutable-bag bags::data::Plain",
        ]
    );
}

#[test]
fn a_files_inline_test_modules_and_a_generated_file_count_no_lines_when_configured() {
    let comments = |count: usize| -> String { (0..count).map(|at| format!("// {at}\n")).collect() };
    // 1,990 lines and a test module of 52: long only with it.
    let short = format!(
        "{}#[cfg(test)]\nmod tests {{\n{}}}\n",
        comments(1_990),
        comments(50)
    );
    // 2,010 lines and a test module of 100 with one of 42 in it.
    let long = format!(
        "{}#[cfg(test)]\nmod tests {{\n{}    #[cfg(test)]\n    mod deep {{\n{}    }}\n}}\n",
        comments(2_010),
        comments(54),
        comments(40)
    );
    let generated = format!("// @generated by a tool.\n{}", comments(2_100));
    let workspace = scratch_workspace(
        "long_file_test_modules",
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"files\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[workspace]\n",
            ),
            (
                "src/lib.rs",
                "//! Files.\nmod short;\nmod long;\nmod generated;\n",
            ),
            ("src/short.rs", &short),
            ("src/long.rs", &long),
            ("src/generated.rs", &generated),
        ],
    );
    let long_files = |keys: Vec<String>| -> Vec<String> {
        keys.into_iter()
            .filter(|key| key.starts_with("long-file "))
            .collect()
    };
    assert_eq!(
        long_files(keys(&workspace)),
        [
            "long-file src/generated.rs",
            "long-file src/long.rs",
            "long-file src/short.rs",
        ]
    );
    std::fs::write(
        workspace.join("deslop-please.toml"),
        "[long-file]\nskip-test-modules = true\nskip-generated = true\n",
    )
    .unwrap();
    assert_eq!(long_files(keys(&workspace)), ["long-file src/long.rs"]);
    let report = run(&workspace, &[]);
    assert!(
        report.stdout.contains("it has 2010 lines, more than 2000"),
        "{}",
        report.stdout
    );
}

#[test]
fn what_a_cfg_builds_for_tests_or_a_test_feature_is_test_code_when_configured() {
    let lib = "//! Core.\n\
pub fn run() -> u32 { after() }
pub fn raw() -> u32 { 2 }
#[cfg(any(test, feature = \"test-utils\"))]
pub fn fixture() -> u32 { raw() + 1 }
#[cfg(feature = \"test-utils\")]
pub struct Probe<T, U> { pub a: T, pub b: U }
pub fn after() -> u32 { 3 }
#[cfg(feature = \"test-utils\")]
pub mod support { pub fn make() -> crate::Probe<u8, u16> { crate::Probe { a: 1, b: 2 } } }
";
    let it = "#[test]\nfn uses() {\n    let probe = core_t::support::make();\n    \
              assert_eq!(core_t::fixture() + u32::from(probe.a) + u32::from(probe.b), 6);\n}\n";
    let workspace = private_pair(
        "test_features",
        lib,
        "fn main() { println!(\"{}\", core_t::run()); }\n",
        &[
            (
                "core/Cargo.toml",
                "[package]\nname = \"core_t\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\
                 [features]\ndefault = [\"test-utils\"]\ntest-utils = []\n",
            ),
            ("core/tests/it.rs", it),
        ],
    );
    let surface = |keys: Vec<String>| -> Vec<String> {
        keys.into_iter()
            .filter(|key| key.starts_with("test-only-pub ") || key.starts_with("over-public "))
            .collect()
    };
    assert_eq!(
        surface(keys(&workspace)),
        [
            "over-public core_t::after",
            "over-public core_t::raw",
            "test-only-pub core_t::fixture",
            "test-only-pub core_t::support",
            "test-only-pub core_t::support::make",
        ]
    );
    std::fs::write(
        workspace.join("deslop-please.toml"),
        "test-features = [\"test-utils\"]\n",
    )
    .unwrap();
    // `raw` is for tests now; `after`, past the generic `Probe`, is not.
    assert_eq!(
        surface(keys(&workspace)),
        ["over-public core_t::after", "test-only-pub core_t::raw"]
    );
}

#[test]
fn a_passive_record_and_a_command_lines_schema_may_be_wide_when_configured() {
    let fields: String = (0..16).map(|at| format!("pub f{at}: u32, ")).collect();
    let zero: String = (0..16).map(|at| format!("f{at}: 0, ")).collect();
    let lib = format!(
        "//! Records.\n\
         pub struct Busy {{ {fields} }}\n\
         impl Busy {{ pub fn sum(&self) -> u32 {{ self.f0 + self.f15 }} }}\n\
         pub struct Record {{ {fields} }}\n\
         impl Record {{ pub fn new() -> Record {{ Record {{ {zero} }} }} }}\n\
         impl std::fmt::Display for Record {{\n    \
             fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {{ write!(f, \"{{}}\", self.f0) }}\n\
         }}\n\
         #[derive(Parser)]\n\
         pub struct Flags {{ {fields} }}\n\
         impl Flags {{ pub fn first(&self) -> u32 {{ self.f0 }} }}\n"
    );
    let workspace = scratch_workspace(
        "passive_records",
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"records\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[workspace]\n",
            ),
            ("src/lib.rs", &lib),
        ],
    );
    let wide = |keys: Vec<String>| -> Vec<String> {
        keys.into_iter()
            .filter(|key| key.starts_with("wide-struct "))
            .collect()
    };
    assert_eq!(
        wide(keys(&workspace)),
        [
            "wide-struct records::Busy",
            "wide-struct records::Flags",
            "wide-struct records::Record",
        ]
    );
    std::fs::write(
        workspace.join("deslop-please.toml"),
        "[wide-struct]\npassive-records = true\nclap-derives = true\n",
    )
    .unwrap();
    // A trait's method is no method of the record's own.
    assert_eq!(wide(keys(&workspace)), ["wide-struct records::Busy"]);
    // A record wider than its cap is reported anyway; one at it is not.
    for (most, reported) in [
        (16, vec!["wide-struct records::Busy"]),
        (
            15,
            vec!["wide-struct records::Busy", "wide-struct records::Record"],
        ),
    ] {
        std::fs::write(
            workspace.join("deslop-please.toml"),
            format!("[wide-struct]\npassive-records = true\nmost-passive-fields = {most}\nclap-derives = true\n"),
        )
        .unwrap();
        assert_eq!(wide(keys(&workspace)), reported, "{most}");
    }
    // Of the two, only the passive `Record` says that its cap reports it.
    let report = run(&workspace, &[]).stdout;
    let capped =
        "= note: a passive record, but wider than 15, which `most-passive-fields` allows\n";
    assert_eq!(report.matches(capped).count(), 1, "{report}");
}

#[test]
fn simd_helpers_each_kernel_keeps_a_copy_of_are_no_duplicates_when_configured() {
    // A body of more than 45 tokens that calls `call` throughout.
    let body = |call: &str| -> String {
        let steps: String = (0..6)
            .map(|at| format!("    let v{} = {call}(v{at}, x);\n", at + 1))
            .collect();
        format!("{{\n    let v0 = {call}(x, y);\n{steps}    v6\n}}\n")
    };
    let helpers = |attribute: &str| -> String {
        format!(
            "use core::arch::x86_64::*;\n\
             {attribute}\npub fn lanes(x: u32, y: u32) -> u32 {}\
             {attribute}\npub fn mixed(x: u32, y: u32) -> u32 {}\
             #[inline(always)]\npub fn plain(x: u32, y: u32) -> u32 {}",
            body("_mm_add_epi32"),
            body("_mm_sub_epi32"),
            body("u32::wrapping_add"),
        )
    };
    let workspace = scratch_workspace(
        "simd_duplicates",
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"simd\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[workspace]\n",
            ),
            ("src/lib.rs", "//! Kernels.\npub mod avx;\npub mod sse;\n"),
            ("src/avx.rs", &helpers("#[inline(always)]")),
            (
                "src/sse.rs",
                &helpers("#[target_feature(enable = \"sse2\")]").replacen(
                    "#[target_feature(enable = \"sse2\")]\npub fn mixed",
                    "pub fn mixed",
                    1,
                ),
            ),
        ],
    );
    let duplicates = |keys: Vec<String>| -> Vec<String> {
        keys.into_iter()
            .filter(|key| key.starts_with("duplicate-code "))
            .collect()
    };
    assert_eq!(
        duplicates(keys(&workspace)),
        [
            "duplicate-code simd::avx::lanes,simd::sse::lanes",
            "duplicate-code simd::avx::mixed,simd::sse::mixed",
            "duplicate-code simd::avx::plain,simd::sse::plain",
        ]
    );
    std::fs::write(
        workspace.join("deslop-please.toml"),
        "[duplicate-code]\nsimd = true\n",
    )
    .unwrap();
    // Each copy must be compiled for the CPU and call its intrinsics; that
    // its file imports them is not enough.
    assert_eq!(
        duplicates(keys(&workspace)),
        [
            "duplicate-code simd::avx::mixed,simd::sse::mixed",
            "duplicate-code simd::avx::plain,simd::sse::plain",
        ]
    );
    // Copies in two crates could be one helper they share.
    let package = |name: &str| {
        format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n")
    };
    let crates = scratch_workspace(
        "simd_across_crates",
        &[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"one\", \"two\"]\nresolver = \"3\"\n",
            ),
            ("one/Cargo.toml", &package("one")),
            (
                "one/src/lib.rs",
                &format!("//! One.\n{}", helpers("#[inline(always)]")),
            ),
            ("two/Cargo.toml", &package("two")),
            (
                "two/src/lib.rs",
                &format!("//! Two.\n{}", helpers("#[inline(always)]")),
            ),
            ("deslop-please.toml", "[duplicate-code]\nsimd = true\n"),
        ],
    );
    assert_eq!(
        duplicates(keys(&crates)),
        [
            "duplicate-code one::lanes,two::lanes",
            "duplicate-code one::mixed,two::mixed",
            "duplicate-code one::plain,two::plain",
        ]
    );
}

#[test]
fn a_value_its_function_owns_is_shared_with_no_one_when_configured() {
    let lib = "//! Bags.\n\
pub mod data {
    pub struct Built { pub items: Vec<u32>, pub size: u32 }
    impl Built { pub fn new() -> Built { Built { items: Vec::new(), size: 0 } } }
    pub struct Shared { pub items: Vec<u32>, pub size: u32 }
    pub struct Found { pub items: Vec<u32>, pub size: u32 }
    pub struct Through { pub items: Vec<u32>, pub size: u32 }
}
pub mod x;
pub mod y;
pub mod v;
pub mod w;
pub mod z {
    use crate::data::{Found, Through};
    pub fn bump(built: &mut crate::data::Built) { built.size += 1; }
    pub fn first(all: &mut [Found]) { if let Some(found) = all.first_mut() { found.items.push(1); } }
    pub fn deref<B: std::ops::DerefMut<Target = Through>>(mut b: B) { b.size = 1; }
}
pub mod u {
    use crate::data::{Found, Through};
    pub fn last(all: &mut [Found]) { let found = all.last_mut().unwrap(); found.size = 2; }
    pub struct Holder { pub through: &'static mut Through }
    pub fn held(mut holder: Holder) { holder.through.items.clear(); }
}
";
    // Each file's first variable is its `local 0` in the index: `x`'s and
    // `y`'s own their values, `v`'s and `w`'s borrow theirs.
    let workspace = scratch_workspace(
        "owned_locals",
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"bags\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[workspace]\n",
            ),
            (
                "src/x.rs",
                "use crate::data::Built;\n\
                 pub fn build() -> Built { let mut built = Built::new(); built.items.push(1); built }\n",
            ),
            (
                "src/y.rs",
                "use crate::data::Built;\n\
                 pub fn sized(mut built: Built) -> Built { built.size = 2; built }\n",
            ),
            (
                "src/v.rs",
                "use crate::data::Shared;\npub fn grow(shared: &mut Shared) { shared.items.push(1); }\n",
            ),
            (
                "src/w.rs",
                "use crate::data::Shared;\npub fn resize(shared: &mut Shared) { shared.size = 2; }\n",
            ),
            ("src/lib.rs", lib),
        ],
    );
    let bags = |keys: Vec<String>| -> Vec<String> {
        keys.into_iter()
            .filter(|key| key.starts_with("shared-mutable-bag "))
            .collect()
    };
    assert_eq!(
        bags(keys(&workspace)),
        [
            "shared-mutable-bag bags::data::Built",
            "shared-mutable-bag bags::data::Found",
            "shared-mutable-bag bags::data::Shared",
            "shared-mutable-bag bags::data::Through",
        ]
    );
    std::fs::write(
        workspace.join("deslop-please.toml"),
        "[shared-mutable-bag]\nowned-locals = true\n",
    )
    .unwrap();
    // Only `z` mutates a `Built` it does not own; `&mut` still shares, as
    // does what a `let` or `if let` finds through one, a type parameter that
    // may be a borrow, and a field that is one. A file's variables are its
    // own: `v`'s `local 0` is not `x`'s.
    assert_eq!(
        bags(keys(&workspace)),
        [
            "shared-mutable-bag bags::data::Found",
            "shared-mutable-bag bags::data::Shared",
            "shared-mutable-bag bags::data::Through",
        ]
    );
}

#[test]
fn a_member_in_no_layer_is_an_error_when_every_member_must_be_layered() {
    // `low` uses `high`, and `high` is in no layer: unchecked by default.
    let workspace = layered_workspace("every_member_layered", "layers = [['low']]\n");
    assert_eq!(run(&workspace, &[]).code, Some(0));
    std::fs::write(
        workspace.join("deslop-please.toml"),
        "layers = [['low']]\nevery-member-layered = true\n",
    )
    .unwrap();
    let unlayered = run(&workspace, &[]);
    assert_eq!(unlayered.code, Some(2));
    assert!(
        unlayered.stderr.contains("`layers` leaves out high,"),
        "{}",
        unlayered.stderr
    );
    std::fs::write(
        workspace.join("deslop-please.toml"),
        "layers = [['low'], ['high']]\nevery-member-layered = true\n",
    )
    .unwrap();
    assert_eq!(keys(&workspace), ["layer-violation low -> high"]);
}

#[test]
fn a_chain_of_members_counts_only_its_first_and_siblings_fold_when_configured() {
    // `sub::get()` hands out a `Sub`, whose `inner` is a `mid::Inner`, whose
    // `shared` is a `deep::Shared`.
    let mut lib = format!(
        "//! Chains.\n{}\
         pub mod sub {{ pub struct Sub {{ pub inner: crate::mid::Inner }}\n    \
         pub fn get() -> Sub {{ Sub {{ inner: crate::mid::Inner {{ shared: crate::deep::Shared {{ config: 1 }} }} }} }} }}\n\
         pub mod mid {{ pub struct Inner {{ pub shared: crate::deep::Shared }} }}\n\
         pub mod deep {{ pub struct Shared {{ pub config: u32 }} }}\n",
        leaves(numbered("m", 15))
    );
    let chain = "pub fn h() -> u32 { let sub = crate::sub::get(); sub.inner.shared.config + crate::sub::get().inner.shared.config }";
    // Through the chain, `r1` names 15 modules and `r2` 16.
    lib += &user("r1", numbered("crate::m", 14), chain);
    lib += &user("r2", numbered("crate::m", 15), chain);
    // `outside` uses 16 modules under `a`, and `a::inside` its 16 siblings.
    lib += &format!(
        "pub mod a {{\n{}{}}}\n",
        leaves(numbered("x", 16)),
        user("inside", numbered("super::x", 16), "")
    );
    lib += &user("outside", numbered("crate::a::x", 16), "");
    let workspace = scratch_workspace(
        "fan_out_chains",
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"chains\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[workspace]\n",
            ),
            ("src/lib.rs", &lib),
        ],
    );
    let fanned = |keys: Vec<String>| -> Vec<String> {
        keys.into_iter()
            .filter(|key| key.starts_with("high-fan-out "))
            .collect()
    };
    assert_eq!(
        fanned(keys(&workspace)),
        [
            "high-fan-out chains::a::inside",
            "high-fan-out chains::outside",
            "high-fan-out chains::r2",
        ]
    );
    // Modules under one parent count once, unless it holds the user.
    std::fs::write(
        workspace.join("deslop-please.toml"),
        "[high-fan-out]\nfold-siblings = true\n",
    )
    .unwrap();
    assert_eq!(
        fanned(keys(&workspace)),
        ["high-fan-out chains::a::inside", "high-fan-out chains::r2"]
    );
    std::fs::write(
        workspace.join("deslop-please.toml"),
        "[high-fan-out]\nmost-modules = 16\n",
    )
    .unwrap();
    assert_eq!(fanned(keys(&workspace)), Vec::<String>::new());
    // A crate in the directory of another is no sibling of its modules:
    // `cli` uses two crates, `engine` and `engine_macros`, not one.
    let package = |name: &str, deps: &str| {
        format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n{deps}")
    };
    let cli = package(
        "cli",
        "[dependencies]\nengine = { path = \"../engine\" }\nengine_macros = { path = \"../engine/macros\" }\n",
    );
    let (engine, macros) = (package("engine", ""), package("engine_macros", ""));
    let crates = scratch_workspace(
        "fan_out_crates",
        &[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"cli\", \"engine\", \"engine/macros\"]\nresolver = \"3\"\n",
            ),
            ("cli/Cargo.toml", &cli),
            (
                "cli/src/lib.rs",
                "//! Cli.\npub fn all() -> u32 { engine::ONE + engine_macros::ONE }\n",
            ),
            ("engine/Cargo.toml", &engine),
            (
                "engine/src/lib.rs",
                "//! Engine.\npub const ONE: u32 = 1;\n",
            ),
            ("engine/macros/Cargo.toml", &macros),
            (
                "engine/macros/src/lib.rs",
                "//! Macros.\npub const ONE: u32 = 1;\n",
            ),
            (
                "deslop-please.toml",
                "[high-fan-out]\nfold-siblings = true\nmost-modules = 1\n",
            ),
        ],
    );
    assert_eq!(fanned(keys(&crates)), ["high-fan-out cli"]);
}

#[test]
fn a_deserialized_record_is_no_shared_bag_when_configured() {
    // The derive need not resolve: what a type derives is read from its text.
    let lib = "//! Records.\n\
pub mod data {
    #[derive(Debug, serde::Deserialize)]
    pub struct Config { pub items: Vec<u32>, pub size: u32 }
    #[derive(Debug, serde::Serialize)]
    pub struct Report { pub items: Vec<u32>, pub size: u32 }
}
pub mod x {
    pub fn grow(config: &mut crate::data::Config, report: &mut crate::data::Report) { config.items.push(1); report.items.push(1); }
}
pub mod y {
    pub fn resize(config: &mut crate::data::Config, report: &mut crate::data::Report) { config.size = 2; report.size = 2; }
}
";
    let workspace = scratch_workspace(
        "deserialized_records",
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"records\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[workspace]\n",
            ),
            ("src/lib.rs", lib),
        ],
    );
    let bags = |keys: Vec<String>| -> Vec<String> {
        keys.into_iter()
            .filter(|key| key.starts_with("shared-mutable-bag "))
            .collect()
    };
    assert_eq!(
        bags(keys(&workspace)),
        [
            "shared-mutable-bag records::data::Config",
            "shared-mutable-bag records::data::Report",
        ]
    );
    std::fs::write(
        workspace.join("deslop-please.toml"),
        "[shared-mutable-bag]\ndeserialized-records = true\n",
    )
    .unwrap();
    assert_eq!(
        bags(keys(&workspace)),
        ["shared-mutable-bag records::data::Report"]
    );
}

#[test]
fn a_private_function_called_once_may_be_wide_and_a_run_of_one_type_is_a_trap_when_configured() {
    let eight = "a: u8, b: u16, c: u32, d: u64, e: i8, f: i16, g: i32, h: i64";
    let sum = "u64::from(a) + u64::from(b) + u64::from(c) + d + e as u64 + f as u64 + g as u64 + h as u64";
    let call = "1, 2, 3, 4, 5, 6, 7, 8";
    let lib = format!(
        "//! Wide.\n\
         fn once({eight}) -> u64 {{ {sum} }}\n\
         fn twice({eight}) -> u64 {{ {sum} }}\n\
         fn deep(n: u32, {eight}) -> u64 {{ if n == 0 {{ {sum} }} else {{ deep(n - 1, a, b, c, d, e, f, g, h) }} }}\n\
         pub fn public({eight}) -> u64 {{ {sum} }}\n\
         pub fn run() -> u64 {{ once({call}) + twice({call}) + twice({call}) + deep(2, {call}) }}\n\
         pub fn slots(a: Vec<u32>, b: Vec<u32>, c: Vec<u32>, d: Vec<u64>) -> usize {{ a.len() + b.len() + c.len() + d.len() }}\n\
         pub fn pairs(a: Vec<u32>, b: Vec<u32>, c: Vec<u64>) -> usize {{ a.len() + b.len() + c.len() }}\n"
    );
    let workspace = scratch_workspace(
        "single_call_site",
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"wide\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[workspace]\n",
            ),
            ("src/lib.rs", &lib),
        ],
    );
    let sized = |keys: Vec<String>| -> Vec<String> {
        keys.into_iter()
            .filter(|key| key.starts_with("wide-signature ") || key.starts_with("positional-trap "))
            .collect()
    };
    let all = [
        "wide-signature wide::deep",
        "wide-signature wide::once",
        "wide-signature wide::public",
        "wide-signature wide::twice",
    ];
    assert_eq!(sized(keys(&workspace)), all);
    // Its call of itself is no call site.
    std::fs::write(
        workspace.join("deslop-please.toml"),
        "[wide-signature]\nsingle-call-site = true\n[positional-trap]\nsame-type-run = 3\n",
    )
    .unwrap();
    assert_eq!(
        sized(keys(&workspace)),
        [
            "positional-trap wide::slots",
            "wide-signature wide::public",
            "wide-signature wide::twice",
        ]
    );
    // A kernel that sets no limit of its own keeps the global one.
    std::fs::write(
        workspace.join("deslop-please.toml"),
        "[kernel]\npaths = [\"src/lib\"]\n[wide-signature]\nmost-params = 9\n",
    )
    .unwrap();
    assert_eq!(sized(keys(&workspace)), Vec::<String>::new());
}

#[test]
fn the_most_fields_variants_and_functions_of_a_type_are_configured() {
    let fields: String = (0..17).map(|at| format!("pub f{at}: u32, ")).collect();
    let variants: String = (0..27).map(|at| format!("V{at}, ")).collect();
    let methods: String = (0..17)
        .map(|at| format!("    pub fn m{at}(&self) -> u32 {{ self.f{at} + {at} }}\n"))
        .collect();
    let calls: String = (0..17).map(|at| format!(" + t.m{at}()")).collect();
    let lib = format!(
        "//! Wide.\npub mod t {{\n    pub struct T {{ {fields}}}\n    pub enum E {{ {variants}}}\n    \
         impl T {{\n{methods}    }}\n}}\n\
         pub mod u {{ pub fn sum(t: &crate::t::T) -> u32 {{ 0{calls} }} }}\n"
    );
    let workspace = scratch_workspace(
        "type_limits",
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"wide\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[workspace]\n",
            ),
            ("src/lib.rs", &lib),
        ],
    );
    let wide = |keys: Vec<String>| -> Vec<String> {
        keys.into_iter()
            .filter(|key| key.starts_with("wide-"))
            .collect()
    };
    assert_eq!(
        wide(keys(&workspace)),
        [
            "wide-enum wide::t::E",
            "wide-struct wide::t::T",
            "wide-type wide::t::T",
        ]
    );
    std::fs::write(
        workspace.join("deslop-please.toml"),
        "[wide-struct]\nmost-fields = 17\n[wide-enum]\nmost-variants = 27\n\
         [wide-type]\nmost-functions = 17\n",
    )
    .unwrap();
    assert_eq!(wide(keys(&workspace)), Vec::<String>::new());
}

#[test]
fn indirections_that_add_nothing_are_reported_only_when_opted_in() {
    let lib = "//! Shims.\n\
pub mod queue {
    pub struct Queue { items: Vec<u32>, head: usize, limit: usize, lock: std::sync::Mutex<usize> }
    impl Queue {
        pub fn new() -> Queue {
            Queue { items: Vec::new(), head: 0, limit: 4, lock: std::sync::Mutex::new(0) }
        }
        pub fn head(&self) -> usize { self.head }
        pub fn entries(&self) -> &Vec<u32> { &self.items }
        pub fn limit_or(&self, other: usize) -> usize { self.limit.max(other) }
        pub fn backlog(&self) -> usize { self.items.len() - self.head }
        pub fn locked(&self) -> usize { *self.lock.lock().unwrap() }
    }
}
pub mod plan {
    pub struct Args { pub depth: u32, pub width: u32 }
    pub struct Shared { pub depth: u32 }
    fn walk(args: Args) -> u32 { args.depth * args.width }
    pub fn run(depth: u32) -> u32 { walk(Args { depth, width: 2 }) + twice() }
    fn read(shared: &Shared) -> u32 { shared.depth }
    pub fn twice() -> u32 { read(&Shared { depth: 1 }) + read(&Shared { depth: 2 }) }
    fn solve_inner(n: u32) -> u32 { n * 3 + 1 }
    pub fn solve(n: u32) -> u32 { solve_inner(n) }
    fn count_from(n: u32, at: u32) -> u32 { if n == 0 { at } else { count_from(n - 1, at + 1) } }
    pub fn count(n: u32) -> u32 { count_from(n, 0) }
    fn shape(n: u32) -> u32 { n + 2 }
    pub fn shaped(n: u32) -> u32 { shape(n) + super::queue::Queue::new().backlog() as u32 }
    fn parse(text: &str) -> u32 { text.len() as u32 * 3 }
    pub fn load(path: &str) -> u32 { let text = std::fs::read_to_string(path).unwrap_or_default(); parse(&text) }
    fn load_at(path: &std::path::Path) -> u32 { path.as_os_str().len() as u32 * 7 }
    pub fn load_any(path: impl AsRef<std::path::Path>) -> u32 { load_at(path.as_ref()) }
    pub struct Run { n: i32 }
    impl Run {
        pub fn run(&mut self) { self.run_inner(); }
        fn run_inner(&mut self) { self.n = self.n * 2 + 1; }
        pub fn step(&self, x: i32) -> i32 { self.step_at(x, -1) }
        fn step_at(&self, x: i32, by: i32) -> i32 { x * by + self.n }
        pub fn last(&self) -> i32 { self.at(self.n - 1) }
        fn at(&self, i: i32) -> i32 { i * 3 + self.n }
    }
    pub struct Depth(u32);
    impl Depth { fn get(&self) -> u32 { self.0 * 2 + 1 } }
    pub struct Probe { depth: Depth }
    impl Probe { pub fn depth(&self) -> u32 { self.depth.get() } }
    #[derive(serde::Deserialize)]
    pub struct Plan { pub depth: u32 }
    fn check_plan(plan: &Plan) -> u32 { plan.depth * 5 }
    pub fn plan() -> u32 { let plan = Plan { depth: 3 }; check_plan(&plan) }
    #[derive(Clone, Copy)]
    pub struct Zeroed { pub depth: u32 }
    fn deep(zeroed: Zeroed) -> u32 { zeroed.depth * 2 }
    pub fn zeroed() -> u32 { let z: Zeroed = unsafe { std::mem::zeroed() }; deep(z) }
}
mod hidden { pub struct Facade; pub struct Twice; }
pub mod open { pub struct Open; }
pub use hidden::{Facade, Twice};
pub use open::Open;
pub mod api { pub use crate::hidden::Twice; pub use crate::open::Open; }
pub mod spec { mod load { pub struct Template; } pub use load::Template; }
pub use spec::Template;
pub mod kinds { pub enum Kind { Fast, Slow } }
pub use kinds::Kind::{Fast, Slow};
pub use dep::Remote;
pub mod gpu { pub use dep::Remote; }
";
    let workspace = scratch_workspace(
        "opt_in_indirections",
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"shims\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\
                 publish = false\n[dependencies]\ndep = { path = \"dep\" }\n[workspace]\n",
            ),
            ("src/lib.rs", lib),
            (
                "dep/Cargo.toml",
                "[package]\nname = \"dep\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
            ),
            ("dep/src/lib.rs", "//! Dep.\npub struct Remote;\n"),
        ],
    );
    let opted = |keys: Vec<String>| -> Vec<String> {
        let lints = [
            "rename-getter",
            "one-use-args",
            "inner-split",
            "redundant-reexport",
        ];
        keys.into_iter()
            .filter(|key| {
                lints
                    .iter()
                    .any(|lint| key.starts_with(&format!("{lint} ")))
            })
            .collect()
    };
    assert_eq!(opted(keys(&workspace)), Vec::<String>::new());
    std::fs::write(
        workspace.join("deslop-please.toml"),
        "opt-in = [\"rename-getter\", \"one-use-args\", \"inner-split\", \"redundant-reexport\"]\n",
    )
    .unwrap();
    // A getter with an argument or one that computes or locks, a struct
    // built twice, not by a literal or deserialized, a recursive inner function and a
    // caller that calls more, computes its argument, reads a file, is generic or is another type's
    // method, `Probe::depth` calling its field's, stay quiet, as do a
    // private module's re-export and the shallowest second path, unless it
    // goes through the deeper one: `Template`'s, from `spec`. A path through
    // `Kind` re-exports no `Kind`; another crate's item has one path here.
    assert_eq!(
        opted(keys(&workspace)),
        [
            "inner-split shims::plan::Run::run_inner",
            "inner-split shims::plan::Run::step_at",
            "inner-split shims::plan::solve_inner",
            "one-use-args shims::plan::Args",
            "redundant-reexport shims::Open",
            "redundant-reexport shims::Template",
            "redundant-reexport shims::api::Open",
            "redundant-reexport shims::api::Twice",
            "redundant-reexport shims::gpu::Remote",
            "rename-getter shims::queue::Queue::entries",
            "rename-getter shims::queue::Queue::head",
        ]
    );
}

#[test]
fn a_getter_is_reported_by_default_only_when_reading_its_field_would_do() {
    let lib = "//! Getters.\n\
pub mod table {
    pub struct Table { rows: Vec<u32>, width: usize, pub label: String, pub(crate) depth: u32, seed: u64 }
    impl Table {
        pub fn new(width: usize) -> Table {
            Table { rows: vec![0; width], width, label: String::new(), depth: 0, seed: 7 }
        }
        pub fn grow(&mut self) { self.rows.push(0); self.width += 1; }
        pub fn width(&self) -> usize { self.width }
        pub fn label(&self) -> &String { &self.label }
        fn rows(&self) -> &Vec<u32> { &self.rows }
        pub fn total(&self) -> u32 { self.rows().iter().sum() }
        pub fn depth(&self) -> u32 { self.depth }
        pub fn seed(&self) -> u64 { self.seed }
    }
    pub struct Span { start: u32, pub end: u32 }
    impl Span { pub fn start(&self) -> u32 { self.start } }
    pub struct Lock { entries: Vec<u32>, pub count: u8 }
    impl Lock {
        pub fn new(mut entries: Vec<u32>) -> Lock { entries.sort(); Lock { entries, count: 0 } }
        pub fn entries(&self) -> &Vec<u32> { &self.entries }
    }
    pub struct Sorted { items: Vec<u32>, pub label: String }
    impl From<Vec<u32>> for Sorted {
        fn from(mut items: Vec<u32>) -> Sorted { items.sort(); Sorted { items, label: String::new() } }
    }
    impl Sorted { pub fn items(&self) -> &Vec<u32> { &self.items } }
    pub struct Cursor { pos: usize, pub data: Vec<u8> }
    impl Iterator for Cursor {
        type Item = u8;
        fn next(&mut self) -> Option<u8> { self.pos += 1; self.data.get(self.pos - 1).copied() }
    }
    impl Cursor { pub fn pos(&self) -> usize { self.pos } }
}
pub mod report {
    use crate::table::{Cursor, Lock, Sorted, Span, Table};
    pub fn show(t: &Table, s: &Span, l: &Lock, o: &Sorted, c: &Cursor) -> usize {
        t.width() + t.label().len() + t.depth() as usize + s.start() as usize + l.entries().len()
            + o.items().len() + c.pos()
    }
    #[cfg(test)]
    mod tests {
        #[test]
        fn seeded() { assert_eq!(crate::table::Table::new(1).seed(), 7); }
    }
}
";
    let manifest = "[package]\nname = \"getters\"\nversion = \"0.1.0\"\nedition = \"2024\"\n";
    let workspace = scratch_workspace(
        "rename_getters",
        &[
            (
                "Cargo.toml",
                &format!("{manifest}publish = false\n[workspace]\n"),
            ),
            ("src/lib.rs", lib),
            (
                "src/main.rs",
                "mod tool {\n    pub struct Tool { w: usize }\n    impl Tool {\n        \
                 pub fn new() -> Tool { Tool { w: 0 } }\n        \
                 pub fn grow(&mut self) { self.w += 1; }\n        \
                 pub fn w(&self) -> usize { self.w }\n    }\n}\n\
                 fn main() { tool::Tool::new().grow(); }\n",
            ),
        ],
    );
    let getters = |keys: Vec<String>| -> Vec<String> {
        keys.into_iter()
            .filter(|key| key.starts_with("rename-getter "))
            .collect()
    };
    std::fs::write(
        workspace.join("deslop-please.toml"),
        "opt-in = [\"rename-getter\"]\n",
    )
    .unwrap();
    // A `pub` field, a crate-visible one read in the crate, a private one
    // read only in its module, one only tests read, and a record's: the
    // getter adds nothing, as for the binary's `Tool::w`, which nothing
    // calls. `width`, which `grow` maintains, `entries` and `items`, which
    // `Lock::new` and `Sorted::from` sort, and `pos`, which `next` moves,
    // are read-only accessors another module reads.
    assert_eq!(
        getters(keys(&workspace)),
        [
            "rename-getter getters::table::Span::start",
            "rename-getter getters::table::Table::depth",
            "rename-getter getters::table::Table::label",
            "rename-getter getters::table::Table::rows",
            "rename-getter getters::table::Table::seed",
            "rename-getter getters::tool::Tool::w",
        ]
    );
    // Published, a library's `pub` getter has callers outside: only tests
    // calling it, or callers in reach of its field, are no reason.
    std::fs::write(
        workspace.join("Cargo.toml"),
        format!("{manifest}[workspace]\n"),
    )
    .unwrap();
    assert_eq!(
        getters(keys(&workspace)),
        [
            "rename-getter getters::table::Span::start",
            "rename-getter getters::table::Table::label",
            "rename-getter getters::table::Table::rows",
            "rename-getter getters::tool::Tool::w",
        ]
    );
    std::fs::write(
        workspace.join("Cargo.toml"),
        format!("{manifest}publish = false\n[workspace]\n"),
    )
    .unwrap();
    std::fs::write(
        workspace.join("deslop-please.toml"),
        "opt-in = [\"rename-getter\"]\n[rename-getter]\nall-private-fields = true\n",
    )
    .unwrap();
    assert_eq!(
        getters(keys(&workspace)),
        [
            "rename-getter getters::table::Cursor::pos",
            "rename-getter getters::table::Lock::entries",
            "rename-getter getters::table::Sorted::items",
            "rename-getter getters::table::Span::start",
            "rename-getter getters::table::Table::depth",
            "rename-getter getters::table::Table::label",
            "rename-getter getters::table::Table::rows",
            "rename-getter getters::table::Table::seed",
            "rename-getter getters::table::Table::width",
            "rename-getter getters::tool::Tool::w",
        ]
    );
}
