//! What `--allow`, `--update-allow` and `--prune-allow` do with an
//! allowlist: ceilings that only fall, reasons and comments that stay,
//! groups, scoped runs and entries whose code moved.

use std::path::Path;

use super::{TOOL, run, scratch_workspace};

/// A workspace of one crate, `debt`, whose `src/lib.rs` is `lib`.
fn crate_with(test: &str, lib: &str, settings: &str) -> std::path::PathBuf {
    scratch_workspace(
        test,
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"debt\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[workspace]\n",
            ),
            ("deslop-please.toml", settings),
            ("src/lib.rs", lib),
        ],
    )
}

/// `pub fn {name}` of `lines` code lines in module `module`.
fn long(module: &str, name: &str, lines: usize) -> String {
    let body: String = (0..lines - 2)
        .map(|at| format!("        let _{name}{at} = {at};\n"))
        .collect();
    format!("pub mod {module} {{\n    pub fn {name}() {{\n{body}    }}\n}}\n")
}

fn entries(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
        .collect()
}

#[test]
fn a_ceiling_only_falls_and_a_reason_and_comments_stay_through_updates_and_prunes() {
    let workspace = crate_with(
        "allow_ceilings",
        &format!("//! Debt.\n{}", long("a", "plan", 160)),
        "[allow]\nceilings = true\n",
    );
    let path = workspace.with_extension("allow");
    let allow = path.to_str().unwrap();
    let _ = std::fs::remove_file(&path);
    assert_eq!(run(&workspace, &["--update-allow", allow]).code, Some(0));
    assert_eq!(entries(&path), ["long-function debt::a::plan <= 160"]);
    std::fs::write(
        &path,
        "# Debt.\n\n# Settled: one walk.\nlong-function debt::a::plan <= 160  # the planner\n",
    )
    .unwrap();

    // Grown past its ceiling: reported, and no update raises it.
    let lib = format!(
        "//! Debt.\n{}{}",
        long("a", "plan", 170),
        long("b", "walk", 151)
    );
    std::fs::write(workspace.join("src/lib.rs"), &lib).unwrap();
    let grown = run(&workspace, &["--allow", allow]);
    assert_eq!(grown.code, Some(1));
    assert!(
        grown
            .stdout
            .contains("it is 170, above `long-function debt::a::plan <= 160  # the planner`"),
        "{}",
        grown.stdout
    );
    let update = run(&workspace, &["--update-allow", allow]);
    assert_eq!(update.code, Some(1));
    assert!(
        update
            .stderr
            .contains("+ long-function debt::b::walk <= 151")
    );
    assert!(update.stderr.contains("no update raises a ceiling"));
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "# Debt.\n\n# Settled: one walk.\nlong-function debt::a::plan <= 160  # the planner\n\
         long-function debt::b::walk <= 151\n"
    );

    // Shrunk: allowed, but a strict run wants the ceiling lowered, and a
    // prune lowers it, keeping its reason and comments, and adds nothing.
    let lib = format!(
        "//! Debt.\n{}{}",
        long("a", "plan", 155),
        long("c", "new", 152)
    );
    std::fs::write(workspace.join("src/lib.rs"), &lib).unwrap();
    let strict = run(&workspace, &["--allow", allow, "--strict-allow"]);
    assert_eq!(strict.code, Some(1));
    assert!(strict.stdout.contains("allows no more than 155"));
    assert!(
        strict
            .stdout
            .contains("- long-function debt::b::walk <= 151\n")
    );
    assert!(strict.stdout.contains(&format!("{TOOL} --prune-allow")));
    assert_eq!(run(&workspace, &["--prune-allow", allow]).code, Some(0));
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "# Debt.\n\n# Settled: one walk.\nlong-function debt::a::plan <= 155  # the planner\n"
    );
    let pruned = run(&workspace, &["--allow", allow, "--strict-allow"]);
    assert_eq!(pruned.code, Some(1));
    assert!(
        pruned
            .stdout
            .contains("error[long-function]: `new` is long")
    );
    assert!(!pruned.stdout.contains("stale-allow"));
}

#[test]
fn a_group_allows_up_to_its_ceiling_and_a_move_is_followed_only_when_configured() {
    let lib = "//! Debt.\nmod m {\n    pub fn one() -> u32 { 1 }\n    pub fn two() -> u32 { 2 }\n}\n\
               pub fn run() -> u32 { m::one() + m::two() }\n";
    let workspace = scratch_workspace(
        "allow_groups",
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"debt\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\
                 [workspace]\n",
            ),
            ("src/lib.rs", lib),
            (
                "src/main.rs",
                "fn main() { println!(\"{}\", debt::run()); }\n",
            ),
        ],
    );
    let path = workspace.with_extension("allow");
    let allow = path.to_str().unwrap();
    std::fs::write(&path, "over-public debt::m::* <= 1\n").unwrap();
    let over = run(&workspace, &["--allow", allow]);
    assert_eq!(over.code, Some(1));
    assert!(
        over.stdout
            .contains("it is 2, above `over-public debt::m::* <= 1`")
    );
    std::fs::write(&path, "over-public debt::m::* <= 2\n").unwrap();
    assert_eq!(run(&workspace, &["--allow", allow]).code, Some(0));
    // A group key needs a ceiling.
    std::fs::write(&path, "over-public debt::m::*\n").unwrap();
    let bare = run(&workspace, &["--allow", allow]);
    assert_eq!(bare.code, Some(2));
    assert!(bare.stderr.contains("needs a ceiling"), "{}", bare.stderr);

    let workspace = crate_with(
        "allow_moves",
        &format!("//! Debt.\n{}", long("a", "plan", 160)),
        "",
    );
    let path = workspace.with_extension("allow");
    let allow = path.to_str().unwrap();
    std::fs::write(&path, "long-function debt::a::plan  # the planner\n").unwrap();
    let moved = format!("//! Debt.\n{}", long("b", "plan", 160));
    std::fs::write(workspace.join("src/lib.rs"), moved).unwrap();
    assert_eq!(run(&workspace, &["--allow", allow]).code, Some(1));
    std::fs::write(
        workspace.join("deslop-please.toml"),
        "[allow]\nfollow-moves = true\n",
    )
    .unwrap();
    assert_eq!(run(&workspace, &["--allow", allow]).code, Some(0));
    let strict = run(&workspace, &["--allow", allow, "--strict-allow"]);
    assert_eq!(strict.code, Some(1));
    assert!(strict.stdout.contains("moved to `debt::b::plan`"));
    assert_eq!(run(&workspace, &["--prune-allow", allow]).code, Some(0));
    assert_eq!(
        entries(&path),
        ["long-function debt::b::plan  # the planner"]
    );
}

#[test]
fn a_run_in_one_crate_reads_and_changes_only_its_entries_and_tsv_names_each_key() {
    let package = |name: &str| {
        format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n")
    };
    let workspace = scratch_workspace(
        "allow_scoped",
        &[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"one\", \"two\"]\nresolver = \"3\"\n",
            ),
            ("one/Cargo.toml", &package("one")),
            (
                "one/src/lib.rs",
                &format!("//! One.\n{}", long("a", "f", 151)),
            ),
            ("two/Cargo.toml", &package("two")),
            (
                "two/src/lib.rs",
                &format!("//! Two.\n{}", long("a", "g", 152)),
            ),
        ],
    );
    let path = workspace.with_extension("allow");
    let allow = path.to_str().unwrap();
    let both = ["long-function one::a::f", "long-function two::a::g"];
    let _ = std::fs::remove_file(&path);
    assert_eq!(run(&workspace, &["--update-allow", allow]).code, Some(0));
    assert_eq!(entries(&path), both);

    // Fixed in `two`: a run in `one` sees nothing stale, and changes
    // nothing of `two`'s; a run of all prunes it.
    std::fs::write(workspace.join("two/src/lib.rs"), "//! Two.\n").unwrap();
    assert_eq!(
        run(&workspace, &["one", "--allow", allow, "--strict-allow"]).code,
        Some(0)
    );
    assert_eq!(
        run(&workspace, &["one", "--update-allow", allow]).code,
        Some(0)
    );
    assert_eq!(
        run(&workspace, &["one", "--prune-allow", allow]).code,
        Some(0)
    );
    assert_eq!(entries(&path), both);
    assert_eq!(
        run(&workspace, &["--allow", allow, "--strict-allow"]).code,
        Some(1)
    );
    assert_eq!(
        run(&workspace, &["two", "--prune-allow", allow]).code,
        Some(0)
    );
    assert_eq!(entries(&path), ["long-function one::a::f"]);

    let tsv = run(&workspace, &["--format", "tsv"]);
    assert_eq!(tsv.code, Some(1));
    assert_eq!(tsv.stdout, "long-function\tone::a::f\tone/src/lib.rs:3\n");
}
