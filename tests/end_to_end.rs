//! End-to-end tests of `deslop-please` on small workspaces through `rust-analyzer scip`.
//! A fixture family is a base workspace in `tests/fixtures/<family>` and its
//! variants in `tests/variants/<family>/<variant>`, each holding only the files
//! it changes: restructurings that must report the same keys, and a fix.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[path = "end_to_end/allow.rs"]
mod allow;
#[path = "end_to_end/policy.rs"]
mod policy;

const TOOL: &str = "deslop-please";
const PROGRAM: &str = env!("CARGO_BIN_EXE_deslop-please");

/// A copy of `fixture`, with `variant`'s files over it, to run `test` in.
fn workspace(fixture: &str, variant: Option<&str>, test: &str) -> PathBuf {
    let rust_analyzer = Command::new("rust-analyzer").arg("--version").output();
    assert!(
        rust_analyzer.is_ok_and(|output| output.status.success()),
        "these tests run rust-analyzer, which is not on PATH (the dev shell has it)"
    );
    let copy = Path::new(env!("CARGO_TARGET_TMPDIR")).join(test);
    if copy.exists() {
        std::fs::remove_dir_all(&copy).unwrap();
    }
    let tests = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    copy_directory(&tests.join("fixtures").join(fixture), &copy);
    if let Some(variant) = variant {
        copy_directory(&tests.join("variants").join(fixture).join(variant), &copy);
    }
    copy
}

fn copy_directory(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let path = entry.unwrap().path();
        let target = to.join(path.file_name().unwrap());
        if path.is_dir() {
            copy_directory(&path, &target);
        } else {
            std::fs::copy(&path, &target).unwrap();
        }
    }
}

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

fn run(workspace: &Path, arguments: &[&str]) -> Run {
    run_with(workspace, &[], arguments)
}

/// A run with `variables` set in its environment, and the test's own.
fn run_with(workspace: &Path, variables: &[(&str, &str)], arguments: &[&str]) -> Run {
    let Output {
        status,
        stdout,
        stderr,
    } = Command::new(PROGRAM)
        .args(arguments)
        .current_dir(workspace)
        // CI sets CARGO_TERM_COLOR=always; the tests read plain text.
        .env("CARGO_TERM_COLOR", "never")
        .envs(variables.iter().copied())
        .output()
        .unwrap();
    let run = Run {
        code: status.code(),
        stdout: String::from_utf8(stdout).unwrap(),
        stderr: String::from_utf8(stderr).unwrap(),
    };
    let variables: Vec<String> = variables
        .iter()
        .map(|(name, value)| format!("{name}='{value}' "))
        .collect();
    eprintln!(
        "$ {}{TOOL} {}\n{}{}",
        variables.concat(),
        arguments.join(" "),
        run.stdout,
        run.stderr
    );
    run
}

/// `lint key` for every diagnostic in `workspace`, as the allow file lists them.
fn keys(workspace: &Path) -> Vec<String> {
    configured_keys(workspace, &[])
}

/// The keys of the configuration that `flags` choose.
fn configured_keys(workspace: &Path, flags: &[&str]) -> Vec<String> {
    let allow = workspace.with_extension("keys");
    let mut arguments = vec!["--update-allow", allow.to_str().unwrap()];
    arguments.extend(flags);
    let update = run(workspace, &arguments);
    assert_eq!(update.code, Some(0));
    let mut keys: Vec<String> = std::fs::read_to_string(&allow)
        .unwrap()
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
        .collect();
    keys.sort();
    keys
}

/// The keys of `family`'s base, or of one of its variants.
fn variant_keys(family: &str, variant: Option<&str>) -> Vec<String> {
    let test = format!("{family}-{}", variant.unwrap_or("base"));
    keys(&workspace(family, variant, &test))
}

/// The contents of every file named `name` in `workspace`'s build directory,
/// where its build scripts' `OUT_DIR`s are.
fn built_files(workspace: &Path, name: &str) -> Vec<String> {
    let metadata = Command::new("cargo")
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .current_dir(workspace)
        .output()
        .unwrap();
    let metadata: serde_json::Value = serde_json::from_slice(&metadata.stdout).unwrap();
    let mut files = Vec::new();
    let build_directory = metadata["build_directory"].as_str().unwrap();
    find_files(Path::new(build_directory), name, &mut files);
    files
}

fn find_files(directory: &Path, name: &str, files: &mut Vec<String>) {
    for entry in std::fs::read_dir(directory).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if entry.file_type().unwrap().is_dir() {
            find_files(&path, name, files);
        } else if path.file_name().is_some_and(|file| file == name) {
            files.push(std::fs::read_to_string(&path).unwrap());
        }
    }
}

#[test]
fn clean_workspace_exits_zero() {
    let workspace = workspace("clean", None, "clean");
    let result = run(&workspace, &[]);
    assert_eq!(result.code, Some(0));
    assert!(result.stdout.contains(&format!("{TOOL}: ok")));
}

#[test]
fn a_leaked_field_leaks_however_it_is_reached_and_not_once_the_type_does_the_work() {
    let run_queue = run(&workspace("field_leak", None, "field_leak"), &[]);
    assert_eq!(run_queue.code, Some(1));
    assert!(run_queue.stdout.contains(
        "error[field-leak]: `RunQueue::items` is known outside `RunQueue`: 4 places in 2 modules"
    ));
    // `backlog` and `drained` only read `head`.
    assert!(run_queue.stdout.contains(
        "    runqueue::queue::ring::RunQueue\n\
         \x20     items ◀━┳━ 2  runqueue::sched::policy\n\
         \x20             ┗━ 2  runqueue::sched::report\n\
         \x20     also known outside: head\n\
         \x20     kept: limit\n"
    ));
    let leaked = [
        "field-leak runqueue::queue::ring::RunQueue::head",
        "field-leak runqueue::queue::ring::RunQueue::items",
    ];
    for variant in [
        "getters",
        "let_getters",
        "all_pub",
        "pasted",
        "tuple_getter",
    ] {
        assert_eq!(
            variant_keys("field_leak", Some(variant)),
            leaked,
            "{variant}"
        );
    }
    // The work moved into the type, or into a type behind a field that it
    // passes each call on to; and reading a copy of a value, with or without
    // a getter, works on nothing, even one the getter lends: `*queue.limit()`.
    for variant in ["real", "passed_on", "read", "lent_read"] {
        assert_eq!(
            variant_keys("field_leak", Some(variant)),
            Vec::<String>::new(),
            "{variant}"
        );
    }
}

#[test]
fn a_module_cycle_stays_one_through_a_re_export_or_a_relay_until_code_moves_where_it_is_used() {
    let net = run(&workspace("module_cycle", None, "module_cycle"), &[]);
    assert_eq!(net.code, Some(1));
    assert!(
        net.stdout
            .contains("error[module-cycle]: 2 modules in `net` use each other in a circle")
    );
    assert!(!net.stdout.contains("error[tangle]"));
    // Things in a chain: moving `BACKLOG_MOST` to its one user breaks it.
    assert!(net.stdout.contains(
        "    ┌─▶ kinds\n\
         \x20   │     ┃ kinds.rs:10  Stream::new uses BACKLOG_MOST\n\
         \x20   │     ▼\n\
         \x20   │   socket\n\
         \x20   │     │ socket.rs:5  listen uses Stream::{self, new}\n\
         \x20   └─────┘\n"
    ));
    assert_eq!(
        variant_keys("module_cycle", Some("reexported")),
        ["module-cycle net::kinds,net::socket"]
    );
    assert_eq!(
        variant_keys("module_cycle", Some("relayed")),
        ["module-cycle net::kinds,net::limits,net::socket"]
    );
    assert_eq!(
        variant_keys("module_cycle", Some("moved")),
        Vec::<String>::new()
    );
}

#[test]
fn a_module_built_only_for_tests_is_test_code_whatever_its_name() {
    // `kinds` declares `#[cfg(test)] mod harness;`, whose `fixtures` call
    // `socket`, and `#[cfg(all(test, not(miri)))] mod probe { .. }`, which
    // does too. Test code makes no circle with `socket`.
    let workspace = workspace("module_cycle", Some("for_tests"), "module_cycle-for_tests");
    assert_eq!(keys(&workspace), Vec::<String>::new());
    // Built for more than the tests, either one is code of `kinds` that uses `socket`.
    let kinds = workspace.join("src/kinds.rs");
    let text = std::fs::read_to_string(&kinds).unwrap();
    for (attribute, instead) in [
        ("#[cfg(test)]", "#[cfg(any(test, miri))]"),
        ("#[cfg(all(test, not(miri)))]", ""),
    ] {
        std::fs::write(&kinds, text.replace(attribute, instead)).unwrap();
        assert_eq!(
            keys(&workspace),
            ["module-cycle net::kinds,net::socket"],
            "{attribute} as {instead:?}"
        );
    }

    // An example builds a module at the same path for itself: the module is
    // not test code, the library's code in it included.
    std::fs::write(&kinds, &text).unwrap();
    let demo = workspace.join("examples/demo");
    std::fs::create_dir_all(demo.join("kinds")).unwrap();
    std::fs::write(
        demo.join("main.rs"),
        "mod kinds;\nfn main() {\n    kinds::harness::run();\n}\n",
    )
    .unwrap();
    std::fs::write(demo.join("kinds.rs"), "pub mod harness;\n").unwrap();
    std::fs::write(demo.join("kinds/harness.rs"), "pub fn run() {}\n").unwrap();
    assert_eq!(keys(&workspace), ["module-cycle net::kinds,net::socket"]);
}

#[test]
fn indexing_runs_the_build_scripts_so_a_cycle_through_what_they_write_is_seen() {
    let workspace = workspace("build_script", None, "build_script");
    let net = run(&workspace, &[]);
    // The first index of a fresh copy ran the build script...
    assert_eq!(
        built_files(&workspace, "limits.rs"),
        ["pub const BACKLOG_MOST: usize = 16;\n"]
    );
    // ...and `socket` includes `BACKLOG_MOST`, which no source file defines,
    // from what it wrote.
    assert_eq!(net.code, Some(1));
    assert!(net.stdout.contains(
        "    ┌─▶ kinds\n\
         \x20   │     ┃ kinds.rs:10  Stream::new uses BACKLOG_MOST\n\
         \x20   │     ▼\n\
         \x20   │   socket\n"
    ));
}

#[test]
fn a_tangle_stays_one_however_it_is_laid_out_until_it_is_one_module_or_untied() {
    let tasks_and_timers = "tangle boot::kernel::tasks::Tasks,boot::kernel::timers::Timers";
    let cycle = "module-cycle boot::kernel::tasks,boot::kernel::timers";
    let boot = run(&workspace("tangle", None, "tangle"), &[]);
    assert!(
        boot.stdout
            .contains("error[tangle]: 2 things know each other in a circle, across 2 modules")
    );
    assert!(boot.stdout.contains(
        "    ┌─▶ boot::kernel::tasks::Tasks\n\
         \x20   │     ┃ tasks.rs:17   Tasks::spawn uses Timers::{self, arm}\n\
         \x20   │     ▼\n\
         \x20   │   boot::kernel::timers::Timers\n\
         \x20   │     │ timers.rs:16  Timers::fire uses Tasks::{self, wake}\n\
         \x20   └─────┘\n"
    ));
    assert!(
        boot.stdout.contains(
            "error[module-cycle]: 2 modules in `boot::kernel` use each other in a circle"
        )
    );
    // Moving an `impl` block or lifting a type into the parent's file puts the
    // modules in order, and leaves the tangle.
    assert_eq!(
        variant_keys("tangle", Some("impl_moved")),
        [tasks_and_timers]
    );
    assert_eq!(
        variant_keys("tangle", Some("lifted")),
        ["tangle boot::kernel::Tasks,boot::kernel::timers::Timers"]
    );
    // The figure's fix: `Tasks` stays lifted, and `tick` unties it from `Timers`.
    for variant in ["merged", "real", "lifted_untied"] {
        assert_eq!(
            variant_keys("tangle", Some(variant)),
            Vec::<String>::new(),
            "{variant}"
        );
    }

    let ring = workspace("tangle", Some("ring"), "tangle-ring");
    let result = run(&ring, &[]);
    assert!(
        result
            .stdout
            .contains("error[tangle]: 8 things know each other in a circle, all in `boot::ring`")
    );
    let ring_key = "tangle boot::ring::A,boot::ring::B,boot::ring::C,boot::ring::D,\
                    boot::ring::E,boot::ring::F,boot::ring::G,boot::ring::H";
    assert_eq!(keys(&ring), [cycle, tasks_and_timers, ring_key]);
}

#[test]
fn a_shallow_module_stays_shallow_however_it_is_dressed_until_it_does_more_with_less() {
    let store = run(&workspace("shallow", None, "shallow"), &[]);
    assert!(store.stdout.contains(
        "error[shallow-module]: `inventory::store` is shallow: it shows 9 of its 15 functions"
    ));
    assert!(store.stdout.contains(
        "= note: `Store` shows 9 functions: Store::contains, Store::get, Store::insert, \
         Store::is_full, Store::largest, Store::new, Store::remove, +2 more"
    ));
    assert!(store.stdout.contains(
        "                  7\n\
         \x20   shows  ███████│██ 9\n\
         \x20   hides  ██████ │ 6\n\
         \x20   Store  ███████│██ 9\n"
    ));
    let shallow = ["shallow-module inventory::store"];
    // Some or all of its methods made free functions that use the store:
    // they are still the store's functions.
    for variant in [
        "padded",
        "impls_moved",
        "pub_field",
        "private_modules",
        "some_free_functions",
        "free_functions",
    ] {
        assert_eq!(variant_keys("shallow", Some(variant)), shallow, "{variant}");
    }
    // `is_full` names the store only in its signature.
    let free = run(
        &workspace("shallow", Some("free_functions"), "shallow-free"),
        &[],
    );
    assert!(free.stdout.contains(
        "= note: `Store` shows 9 functions: Store::new, contains, get, insert, is_full, \
         largest, remove, +2 more"
    ));
    assert_eq!(variant_keys("shallow", Some("deep")), Vec::<String>::new());
}

#[test]
fn small_types_that_each_show_a_few_functions_are_not_shallow_however_many_they_show_in_all() {
    let stats = run(&workspace("small_types", None, "small_types"), &[]);
    assert!(stats.stdout.contains(&format!("{TOOL}: ok")));
    assert_eq!(stats.code, Some(0));
}

#[test]
fn a_long_file_and_a_wide_directory_are_reported_until_they_are_split() {
    let workspace = workspace("clean", None, "layout");
    let kinds = workspace.join("src/kinds.rs");
    let original = std::fs::read_to_string(&kinds).unwrap();
    let padded = original.clone() + &"\n".repeat(2_000 - original.lines().count());
    std::fs::write(&kinds, &padded).unwrap();
    let socket = workspace.join("src/socket");
    let mod_rs = socket.join("mod.rs");
    let socket_mod = std::fs::read_to_string(&mod_rs).unwrap();
    let parts = |count: usize| {
        let declared: String = (1..=count).map(|n| format!("mod part{n};\n")).collect();
        std::fs::write(&mod_rs, declared + &socket_mod).unwrap();
        for n in 1..=count {
            let part = format!("pub struct Part{n};\n");
            std::fs::write(socket.join(format!("part{n}.rs")), part).unwrap();
        }
    };
    // 2,000 lines, and 15 files: at the most, not past it.
    parts(13);
    assert_eq!(keys(&workspace), Vec::<String>::new());

    parts(14);
    std::fs::write(&kinds, padded + "pub struct Late;\n").unwrap();
    let result = run(&workspace, &[]);
    assert_eq!(result.code, Some(1));
    assert!(result.stdout.contains(
        "error[wide-directory]: `src/socket/` is wide: it holds 16 files and directories, \
         more than 15\n\
         \x20 --> src/socket/mod.rs:1:1\n\
         \x20 = note: 16 files: datagram.rs, mod.rs, part1.rs, part10.rs, part11.rs, \
         part12.rs, part13.rs, +9 more\n\
         \x20                        15\n\
         \x20   holds  ███████████████│█ 16\n"
    ));
    assert!(result.stdout.contains(
        "error[long-file]: `src/kinds.rs` is long: it has 2001 lines, more than 2000\n\
         \x20 --> src/kinds.rs:1:1\n"
    ));
    assert_eq!(
        keys(&workspace),
        ["long-file src/kinds.rs", "wide-directory src/socket/"]
    );
}

#[test]
fn the_allow_file_suppresses_only_existing_entries_and_notes_what_is_fixed() {
    let workspace = workspace("field_leak", None, "allow");
    let allow = workspace.with_extension("allow");
    let allow = allow.to_str().unwrap();
    assert_eq!(run(&workspace, &["--update-allow", allow]).code, Some(0));
    let allowed = run(&workspace, &["--allow", allow]);
    assert_eq!(allowed.code, Some(0));
    assert!(allowed.stdout.contains(&format!("{TOOL}: ok")));

    let real = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/variants/field_leak/real");
    copy_directory(&real, &workspace);
    let fixed = run(&workspace, &["--allow", allow]);
    assert_eq!(fixed.code, Some(0));
    assert!(fixed.stdout.contains("2 allowed entries"));
    // As before ceilings: only stale entries, which an update takes out.
    assert!(
        fixed.stdout.contains(&format!(
            "2 allowed entries in {allow} are now fixed (run `{TOOL} --update-allow {allow}` to tighten it)"
        )),
        "{}",
        fixed.stdout
    );

    let lib = workspace.join("src/lib.rs");
    let ring = "\npub mod ring {\n    pub struct A(pub Option<Box<B>>);\n    pub struct B(pub Option<Box<C>>);\n\
                pub struct C(pub Option<Box<D>>);\n    pub struct D(pub Option<Box<E>>);\n\
                pub struct E(pub Option<Box<F>>);\n    pub struct F(pub Option<Box<G>>);\n\
                pub struct G(pub Option<Box<H>>);\n    pub struct H(pub Option<Box<A>>);\n}\n";
    let with_ring = std::fs::read_to_string(&lib).unwrap() + ring;
    std::fs::write(&lib, with_ring).unwrap();
    let new = run(&workspace, &["--allow", allow]);
    assert_eq!(new.code, Some(1));
    assert!(new.stdout.contains("error[tangle]: 8 things"));
    assert!(!new.stdout.contains("field-leak"));
}

#[test]
fn across_crates_a_field_leaks_and_test_code_is_left_out() {
    let workspace = workspace("two_crates", None, "two_crates");
    assert_eq!(run(&workspace, &[]).code, Some(0));

    let lib = workspace.join("crates/lib/src/lib.rs");
    let vault = "\npub struct Plain { pub x: u32, pub y: u32 }\n\
                 pub struct Vault { secret: u32, pub leaked: u32 }\n\
                 impl Vault {\n    pub fn new() -> Self { Self { secret: 1, leaked: 2 } }\n    \
                 pub fn next(&self) -> u32 { self.secret + 1 }\n}\n\
                 #[cfg(test)]\nmod tests {\n    #[test]\n    fn peeks() { let _ = super::Vault::new().leaked; }\n}\n";
    std::fs::write(&lib, std::fs::read_to_string(&lib).unwrap() + vault).unwrap();
    let it = workspace.join("app/tests/it.rs");
    let peeks = "\n#[test]\nfn peeks() {\n    let _ = lib::Vault::new().leaked;\n}\n";
    std::fs::write(&it, std::fs::read_to_string(&it).unwrap() + peeks).unwrap();

    let main = workspace.join("app/src/main.rs");
    let original = std::fs::read_to_string(&main).unwrap();
    let plain = format!("{original}\nfn bump_plain(p: &mut lib::Plain) {{ p.x += 1; }}\n");
    std::fs::write(&main, plain).unwrap();
    assert_eq!(keys(&workspace), Vec::<String>::new());
    let read = format!("{original}\nfn read_vault(v: &lib::Vault) -> u32 {{ v.leaked }}\n");
    std::fs::write(&main, read).unwrap();
    assert_eq!(keys(&workspace), Vec::<String>::new());

    let leak = format!("{original}\nfn bump_vault(v: &mut lib::Vault) {{ v.leaked += 1; }}\n");
    std::fs::write(&main, leak).unwrap();
    let result = run(&workspace, &[]);
    assert_eq!(result.code, Some(1));
    assert!(result.stdout.contains(
        "error[field-leak]: `Vault::leaked` is known outside `Vault`: 1 place in 1 module"
    ));
    assert_eq!(keys(&workspace), ["field-leak lib::Vault::leaked"]);
}

#[test]
fn integration_test_submodules_named_after_a_library_module_do_not_make_it_shallow() {
    let workspace = workspace("clean", None, "test_target_same_name");
    let lib = workspace.join("src/lib.rs");
    std::fs::write(
        &lib,
        "pub mod queue;\npub fn run() -> u32 {\n    \
         queue::f1() + queue::f2() + queue::f3() + queue::f4() \
         + queue::f5() + queue::f6() + queue::f7() + queue::f8()\n}\n",
    )
    .unwrap();
    std::fs::write(
        workspace.join("src/queue.rs"),
        "pub fn f1() -> u32 { 1 }\npub fn f2() -> u32 { 2 }\n\
         pub fn f3() -> u32 { 3 }\npub fn f4() -> u32 { 4 }\n\
         pub fn f5() -> u32 { 5 }\npub fn f6() -> u32 { 6 }\n\
         pub fn f7() -> u32 { 7 }\npub fn f8() -> u32 { 8 }\n",
    )
    .unwrap();
    // Leaf module `src/queue.rs` is not judged by `shallow-module`.
    assert_eq!(keys(&workspace), Vec::<String>::new());

    // An integration test target declaring `mod queue;` with child module `part`
    // and an inline `mod helper` must not turn `src/queue.rs` into a parent
    // module judged by `shallow-module`, while layout checks still see test files.
    std::fs::create_dir_all(workspace.join("tests/queue")).unwrap();
    std::fs::write(workspace.join("tests/main.rs"), "mod queue;\n").unwrap();
    std::fs::write(
        workspace.join("tests/queue/mod.rs"),
        "mod part;\nmod helper {\n    pub fn expected() -> u32 { 36 }\n}\n",
    )
    .unwrap();
    std::fs::write(
        workspace.join("tests/queue/part.rs"),
        "#[test]\nfn runs() { assert_eq!(clean::run(), super::helper::expected()); }\n",
    )
    .unwrap();
    assert_eq!(keys(&workspace), Vec::<String>::new());

    // Layout checks still apply to files inside `tests/queue/`.
    let long = "#[test]\nfn runs() { assert_eq!(clean::run(), super::helper::expected()); }\n"
        .to_owned()
        + &"\n".repeat(2000);
    std::fs::write(workspace.join("tests/queue/part.rs"), long).unwrap();
    assert_eq!(keys(&workspace), ["long-file tests/queue/part.rs"]);
}

#[test]
fn a_binary_that_includes_another_binarys_root_changes_nothing_however_the_two_sort() {
    // `git-wt` sorts before `wt`, and `include!`s its root, `src/main.rs`.
    let wt = run(&workspace("included_root", None, "included_root"), &[]);
    assert_eq!(wt.code, Some(1));
    assert!(wt.stdout.contains(
        "error[field-leak]: `Args::verbose` is known outside `Args`: 1 place in 1 module\n\
         \x20 --> src/main.rs:5:1\n"
    ));
    let leaked = ["field-leak wt::cli::Args::verbose"];
    assert_eq!(variant_keys("included_root", None), leaked);
    assert_eq!(variant_keys("included_root", Some("renamed")), leaked);
}

#[test]
fn a_target_or_features_bring_in_the_code_behind_their_cfg_and_each_keeps_its_index() {
    let workspace = workspace("gated", None, "gated");
    let console = "module-cycle gated::console::handle,gated::console::modes";
    let export = "module-cycle gated::export::csv,gated::export::rows";
    let metrics = "module-cycle gated::metrics::counter,gated::metrics::sink";
    assert_eq!(keys(&workspace), [metrics]);
    // The Windows standard library need not be installed: rust-analyzer
    // reads its source.
    let windows = ["--target", "x86_64-pc-windows-msvc"];
    assert_eq!(configured_keys(&workspace, &windows), [console, metrics]);
    assert_eq!(
        configured_keys(&workspace, &["--features", "export"]),
        [export, metrics]
    );
    assert_eq!(
        configured_keys(&workspace, &["--all-features"]),
        [export, metrics]
    );
    assert_eq!(
        configured_keys(&workspace, &["--no-default-features"]),
        Vec::<String>::new()
    );

    let fresh = run(&workspace, &["-F", "export", "--no-default-features"]);
    assert_eq!(fresh.code, Some(1));
    assert!(fresh.stderr.contains(&format!(
        "{TOOL}: indexing workspace with --features export --no-default-features..."
    )));
    assert!(fresh.stdout.contains("2 modules in `gated::export`"));
    assert!(!fresh.stdout.contains("gated::metrics"));
    // Back to each: its index is still there, however its flags are written.
    for (flags, code) in [
        (&[][..], 1),
        (&windows, 1),
        (&["--features=export"], 1),
        (&["--all-features"], 1),
        (&["--no-default-features"], 0),
    ] {
        let again = run(&workspace, flags);
        assert!(!again.stderr.contains("indexing"), "{flags:?}");
        assert_eq!(again.code, Some(code), "{flags:?}");
    }
}

#[test]
fn cargo_s_own_configuration_chooses_the_target_and_cfgs_too_and_each_keeps_its_index() {
    // A directory of its own around the workspace, for the `.cargo/config.toml`
    // that cargo reads there, as it reads each ancestor's.
    let workspace = workspace("gated", None, "gated-cargo-config/gated");
    let config = workspace.parent().unwrap().join(".cargo");
    if config.exists() {
        std::fs::remove_dir_all(&config).unwrap();
    }
    std::fs::create_dir_all(&config).unwrap();
    let build_target = |triple: &str| {
        let toml = format!("[build]\ntarget = \"{triple}\"\n");
        std::fs::write(config.join("config.toml"), toml).unwrap();
    };
    let console = "2 modules in `gated::console`";
    let windows = ["--target", "x86_64-pc-windows-msvc"];
    let plain = run(&workspace, &[]);
    assert!(plain.stderr.contains("indexing workspace..."));
    assert!(!plain.stdout.contains(console));
    assert!(run(&workspace, &windows).stdout.contains(console));

    // Each brings in `console`, indexing once: the next run finds its index.
    let indexes_once = |variables: &[(&str, &str)]| {
        for indexes in [true, false] {
            let report = run_with(&workspace, variables, &[]);
            assert_eq!(
                report.stderr.contains("indexing workspace..."),
                indexes,
                "{variables:?}"
            );
            assert!(report.stdout.contains(console), "{variables:?}");
        }
    };
    build_target("x86_64-pc-windows-msvc");
    indexes_once(&[]);
    std::fs::remove_file(config.join("config.toml")).unwrap();
    indexes_once(&[("RUSTFLAGS", "--cfg windows")]);

    // `--target` overrides `build.target`, as on cargo's command line.
    build_target("aarch64-apple-darwin");
    let flagged = run(&workspace, &windows);
    assert!(!flagged.stderr.contains("indexing"));
    assert!(flagged.stdout.contains(console));
    // Without either, the first index is still there.
    std::fs::remove_file(config.join("config.toml")).unwrap();
    let again = run(&workspace, &[]);
    assert!(!again.stderr.contains("indexing"));
    assert_eq!(again.stdout, plain.stdout);
}

#[test]
fn a_target_or_a_feature_cargo_does_not_know_is_an_error_not_a_report() {
    let workspace = workspace("gated", None, "gated-unknown");
    let feature = run(&workspace, &["--features", "exports"]);
    assert_eq!(feature.code, Some(2));
    assert!(feature.stdout.is_empty());
    assert!(
        feature
            .stderr
            .contains("cannot index with `--features exports`: cargo metadata says: error:")
    );
    assert!(
        feature
            .stderr
            .contains("does not contain this feature: exports")
    );
    let target = run(&workspace, &["--target", "x86_64-pc-windows-mscv"]);
    assert_eq!(target.code, Some(2));
    assert!(target.stdout.is_empty());
    assert!(target.stderr.contains(
        "cannot index with `--target x86_64-pc-windows-mscv`: cargo metadata says: error:"
    ));
    assert!(!target.stderr.contains("indexing"));
}

#[test]
fn outside_a_cargo_workspace_reports_error() {
    let empty = std::env::temp_dir().join(format!("{TOOL}-not-a-workspace-{}", std::process::id()));
    std::fs::create_dir_all(&empty).unwrap();
    let run = run(&empty, &[]);
    std::fs::remove_dir_all(&empty).unwrap();
    assert_eq!(run.code, Some(2));
    assert!(run.stderr.contains("not inside a Cargo workspace"));
}

/// Creates a fresh temporary Cargo workspace with `files` (path -> contents).
fn scratch_workspace(test: &str, files: &[(&str, &str)]) -> PathBuf {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(test);
    if root.exists() {
        std::fs::remove_dir_all(&root).unwrap();
    }
    for &(rel, contents) in files {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, contents).unwrap();
    }
    root
}

#[test]
fn wide_type_flags_bloated_types_and_ignores_passive_error_types() {
    let mut store_rs = String::from(
        "pub struct Err;\n\
         pub struct God { secret: u32 }\n\
         impl God {\n\
             pub fn new() -> Self { Self { secret: 0 } }\n\
             fn hidden(&self) -> u32 { self.secret + 1 }\n",
    );
    let mut call_body = String::from("let g = store::God::new();\nlet mut sum = 0u32;\n");
    for i in 1..=15 {
        store_rs.push_str(&format!(
            "    pub fn m{i}(&self) -> Result<u32, Err> {{ Ok(self.hidden() + {i}) }}\n"
        ));
        call_body.push_str(&format!("sum += g.m{i}().unwrap_or(0);\n"));
    }
    store_rs.push_str("}\n");
    for i in 1..=16 {
        store_rs.push_str(&format!(
            "pub fn check{i}(x: u32) -> Result<u32, Err> {{ if x > 0 {{ Ok(x + {i}) }} else {{ Err(Err) }} }}\n"
        ));
        call_body.push_str(&format!("sum += store::check{i}(1).unwrap_or(0);\n"));
    }
    call_body.push_str("sum\n");

    let ws = scratch_workspace(
        "wide_type_e2e",
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"wide\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[workspace]\n",
            ),
            (
                "src/lib.rs",
                &format!("mod store;\npub fn drive() -> u32 {{\n{call_body}}}\n"),
            ),
            ("src/store.rs", &store_rs),
        ],
    );
    let report = run(&ws, &[]);
    assert_eq!(report.code, Some(1));
    assert!(report.stdout.contains("error[wide-type]: `God` is wide"));
    assert!(!report.stdout.contains("`Err` is wide"));
    assert_eq!(
        keys(&ws),
        ["wide-module wide::store", "wide-type wide::store::God"]
    );
}

#[test]
fn scattered_impl_flags_sibling_impls_but_allows_home_submodules() {
    let ws = scratch_workspace(
        "scattered_impl_e2e",
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"scat\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[workspace]\n",
            ),
            ("src/lib.rs", "mod admin;\nmod home;\nmod local;\n"),
            (
                "src/home.rs",
                "pub struct Store { pub(crate) count: usize }\n\
                 impl Store { pub fn new() -> Self { Self { count: 0 } } }\n",
            ),
            (
                "src/admin.rs",
                "use crate::home::Store;\n\
                 impl Store { pub fn reset(&mut self) { self.count = 0; } }\n",
            ),
            (
                "src/local/mod.rs",
                "mod sub;\n\
                 pub struct Local { count: usize }\n\
                 impl Local { pub fn new() -> Self { Self { count: 0 } } }\n",
            ),
            (
                "src/local/sub.rs",
                "use super::Local;\n\
                 impl Local { pub fn inc(&mut self) { self.count += 1; } }\n",
            ),
        ],
    );
    assert_eq!(keys(&ws), ["scattered-impl scat::home::Store"]);
}

#[test]
fn visibility_lints_catch_unused_pub_and_over_public_in_private_crates() {
    let ws = scratch_workspace(
        "visibility_e2e",
        &[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"app\", \"core\"]\nresolver = \"3\"\n",
            ),
            (
                "core/Cargo.toml",
                "[package]\nname = \"core_lib\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n",
            ),
            (
                "core/src/lib.rs",
                "pub fn dead() -> u32 { 1 }\n\
                 pub fn test_only() -> u32 { 2 }\n\
                 pub fn internal_only() -> u32 { 3 }\n\
                 pub fn shared() -> u32 { internal_only() + 1 }\n\
                 pub fn it_used() -> u32 { internal_only() + 2 }\n\
                 #[cfg(test)]\nmod tests {\n\
                     #[test]\nfn t() { assert_eq!(super::test_only(), 2); }\n\
                 }\n",
            ),
            (
                "core/tests/it.rs",
                "#[test]\nfn uses_it() { assert_eq!(core_lib::it_used(), 5); }\n",
            ),
            (
                "app/Cargo.toml",
                "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\
                 [dependencies]\ncore_lib = { path = \"../core\" }\n",
            ),
            (
                "app/src/main.rs",
                "fn main() { let _ = core_lib::shared(); }\n",
            ),
        ],
    );
    assert_eq!(
        keys(&ws),
        [
            "over-public core_lib::internal_only",
            "test-only-pub core_lib::it_used",
            "test-only-pub core_lib::test_only",
            "unused-pub core_lib::dead",
        ]
    );
}

#[test]
fn pass_through_catches_shims_and_glob_reexports_but_spares_builders_and_private_splits() {
    let ws = scratch_workspace(
        "pass_through_e2e",
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"shims\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[workspace]\n",
            ),
            (
                "src/lib.rs",
                "mod parts;\n\
                 pub mod exposed;\n\
                 pub use parts::*;\n\
                 pub use exposed::*;\n\
                 pub struct Inner { x: u32 }\n\
                 impl Inner { pub fn bump(&mut self, n: u32) { self.x += n; } }\n\
                 pub struct Holder { pub inner: Inner, secret: Inner, pub flag: bool }\n\
                 impl Holder {\n\
                     pub fn flag(&self) -> bool { self.flag }\n\
                     pub fn with_flag(mut self, flag: bool) -> Self { self.flag = flag; self }\n\
                     pub fn bump_inner(&mut self, n: u32) { self.inner.bump(n) }\n\
                     pub fn bump_secret(&mut self, n: u32) { self.secret.bump(n) }\n\
                     pub fn alias_bump(&mut self, n: u32) { self.bump_secret(n) }\n\
                 }\n",
            ),
            ("src/parts.rs", "pub struct SplitPart;\n"),
            ("src/exposed.rs", "pub struct DupPart;\n"),
        ],
    );
    assert_eq!(
        keys(&ws),
        [
            "pass-through shims::Holder::alias_bump",
            "pass-through shims::Holder::bump_inner",
            "pass-through shims::Holder::flag",
            "pass-through shims::exposed::*",
        ]
    );
}

#[test]
fn functions_wide_module_wide_struct_bag_and_coupling_lints_work_end_to_end() {
    // 1. long-function & wide-signature
    let mut big_body = String::from(
        "pub fn huge(a: u32, b: u32, c: u32, d: u32, e: u32, f: u32, g: u32, h: u32) -> u32 {\n    let mut s = a + b + c + d + e + f + g + h;\n",
    );
    for _ in 0..155 {
        big_body.push_str("    s += 1;\n");
    }
    big_body.push_str("    s\n}\n");

    // 2. wide-module (16 free functions in leaf module `utils`)
    let mut utils_rs = String::new();
    let mut drive_utils = String::from("let mut total = 0u32;\n");
    for i in 0..16 {
        utils_rs.push_str(&format!("pub fn u{i}(x: u32) -> u32 {{ x + {i} }}\n"));
        drive_utils.push_str(&format!("total += utils::u{i}(total);\n"));
    }

    // 3. wide-struct (16 fields on non-serde struct)
    let mut bag_rs = String::from("pub struct Bag {\n");
    for i in 0..16 {
        bag_rs.push_str(&format!("    pub f{i}: u32,\n"));
    }
    bag_rs.push_str("}\n");

    // 4. shared-mutable-bag (mutated from 2 external modules, 0 kept fields)
    let mut_a = "use crate::bag::Bag;\npub fn touch_a(b: &mut Bag) { b.f0 += 1; }\n";
    let mut_b = "use crate::bag::Bag;\npub fn touch_b(b: &mut Bag) { b.f1 = 2; }\n";

    // 5. feature-envy (5 distinct methods on foreign type `Target`)
    let target_rs = "pub struct Target { x: u32 }\n\
                     impl Target {\n\
                         pub fn m0(&self) -> u32 { self.x + 10 }\n\
                         pub fn m1(&self) -> u32 { self.x + 1 }\n\
                         pub fn m2(&self) -> u32 { self.x + 2 }\n\
                         pub fn m3(&self) -> u32 { self.x + 3 }\n\
                         pub fn m4(&self) -> u32 { self.x + 4 }\n\
                     }\n";
    let envious_rs = "use crate::target::Target;\n\
                      pub struct Inspector;\n\
                      impl Inspector {\n\
                          pub fn inspect(&self, t: &Target) -> u32 {\n\
                              t.m0() + t.m1() + t.m2() + t.m3() + t.m4()\n\
                          }\n\
                      }\n";

    let ws = scratch_workspace(
        "next_six_e2e",
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"nextsix\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n[workspace]\n",
            ),
            (
                "src/lib.rs",
                &format!(
                    "mod bag;\nmod envious;\nmod mut_a;\nmod mut_b;\nmod target;\nmod utils;\n\
                     {big_body}\n\
                     pub(crate) fn internal_only_root() -> u32 {{ 1 }}\n\
                     pub fn drive(b: &mut bag::Bag, t: &target::Target) -> u32 {{\n\
                         {drive_utils}\n\
                         mut_a::touch_a(b);\n\
                         mut_b::touch_b(b);\n\
                         total + envious::Inspector.inspect(t) + internal_only_root()\n\
                     }}\n"
                ),
            ),
            ("src/utils.rs", &utils_rs),
            ("src/bag.rs", &bag_rs),
            ("src/mut_a.rs", mut_a),
            ("src/mut_b.rs", mut_b),
            ("src/target.rs", target_rs),
            ("src/envious.rs", envious_rs),
        ],
    );
    let found = keys(&ws);
    for expected in [
        "feature-envy nextsix::envious::Inspector::inspect",
        "long-function nextsix::huge",
        "over-public nextsix::internal_only_root",
        "shared-mutable-bag nextsix::bag::Bag",
        "wide-module nextsix::utils",
        "wide-signature nextsix::huge",
        "wide-struct nextsix::bag::Bag",
    ] {
        assert!(
            found.iter().any(|k| k == expected),
            "missing `{expected}` in {found:?}"
        );
    }
}

#[test]
fn duplicate_code_string_error_wide_enum_reexport_sprawl_layers_unused_dep_and_strict_allow() {
    let mut mega_enum = String::from("pub enum Mega {\n");
    for i in 0..26 {
        mega_enum.push_str(&format!("    V{i},\n"));
    }
    mega_enum.push_str("}\n");

    let mut items_mod = String::new();
    let mut facade_rs = String::from("mod items;\n");
    for i in 0..16 {
        items_mod.push_str(&format!("pub struct R{i};\n"));
        facade_rs.push_str(&format!("pub use items::R{i};\n"));
    }

    let dup_a = "\
pub fn calc_a(nums: &[u64], cap: u64) -> Result<u64, String> {\n\
    let mut total = 0u64;\n\
    for &n in nums {\n\
        if n > 0 && n < cap {\n\
            total = total.saturating_add(n * 3 + 1);\n\
        } else if n >= cap {\n\
            total = total.saturating_sub(1);\n\
        }\n\
    }\n\
    if total > cap { Ok(total - cap) } else { Ok(total) }\n\
}\n";
    let dup_b = "\
pub fn calc_b(values: &[u64], limit: u64) -> u64 {\n\
    let mut sum = 0u64;\n\
    for &n in values {\n\
        if n > 0 && n < limit {\n\
            sum = sum.saturating_add(n * 3 + 1);\n\
        } else if n >= limit {\n\
            sum = sum.saturating_sub(1);\n\
        }\n\
    }\n\
    if sum > limit { sum - limit } else { sum }\n\
}\n";
    let dup_c = "\
pub fn calc_c(xs: &[u64], bound: u64) -> u64 {\n\
    let mut acc = 0u64;\n\
    for &n in xs {\n\
        if n > 0 && n < bound {\n\
            acc = acc.saturating_add(n * 3 + 1);\n\
        } else if n >= bound {\n\
            acc = acc.saturating_sub(1);\n\
        }\n\
    }\n\
    if acc > bound { acc - bound } else { acc }\n\
}\n";

    let ws = scratch_workspace(
        "batch3_e2e",
        &[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"low\", \"high\", \"unused_dep\"]\nresolver = \"3\"\n",
            ),
            (
                "deslop-please.toml",
                "layers = [\n  [\"low\", \"unused_dep\"],\n  [\"high\"],\n]\n",
            ),
            (
                "unused_dep/Cargo.toml",
                "[package]\nname = \"unused_dep\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
            ),
            ("unused_dep/src/lib.rs", "pub fn noop() {}\n"),
            (
                "high/Cargo.toml",
                "[package]\nname = \"high\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
            ),
            ("high/src/lib.rs", "pub fn top() -> u32 { 1 }\n"),
            (
                "low/Cargo.toml",
                "[package]\nname = \"low\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\
                 [dependencies]\nhigh = { path = \"../high\" }\nunused_dep = { path = \"../unused_dep\" }\n",
            ),
            (
                "low/src/lib.rs",
                &format!(
                    "pub mod facade;\n{mega_enum}\n{dup_a}\n{dup_b}\n{dup_c}\n\
                     pub fn call_high() -> u32 {{ high::top() }}\n"
                ),
            ),
            ("low/src/facade/mod.rs", &facade_rs),
            ("low/src/facade/items.rs", &items_mod),
        ],
    );
    let found = keys(&ws);
    for expected in [
        "duplicate-code low::calc_b,low::calc_c",
        "layer-violation low -> high",
        "layer-violation low -> unused_dep",
        "reexport-sprawl low::facade",
        "string-error low::calc_a",
        "unused-dep low::unused_dep",
        "wide-enum low::Mega",
    ] {
        assert!(
            found.iter().any(|k| k == expected),
            "missing `{expected}` in {found:?}"
        );
    }

    // Verify `--strict-allow` fails when allowlist contains a stale entry.
    let allow_file = ws.join("test.allow");
    let mut entries = found.join("\n");
    entries.push_str("\nwide-enum low::NonExistent\n");
    std::fs::write(&allow_file, entries).unwrap();
    let allow_str = allow_file.to_str().unwrap();

    let ok_run = run(&ws, &["--allow", allow_str]);
    assert_eq!(ok_run.code, Some(0), "{}", ok_run.stdout);
    let strict_run = run(&ws, &["--allow", allow_str, "--strict-allow"]);
    assert_eq!(strict_run.code, Some(1), "{}", strict_run.stdout);
    assert!(
        strict_run.stdout.contains("error[stale-allow]"),
        "{}",
        strict_run.stdout
    );
}

#[test]
fn single_impl_trait_duplicate_struct_positional_trap_god_hub_and_pub_mod_field_work_end_to_end() {
    // Build a hub module with 10 dependencies and 10 callers.
    let mut files: Vec<(String, String)> = Vec::new();
    let mut lib_rs = String::from(
        "pub mod detail;\n\
         pub mod hub;\n\
         pub mod shadow;\n\
         pub struct Exposed { pub internal_field: u32, pub public_field: u32 }\n\
         pub trait OnlyOne { fn run(&self) -> u32; }\n\
         pub struct ImplA;\n\
         impl OnlyOne for ImplA { fn run(&self) -> u32 { 1 } }\n\
         pub trait StrExt { fn len2(&self) -> usize; }\n\
         impl StrExt for str { fn len2(&self) -> usize { self.len() } }\n\
         pub fn bool_trap(a: bool, b: bool) -> bool { a && b }\n\
         pub fn prim_trap(a: f32, b: f32, c: f32, d: f32) -> f32 { a + b + c + d }\n\
         pub fn use_internal(e: &Exposed) -> u32 { e.internal_field + detail::ping() }\n",
    );
    let mut hub_rs = String::from("pub fn hub_fn() -> u32 {\n    let mut s = 0u32;\n");
    for i in 0..10 {
        lib_rs.push_str(&format!("mod d{i};\nmod c{i};\n"));
        files.push((
            format!("core/src/d{i}.rs"),
            format!("pub fn f() -> u32 {{ {i} }}\n"),
        ));
        files.push((
            format!("core/src/c{i}.rs"),
            "pub fn call() -> u32 { crate::hub::hub_fn() }\n".to_owned(),
        ));
        hub_rs.push_str(&format!("    s += crate::d{i}::f();\n"));
    }
    hub_rs.push_str("    s\n}\n");

    files.push((
        "Cargo.toml".into(),
        "[workspace]\nmembers = [\"app\", \"core\"]\nresolver = \"3\"\n".into(),
    ));
    files.push((
        "core/Cargo.toml".into(),
        "[package]\nname = \"core_w\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n"
            .into(),
    ));
    files.push(("core/src/lib.rs".into(), lib_rs));
    files.push(("core/src/hub.rs".into(), hub_rs));
    files.push((
        "core/src/detail.rs".into(),
        "pub struct StakesA { pub sb: u32, pub bb: u32, pub ante: u32, pub stack: u32 }\n\
         pub fn ping() -> u32 { 1 }\n"
            .into(),
    ));
    files.push((
        "core/src/shadow.rs".into(),
        "pub struct StakesB { pub sb: u32, pub bb: u32, pub ante: u32, pub stack: u32 }\n".into(),
    ));
    files.push((
        "app/Cargo.toml".into(),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\
         [dependencies]\ncore_w = { path = \"../core\" }\n"
            .into(),
    ));
    files.push((
        "app/src/main.rs".into(),
        "fn main() {\n\
             let e = core_w::Exposed { internal_field: 1, public_field: 2 };\n\
             let _ = e.public_field + core_w::use_internal(&e);\n\
             let _ = core_w::hub::hub_fn();\n\
             let _ = core_w::bool_trap(true, false);\n\
             let _ = core_w::prim_trap(1.0, 2.0, 3.0, 4.0);\n\
         }\n"
        .into(),
    ));

    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_str()))
        .collect();
    let ws = scratch_workspace("batch4_e2e", &refs);
    let found = keys(&ws);
    for expected in [
        "duplicate-struct core_w::detail::StakesA,core_w::shadow::StakesB",
        "god-hub core_w::hub",
        "over-public core_w::detail",
        "positional-trap core_w::bool_trap",
        "positional-trap core_w::prim_trap",
        "single-impl-trait core_w::OnlyOne",
    ] {
        assert!(
            found.iter().any(|k| k == expected),
            "missing `{expected}` in {found:?}"
        );
    }
    assert!(
        !found
            .iter()
            .any(|k| k == "single-impl-trait core_w::StrExt"),
        "extension trait should not be flagged: {found:?}"
    );
}

/// Two crates, `low` and `high`, where `low` depends on `high`, and `settings`
/// as their `deslop-please.toml`.
fn layered_workspace(test: &str, settings: &str) -> PathBuf {
    scratch_workspace(
        test,
        &[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"low\", \"high\"]\nresolver = \"3\"\n",
            ),
            ("deslop-please.toml", settings),
            (
                "high/Cargo.toml",
                "[package]\nname = \"high\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
            ),
            ("high/src/lib.rs", "pub fn top() -> u32 { 1 }\n"),
            (
                "low/Cargo.toml",
                "[package]\nname = \"low\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\
                 [dependencies]\nhigh = { path = \"../high\" }\n",
            ),
            ("low/src/lib.rs", "pub fn call() -> u32 { high::top() }\n"),
        ],
    )
}

#[test]
fn the_settings_file_is_toml_and_a_key_it_does_not_know_is_an_error() {
    // Literal strings and trailing comments, which TOML allows.
    let downward = "# lowest first\nlayers = [\n  ['high'], # 'low' uses it\n  [\"low\"],\n]\n";
    let workspace = layered_workspace("settings_toml", downward);
    assert_eq!(keys(&workspace), Vec::<String>::new());
    let upward = "layers = [['low'], ['high']]\n";
    std::fs::write(workspace.join("deslop-please.toml"), upward).unwrap();
    assert_eq!(keys(&workspace), ["layer-violation low -> high"]);

    std::fs::write(workspace.join("deslop-please.toml"), "layer = [['low']]\n").unwrap();
    let misspelt = run(&workspace, &[]);
    assert_eq!(misspelt.code, Some(2));
    assert!(
        misspelt.stderr.contains("deslop-please.toml") && misspelt.stderr.contains("`layer`"),
        "{}",
        misspelt.stderr
    );
}

/// A private library crate, `core_t`, with `lib`, and a binary, `app`, that
/// depends on it with `main`.
fn private_pair(test: &str, lib: &str, main: &str, extra: &[(&str, &str)]) -> PathBuf {
    let mut files = vec![
        (
            "Cargo.toml",
            "[workspace]\nmembers = [\"app\", \"core\"]\nresolver = \"3\"\n",
        ),
        (
            "core/Cargo.toml",
            "[package]\nname = \"core_t\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n",
        ),
        ("core/src/lib.rs", lib),
        (
            "app/Cargo.toml",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\
             [dependencies]\ncore_t = { path = \"../core\" }\n",
        ),
        ("app/src/main.rs", main),
    ];
    files.extend_from_slice(extra);
    scratch_workspace(test, &files)
}

#[test]
fn a_traits_items_take_its_visibility_and_are_not_narrowed_on_their_own() {
    let lib = "pub trait Shape {\n    fn area(&self) -> u32;\n    \
               fn name(&self) -> &'static str { \"shape\" }\n}\n\
               pub struct Square { side: u32 }\n\
               impl Shape for Square { fn area(&self) -> u32 { self.side() * self.side } }\n\
               pub struct Dot;\nimpl Shape for Dot { fn area(&self) -> u32 { 0 } }\n\
               impl Square {\n    pub fn new(side: u32) -> Self { Self { side } }\n    \
               pub fn side(&self) -> u32 { self.side }\n}\n\
               pub fn total(shape: &dyn Shape) -> u32 { shape.area() + shape.name().len() as u32 }\n";
    let main = "fn show(shape: &dyn core_t::Shape) -> u32 { core_t::total(shape) }\n\
                fn main() { let _ = show(&core_t::Square::new(2)) + show(&core_t::Dot); }\n";
    let workspace = private_pair("trait_items", lib, main, &[]);
    // `area` and `name` are used only inside `core_t`, as `side` is, but
    // only `side` can be narrowed.
    assert_eq!(keys(&workspace), ["over-public core_t::Square::side"]);
}

#[test]
fn what_a_kept_signature_names_stays_pub_and_a_narrowable_one_reports_at_its_root() {
    let lib = "\
pub struct Config { level: u32 }
pub struct Scratch(u32);
pub fn load() -> Config { let scratch = Scratch(2); Config { level: scratch.0 } }
pub fn level(config: &Config) -> u32 { config.level + helper(&Detail(1)) }
pub struct Detail(u32);
pub fn helper(detail: &Detail) -> u32 { detail.0 }
#[derive(Default)]
pub struct Opts { pub a: u32, pub b: u32 }
pub fn sum(opts: &Opts) -> u32 { opts.a + opts.b }
pub struct Pair { pub x: u32, pub y: u32 }
pub fn pair() -> Pair { Pair { x: 1, y: 2 } }
mod inner {
    pub struct Hidden;
    impl Hidden { pub fn get(&self) -> u32 { 3 } }
}
pub use inner::Hidden;
pub fn hidden() -> u32 { Hidden.get() + depth(&Tree { leaf: None }) }
pub const N: usize = 2; pub fn buffer() -> [u8; N] { [0; N] }
pub struct Tree { pub leaf: Option<Box<Leaf>> } pub struct Leaf { pub tree: Option<Box<Tree>> } fn depth(tree: &Tree) -> u32 { tree.leaf.as_ref().map_or(0, |leaf| leaf.tree.as_ref().map_or(1, |tree| 1 + depth(tree))) }
pub enum Shape { Round(Bark) } pub struct Bark(pub u32); pub fn shape() -> Shape { Shape::Round(Bark(1)) }
";
    let main = "\
fn main() {
    let opts = core_t::Opts { a: 1, ..Default::default() };
    let core_t::Pair { x, .. } = core_t::pair();
    let _ = core_t::level(&core_t::load()) + core_t::sum(&opts) + x + core_t::hidden();
    let _ = (core_t::buffer(), core_t::shape());
}
";
    let workspace = private_pair("pinned", lib, main, &[]);
    // `Config` stays as `pub` as `load`, which `app` calls, and `Detail`
    // narrows with `helper`; `Scratch` is only in a body, and the pattern
    // `Pair { x, .. }` moves no field, as `..Default::default()` moves `b`.
    // A const pins nothing; of `Tree` and `Leaf` one reports; a variant pins `Bark`.
    assert_eq!(
        keys(&workspace),
        [
            "over-public core_t::N",
            "over-public core_t::Pair::y",
            "over-public core_t::Scratch",
            "over-public core_t::Tree",
            "over-public core_t::helper",
            "over-public core_t::inner::Hidden",
        ]
    );
    let report = run(&workspace, &[]).stdout;
    assert!(
        report.contains(
            "error[over-public]: `helper` is `pub` but only used inside `core_t` (1 use in 1 module)\n\
             \x20 --> core/src/lib.rs:6:1\n\
             \x20 = note: no external crate, binary, example, or integration test references `helper`\n\
             \x20 = note: its signature pins 1 item, which can narrow with it: core_t::Detail\n"
        ),
        "{report}"
    );
    assert!(
        report.contains(
            "= note: its signature pins 1 item, which can narrow with it: core_t::Leaf\n"
        ),
        "{report}"
    );
    // `Hidden` narrows once the `pub use` does.
    assert!(
        report.contains(
            "error[over-public]: `Hidden` is `pub` but only used inside `core_t` (3 uses in 2 modules)\n\
             \x20 --> core/src/lib.rs:16:1\n"
        ),
        "{report}"
    );
    assert!(
        report.contains(
            "= note: core/src/lib.rs:16:1: a `pub use` re-exports it; narrow or remove that too\n"
        ),
        "{report}"
    );
}

#[test]
fn what_a_configured_consumer_outside_the_workspace_uses_is_used_from_another_crate() {
    let squid = scratch_workspace(
        "consumers-squid",
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"squid\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\
                 [workspace]\n[dependencies]\ncore_t = { path = \"../consumers/core\" }\n",
            ),
            (
                "src/lib.rs",
                "pub fn go() -> u32 { core_t::by_squid() }\n\
                 #[cfg(test)]\nmod tests {\n    #[test]\n    \
                 fn checks() { assert_eq!(core_t::by_squid_tests(), 3); }\n}\n",
            ),
        ],
    );
    assert!(squid.exists());
    let workspace = scratch_workspace(
        "consumers",
        &[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"core\"]\nexclude = [\"gpu\"]\nresolver = \"3\"\n",
            ),
            (
                "core/Cargo.toml",
                "[package]\nname = \"core_t\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n",
            ),
            (
                "core/src/lib.rs",
                "pub fn by_gpu() -> u32 { 1 }\npub fn by_squid() -> u32 { 2 }\n\
                 pub fn by_squid_tests() -> u32 { 3 }\npub fn by_nobody() -> u32 { 4 }\n",
            ),
            (
                "gpu/Cargo.toml",
                "[package]\nname = \"gpu\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\
                 [workspace]\n[dependencies]\ncore_t = { path = \"../core\" }\n",
            ),
            (
                "gpu/src/lib.rs",
                "pub fn run() -> u32 { core_t::by_gpu() }\n",
            ),
        ],
    );
    assert_eq!(
        keys(&workspace),
        [
            "unused-pub core_t::by_gpu",
            "unused-pub core_t::by_nobody",
            "unused-pub core_t::by_squid",
            "unused-pub core_t::by_squid_tests",
        ]
    );
    let settings = "consumers = [\"gpu\", \"../consumers-squid\"]\n";
    std::fs::write(workspace.join("deslop-please.toml"), settings).unwrap();
    // A consumer's tests are tests.
    assert_eq!(
        keys(&workspace),
        [
            "test-only-pub core_t::by_squid_tests",
            "unused-pub core_t::by_nobody",
        ]
    );
}

#[test]
fn a_restricted_method_is_visible_from_where_its_impl_block_is_not_its_types_module() {
    let workspace = scratch_workspace(
        "declared_home",
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"homes\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\
                 publish = false\n[workspace]\n",
            ),
            (
                "src/lib.rs",
                "mod home;\nmod ops;\n\
                 pub fn run() -> u32 { let store = home::Store::new(); ops::go(&store) + store.total() }\n",
            ),
            (
                "src/home.rs",
                "mod detail;\npub(crate) struct Store { pub(crate) n: u32 }\n\
                 impl Store {\n    pub(crate) fn new() -> Self { Store { n: 1 } }\n    \
                 pub(crate) fn total(&self) -> u32 { self.shared() + 1 }\n}\n",
            ),
            (
                "src/home/detail.rs",
                "use super::Store;\n\
                 impl Store { pub(crate) fn shared(&self) -> u32 { self.n + 1 } }\n",
            ),
            (
                "src/ops.rs",
                "use crate::home::Store;\n\
                 impl Store { pub(crate) fn helper(&self) -> u32 { self.n * 2 } }\n\
                 pub(crate) fn go(store: &Store) -> u32 { store.helper() + 1 }\n",
            ),
            ("src/main.rs", "fn main() { let _ = homes::run(); }\n"),
        ],
    );
    // Private, `helper` is still visible in `ops`, where all its callers
    // are, and `shared` would no longer be visible in `home`, its caller.
    assert_eq!(
        keys(&workspace),
        [
            "over-public homes::home::Store::helper",
            "scattered-impl homes::home::Store",
        ]
    );
    let report = run(&workspace, &[]).stdout;
    assert!(
        report.contains(
            "error[over-public]: `Store::helper` is `pub(crate)`/`pub(super)` but only used inside `homes::ops`"
        ),
        "{report}"
    );
}

#[test]
fn an_example_is_neither_measured_nor_coupled_but_still_justifies_pub() {
    let mut lib = String::from(
        "pub fn for_example() -> u32 { 7 }\n\
         pub struct Queue { pub items: Vec<u32>, head: usize }\n\
         impl Queue {\n    pub fn new() -> Self { Queue { items: Vec::new(), head: 0 } }\n    \
         pub fn len(&self) -> usize { self.items.len() - self.head }\n}\n",
    );
    let mut demo = String::from("struct Big {\n");
    for i in 0..16 {
        // Fan-out counts no child of the example's root module, the crate's.
        lib.push_str(&format!(
            "pub mod g{i} {{ pub mod m {{ pub fn f() -> u32 {{ {i} }} }} }}\n"
        ));
        demo.push_str(&format!("    f{i}: u32,\n"));
    }
    demo.push_str(
        "}\nfn wide(a: u8, b: u16, c: u32, d: u64, e: i8, f: i16, g: i32, h: i64) -> i64 {\n    \
         let mut s = h + g as i64 + f as i64 + e as i64 + d as i64 + c as i64 + b as i64 + a as i64;\n",
    );
    demo.push_str(&"    s += 1;\n".repeat(155));
    demo.push_str(
        "    s\n}\nfn main() {\n    let mut queue = ex::Queue::new();\n    \
                   queue.items.push(ex::for_example());\n    let _ = queue.len();\n    let s = 0",
    );
    for i in 0..16 {
        demo.push_str(&format!(" + ex::g{i}::m::f()"));
    }
    demo.push_str(";\n    let big = Big {");
    for i in 0..16 {
        demo.push_str(&format!(" f{i}: s,"));
    }
    demo.push_str(
        " };\n    let _ = wide(1, 2, 3, 4, 5, 6, 7, big.f0 as i64 + big.f15 as i64);\n}\n",
    );
    let workspace = scratch_workspace(
        "examples",
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"ex\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\
                 publish = false\n[workspace]\n",
            ),
            ("src/lib.rs", &lib),
            ("examples/demo.rs", &demo),
        ],
    );
    assert_eq!(keys(&workspace), Vec::<String>::new());
}

#[test]
fn a_twin_built_for_other_code_generation_or_a_body_that_decides_passes_nothing_through() {
    let lib = "\
//! Kernels.
pub fn body(xs: &mut [f32]) { for x in xs.iter_mut() { *x += 1.0; } }
#[target_feature(enable = \"avx2\")]
pub unsafe fn body_avx2(xs: &mut [f32]) { body(xs) }
#[inline(never)]
pub fn body_portable(xs: &mut [f32]) { body(xs) }
pub fn other(xs: &mut [f32]) { for x in xs.iter_mut() { *x *= 2.0; } }
pub fn plain(xs: &mut [f32]) { other(xs) }
pub fn heads_up(n: u32) -> bool { n == 2 }
pub fn seats(n: u32) -> u32 { if heads_up(n) { 7 } else { 10 } }
pub fn scaled<const N: u32>() -> u32 { N * 3 }
pub fn fixed() -> u32 { scaled::<2>() }
pub struct Inner; impl Inner { pub fn step(&self, n: u32) -> u32 { n * n + 1 } }
pub struct Outer(pub Inner); impl Outer { pub fn step(&self, n: u32) -> u32 { self.0.step(n) } }
";
    let workspace = scratch_workspace(
        "codegen_twins",
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"twins\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[workspace]\n",
            ),
            ("src/lib.rs", lib),
        ],
    );
    // `self.0` names a field, no value of the forwarder's own.
    assert_eq!(
        keys(&workspace),
        [
            "pass-through twins::Outer::step",
            "pass-through twins::plain"
        ]
    );
}

#[test]
fn a_use_written_as_text_in_a_serde_attribute_or_a_format_string_is_a_use() {
    let lib = "\
pub struct Depth(pub u32);
impl Depth { pub fn is_off(&self) -> bool { self.0 == 0 } }
pub const LIMIT: u32 = 3;
pub struct Rows(Vec<u32>);
impl Rows { pub fn new() -> Self { Rows(Vec::new()) } pub fn len(&self) -> usize { self.0.len() } }
impl Rows { pub fn is_empty(&self) -> bool { self.0.is_empty() } }
pub fn width() -> usize { 8 }
pub mod shown { pub const WIDE: u32 = 1; pub fn show() -> String { format!(\"{WIDE}\") } }
pub mod hidden { pub const WIDE: u32 = 2; }
pub mod limits { pub const MAX: u32 = 9; pub const MIN: u32 = 0; pub const TOP: u32 = 5; }
pub mod prelude { pub use crate::limits::TOP; }
pub mod glob { use crate::prelude::{self, *}; pub fn top() -> String { format!(\"{TOP}\") } }
pub mod named { use crate::limits::MAX; const MIN: u32 = 1; pub fn f() -> String { format!(\"{MAX}{MIN}\") } }
pub struct Row { pub cells: Vec<u32>, pub shift: u8 }
pub struct Other { pub cells: Vec<u32> }
impl Row { pub fn wipe(&mut self) { self.shift = 0; } }
macro_rules! row { () => { impl Row { pub fn new() -> Self { let wipe = 0; Self { cells: Vec::new(), shift: wipe } }
    pub fn copy(wipe: Self) -> Self { wipe } } }; }
row!();
pub struct Rep { pub cells: u32 }
macro_rules! rep { ($($t:ty),*) => { $( impl From<$t> for Rep { fn from(v: $t) -> Self { Self { cells: v as u32 } } } )* }; }
rep!(u8, u16);
#[derive(Default)]
pub struct Pat { pub cells: Vec<u32> }
macro_rules! pat { () => { impl Pat { pub fn size(&self) -> usize { let Self { ref cells, .. } = *self; cells.len() } } }; }
pat!();
";
    let main = "\
use core_t::LIMIT;
#[derive(serde::Serialize)]
struct View { #[serde(skip_serializing_if = \"core_t::Depth::is_off\")] depth: core_t::Depth }
fn main() {
    let view = View { depth: core_t::Depth(1) };
    let rows = core_t::Rows::new();
    let _ = format!(\"{LIMIT} {} {:>width$} {width:>3}\", view.depth.0, rows.len(), width = 2);
    let _ = (core_t::shown::show(), core_t::glob::top(), core_t::named::f(), core_t::Row::new());
    let _ = (core_t::Rep::from(1u8), core_t::Pat::default());
}
";
    let serde = "\
extern crate proc_macro;
use proc_macro::TokenStream;
#[proc_macro_derive(Serialize, attributes(serde))]
pub fn derive(_: TokenStream) -> TokenStream { TokenStream::new() }
";
    let workspace = private_pair(
        "text_uses",
        lib,
        main,
        &[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"app\", \"core\", \"serde\"]\nresolver = \"3\"\n",
            ),
            (
                "app/Cargo.toml",
                "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\
                 [dependencies]\ncore_t = { path = \"../core\" }\nserde = { path = \"../serde\" }\n",
            ),
            (
                "serde/Cargo.toml",
                "[package]\nname = \"serde\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\
                 [lib]\nproc-macro = true\n",
            ),
            ("serde/src/lib.rs", serde),
        ],
    );
    // `{width:>3}` captures the argument `width`, no function: only a
    // value can be captured, and `{WIDE}` only its own module's constant.
    // A glob brings in what its module re-exports; `use limits::MAX` no `MIN`.
    // `row!`'s body uses the fields of `Row`, which it implements, not `Other`'s,
    // and a local named `wipe` is no use of `Row::wipe`. `rep!` uses `cells` in
    // each `$( impl .. )*`, and `pat!` in a `ref` pattern.
    assert_eq!(
        keys(&workspace),
        [
            "over-public core_t::Pat::cells",
            "over-public core_t::Rep::cells",
            "over-public core_t::Row::cells",
            "over-public core_t::Row::shift",
            "over-public core_t::limits",
            "over-public core_t::limits::MAX",
            "over-public core_t::limits::TOP",
            "over-public core_t::shown::WIDE",
            "unused-pub core_t::Other",
            "unused-pub core_t::Row::wipe",
            "unused-pub core_t::hidden",
            "unused-pub core_t::hidden::WIDE",
            "unused-pub core_t::limits::MIN",
            "unused-pub core_t::width",
        ]
    );
}

#[test]
fn a_single_impl_in_a_crate_above_for_another_crates_type_or_from_a_macro_is_legitimate() {
    let base = "\
pub struct Thing(pub u32);
pub trait Seam { fn run(&self) -> u32; }
pub trait Solo { fn solo(&self) -> u32; }
impl Solo for Thing { fn solo(&self) -> u32 { self.0 } }
pub trait Kernel { fn k() -> u32; }
macro_rules! kernel {
    ($t:ty) => { impl Kernel for $t { fn k() -> u32 { 1 } } };
}
kernel!(u8);
kernel!(u16);
pub trait Once { fn once() -> u32; }
macro_rules! once {
    ($t:ty) => { impl Once for $t { fn once() -> u32 { 2 } } };
}
once!(u8);
pub fn all(seam: &dyn Seam) -> u32 {
    seam.run() + Thing(1).solo() + <u8 as Kernel>::k() + <u16 as Kernel>::k() + <u8 as Once>::once()
}
";
    let top = "\
pub struct Engine;
impl base::Seam for Engine { fn run(&self) -> u32 { 3 } }
trait Ext { fn twice(&self) -> u32; }
impl Ext for base::Thing { fn twice(&self) -> u32 { self.0 * 2 } }
fn main() { let _ = base::all(&Engine) + base::Thing(2).twice(); }
";
    let workspace = scratch_workspace(
        "single_impl_seams",
        &[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"base\", \"top\"]\nresolver = \"3\"\n",
            ),
            (
                "base/Cargo.toml",
                "[package]\nname = \"base\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n",
            ),
            ("base/src/lib.rs", base),
            (
                "top/Cargo.toml",
                "[package]\nname = \"top\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\
                 [dependencies]\nbase = { path = \"../base\" }\n",
            ),
            ("top/src/main.rs", top),
        ],
    );
    let found: Vec<String> = keys(&workspace)
        .into_iter()
        .filter(|key| key.starts_with("single-impl-trait"))
        .collect();
    // `kernel!` writes two impls of `Kernel`; `once!` writes one of `Once`.
    assert_eq!(
        found,
        [
            "single-impl-trait base::Once",
            "single-impl-trait base::Solo"
        ]
    );
}

#[test]
fn structs_duplicate_each_other_only_when_their_fields_have_the_same_types_too() {
    let lib = "\
pub mod ids { pub struct PlayerId(pub u8); }
use ids::PlayerId;
pub struct Hand4 { pub cards: [u8; 4], pub seat: PlayerId, pub stack: u32, pub pot: u32 }
pub struct Hand3 { pub cards: [u8; 3], pub seat: PlayerId, pub stack: u32, pub pot: u32 }
pub struct SeatA { pub cards: [u8; 2], pub seat: PlayerId, pub stack: u32, pub pot: u32 }
pub struct SeatB { pub cards: [u8; 2], pub seat: crate::ids::PlayerId, pub stack: u32, pub pot: u32 }
pub struct SeatC { pub cards: [u8; 2], pub seat: usize, pub stack: u32, pub pot: u32 }
";
    let workspace = scratch_workspace(
        "duplicate_struct_types",
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"dups\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[workspace]\n",
            ),
            ("src/lib.rs", lib),
        ],
    );
    // A path to the same type is the same type.
    assert_eq!(
        keys(&workspace),
        ["duplicate-struct dups::SeatA,dups::SeatB"]
    );
}

#[test]
fn a_function_is_as_long_as_its_code_lines_not_its_docs_attributes_or_comments() {
    let body = |lines: usize| -> String {
        (0..lines)
            .map(|at| format!("    let _v{at} = {at};\n"))
            .collect()
    };
    let docs: String = (0..40)
        .map(|at| format!("/// Line {at} of the docs.\n"))
        .collect();
    let mut lib = String::from("//! Lengths.\n");
    // 150 lines of code with the `fn` line and the `}`: at the limit.
    lib += &docs;
    lib += "#[expect(clippy::too_many_lines, reason = \"a fixture\")]\npub fn at_limit() {\n";
    for chunk in 0..4 {
        lib += &body(37);
        lib += &format!("\n    // Chunk {chunk} ends.\n    /* and a block\n       comment */\n\n");
    }
    lib += "}\n";
    // 151 lines of code: one over.
    lib += "pub fn over_limit() {\n";
    lib += &body(149);
    lib += "}\n";
    let workspace = scratch_workspace(
        "long_function_code_lines",
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"lengths\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[workspace]\n",
            ),
            ("src/lib.rs", &lib),
        ],
    );
    let found: Vec<String> = keys(&workspace)
        .into_iter()
        .filter(|key| key.starts_with("long-function"))
        .collect();
    assert_eq!(found, ["long-function lengths::over_limit"]);
}

#[test]
fn a_type_is_wide_by_what_code_outside_its_module_uses_and_a_submodule_is_outside() {
    let methods = |ty: &str| -> String {
        let mut text = format!("pub struct {ty} {{ base: u32 }}\nimpl {ty} {{\n");
        for at in 1..=16 {
            text += &format!("    pub fn m{at}(&self) -> u32 {{ self.base * {at} + 1 }}\n");
        }
        text + "}\n"
    };
    let calls =
        |value: &str| -> String { (1..=16).map(|at| format!(" + {value}.m{at}()")).collect() };
    // `Busy` is used in full only by its own module; `Open` by a submodule.
    let store = format!(
        "pub mod child;\n{}{}pub fn tally(busy: &Busy) -> u32 {{ 0{} }}\n",
        methods("Busy"),
        methods("Open"),
        calls("busy"),
    );
    let child = format!(
        "pub fn total(open: &super::Open) -> u32 {{ 0{} }}\n",
        calls("open")
    );
    let lib = "//! Types.\nmod store;\n\
               pub fn drive() -> u32 {\n\
                   let busy = store::Busy { base: 1 };\n\
                   let open = store::Open { base: 2 };\n\
                   store::tally(&busy) + busy.m1() + busy.m2() + store::child::total(&open)\n\
               }\n";
    let workspace = scratch_workspace(
        "wide_type_outside_home",
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"wide\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[workspace]\n",
            ),
            ("src/lib.rs", lib),
            ("src/store.rs", &store),
            ("src/store/child.rs", &child),
        ],
    );
    let found: Vec<String> = keys(&workspace)
        .into_iter()
        .filter(|key| key.starts_with("wide-type"))
        .collect();
    assert_eq!(found, ["wide-type wide::store::Open"]);
}
