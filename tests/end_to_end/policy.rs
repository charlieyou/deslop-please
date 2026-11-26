//! What `deslop-please.toml`'s policies let through, and that without them
//! every lint keeps its default.

use super::{keys, private_pair, scratch_workspace};

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
