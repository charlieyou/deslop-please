//! What `deslop-please.toml`'s policies let through, and that without them
//! every lint keeps its default.

use super::{keys, private_pair, run, scratch_workspace};

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
             pub mod a {{\n    pub(crate) fn helper() -> u32 {{ 5 }}\n    \
                 pub fn run() -> u32 {{ helper() + crate::tested() + crate::inside_only() }}\n}}\n\
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
            ("other/Cargo.toml", &package("other_t")),
            ("other/src/lib.rs", &lib("other_t")),
            ("other/tests/it.rs", &it("other_t")),
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
    let mut everywhere = strict("core_t");
    everywhere.extend(strict("other_t"));
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
            "high-fan-out high::h1",
            "high-fan-out high::h2",
            "high-fan-out high::h3",
            "high-fan-out low::l1",
        ]
    );
    std::fs::write(
        workspace.join("deslop-please.toml"),
        "layers = [[\"low\"], [\"high\"]]\n[high-fan-out]\nmost-by-layer = { high = 17 }\n\
         other-crates = \"top-module\"\n",
    )
    .unwrap();
    assert_eq!(
        fanned(keys(&workspace)),
        ["high-fan-out high::h3", "high-fan-out low::l1"]
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
}
pub mod x { pub fn grow(plain: &mut crate::data::Plain) { plain.a.push(1); } }
pub mod y { pub fn grow(plain: &mut crate::data::Plain) { plain.b.push(1); } }
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
    // `poke` may use the private `e`, not the `pub` `d`.
    assert_eq!(
        leaks(keys(&workspace)),
        [
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
