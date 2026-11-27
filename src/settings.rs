//! `deslop-please.toml` at the workspace's root: what a codebase tells the
//! lints about itself. Every key is optional, and a missing file is the
//! empty one, which keeps every lint at its default. A key this tool does not
//! know is an error, so that a misspelt policy is never silently ignored.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

use crate::Error;

/// The file's name, at the workspace's root.
pub const FILE: &str = "deslop-please.toml";

/// The lints that `opt-in` turns on, and that report nothing without it.
pub const OPT_IN: [&str; 4] = [
    "rename-getter",
    "one-use-args",
    "inner-split",
    "redundant-reexport",
];

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Settings {
    /// The crates by layer, lowest first: a normal dependency must go to a
    /// lower layer (`layer-violation`).
    pub layers: Vec<Vec<String>>,
    /// Every member of the workspace must be in a layer of `layers`: one
    /// that is in none is an error, not a crate whose dependencies go
    /// unchecked.
    pub every_member_layered: bool,
    /// Directories of crates outside the workspace that use it, relative
    /// to its root: what they use of it is used from another crate.
    pub consumers: Vec<String>,
    /// Hot code held to limits of its own.
    pub kernel: Kernel,
    /// What tests may widen, in which crates.
    pub test_only_pub: TestOnlyPub,
    /// Which functions may return `Result<_, String>`.
    pub string_error: StringError,
    /// How `high-fan-out` counts, and how much it lets through.
    pub high_fan_out: HighFanOut,
    /// Which structs guard their fields, and who is inside them.
    pub field_leak: FieldLeak,
    /// Which mutations share a struct.
    pub shared_mutable_bag: SharedMutableBag,
    /// Which lines of a file count.
    pub long_file: LongFile,
    /// Which structs may hold many fields.
    pub wide_struct: WideStruct,
    /// How many functions a type may show.
    pub wide_type: WideType,
    /// How many variants an enum may have.
    pub wide_enum: WideEnum,
    /// How many parameters a function may take, and which are spared.
    pub wide_signature: WideSignature,
    /// Which runs of parameters are traps.
    pub positional_trap: PositionalTraps,
    /// The lints that report nothing unless named here: `rename-getter`,
    /// `one-use-args`, `inner-split`, `redundant-reexport`.
    pub opt_in: Vec<String>,
    /// Which copies of a body are no duplicates.
    pub duplicate_code: DuplicateCode,
    /// How `--update-allow` and `--allow` treat the allowlist.
    pub allow: Allow,
    /// Features that build code only for tests, such as `test-utils`. Set,
    /// even empty, it makes an item that a `cfg` attribute builds only for
    /// `test` or these features test code, as a module is.
    pub test_features: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct DuplicateCode {
    /// Copies in one crate that each are `#[inline(always)]` or
    /// `#[target_feature]` and call the CPU's intrinsics are SIMD helpers
    /// that each kernel module keeps its own of, so that they compile with
    /// its features.
    pub simd: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct SharedMutableBag {
    /// Code that mutates a value its function owns shares it with no one:
    /// a variable whose type, as the index has it, borrows nothing. Through
    /// a `&mut` it still does.
    pub owned_locals: bool,
    /// A type deriving `Deserialize` is a record read from a file, whose
    /// fields its readers set one by one on purpose.
    pub deserialized_records: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Allow {
    /// `--update-allow` writes each new entry with what its diagnostic
    /// measures as its ceiling, `<= N`, which no update raises.
    pub ceilings: bool,
    /// A stale entry allows a new diagnostic that nothing else allows with
    /// its lint, crate, last name and, for a member, type, or a circle no
    /// bigger that shares most of its members: the same debt, moved. A
    /// file's or directory's entry follows no move. `--prune-allow` rewrites
    /// it, and `--strict-allow` fails until then.
    pub follow_moves: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct WideStruct {
    /// A struct with no method of its own that takes `self` is a record
    /// that holds data for others, which a flat list of fields suits.
    pub passive_records: bool,
    /// The most fields such a record may have before it is reported anyway;
    /// unset, any number. At `most-fields` or below, the limit for every
    /// struct, it spares no record.
    pub most_passive_fields: Option<usize>,
    /// A struct deriving clap's `Parser`, `Args` or `Subcommand` is a
    /// command line's schema, flat as its flags are.
    pub clap_derives: bool,
    /// The most fields any struct may hold; default 15.
    pub most_fields: Option<usize>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct WideType {
    /// The most working functions a type may show outside it; default 15.
    pub most_functions: Option<usize>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct WideEnum {
    /// The most variants an enum may have; default 25.
    pub most_variants: Option<usize>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct WideSignature {
    /// The most parameters a function outside the kernel may take; default 7.
    pub most_params: Option<usize>,
    /// A private function called from one place, not counting its calls of
    /// itself, may take any number: a struct of its parameters would be
    /// built once, for that one call.
    pub single_call_site: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct PositionalTraps {
    /// Outside the kernel, a run of this many adjacent parameters of one
    /// type, whatever the type, is a trap too: `a: Vec<u32>, b: Vec<u32>,
    /// c: Vec<u32>`. Unset, only `bool`s and runs of 4 of one primitive are.
    pub same_type_run: Option<usize>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct LongFile {
    /// A test module inline in the file, `#[cfg(test)] mod tests { .. }`,
    /// counts no lines.
    pub skip_test_modules: bool,
    /// A file whose first lines mark it `@generated` is not measured.
    pub skip_generated: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct FieldLeak {
    /// A struct whose fields are all `pub` is plain data, which guards
    /// nothing: `shared-mutable-bag` judges it, not `field-leak`.
    pub plain_data: bool,
    /// Code in a type's home module is inside it for its private fields,
    /// which rustc lets that module use.
    pub home_module: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct HighFanOut {
    /// No `god-hub`: a module that `high-fan-out` reports notes how many
    /// modules depend on it instead, and what is below it is no part of
    /// its fan-out, as it is none of its fan-in.
    pub fold_god_hub: bool,
    /// By a crate of `layers`, the most modules one module of its layer or a
    /// layer above may use; the lowest crate's layer that is at or below a
    /// module's sets its limit. With `fold_god_hub`, a hub keeps the default
    /// if this is looser.
    pub most_by_layer: BTreeMap<String, usize>,
    /// How a module of another crate counts.
    pub other_crates: Granularity,
    /// The most modules one module may use where `most-by-layer` sets no
    /// limit; default 15.
    pub most_modules: Option<usize>,
    /// Modules under one parent count as one: a use of `a::b::x` counts as
    /// one of `a::b`, unless `a::b` holds the module that uses it.
    pub fold_siblings: bool,
}

/// What one use of another crate's module counts as.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Granularity {
    /// Its module.
    #[default]
    Module,
    /// The crate's top module that holds it: `crate::top_module`.
    TopModule,
}

/// Where a `String` error is fine: away from a library's boundary. By
/// default no function may return one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct StringError {
    /// Only functions visible outside their module are reported: a private
    /// one's caller in the module still turns the string into an error.
    pub boundary_only: bool,
    /// Crates, by package or crate name, that end in a person reading the
    /// message: only their `pub` functions are reported.
    pub application_crates: Vec<String>,
}

/// When a test's use is a reason for an item's visibility. By default it
/// never is: an item `pub` for tests elsewhere is `test-only-pub`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct TestOnlyPub {
    /// The crates, by package or crate name, that the policy is for; in
    /// others every test use is reported as today.
    pub crates: Vec<String>,
    /// Tests in other packages, in the crate's own `tests/` and `benches/`,
    /// and a consumer's tests, for a `pub` item. `own-package` takes only
    /// the package's own `tests/` and `benches/`.
    pub external_tests: TestUse,
    /// Tests elsewhere in the crate, for a `pub(crate)` item.
    pub crate_tests: TestUse,
}

/// What a test's use of an item says about its visibility.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TestUse {
    /// Nothing: the item is wider than its production use needs.
    #[default]
    Report,
    /// It justifies the visibility, when the item has a production use too.
    Justify,
    /// For `external-tests`: as `justify`, for tests in the package's own
    /// `tests/` and `benches/` only, not another package's or a consumer's.
    OwnPackage,
}

/// Whether `names`, package names such as `solver-core` or crate names such
/// as `solver_core`, holds the crate named `name`.
pub fn lists(names: &[String], name: &str) -> bool {
    names.iter().any(|listed| listed.replace('-', "_") == name)
}

/// Paths of hot code that numeric work makes wide and long: kernels,
/// tables, inner loops. Its functions and files are held to finite limits of
/// their own; no lint is off for them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Kernel {
    /// Relative to the workspace's root, `/`-separated. A path names a file
    /// or a directory, and its last segment names `<segment>.rs` too, so
    /// that `src/kernel` holds `src/kernel.rs` and what is in `src/kernel/`.
    /// A `*` in a segment stands for any characters in it.
    pub paths: Vec<String>,
    /// The most parameters of a function (`wide-signature`).
    pub most_params: Option<usize>,
    /// The most code lines of a function (`long-function`).
    pub most_lines: Option<usize>,
    /// The most lines of a file (`long-file`).
    pub most_file_lines: Option<usize>,
    /// Which `positional-trap` runs to report.
    pub positional_trap: Traps,
}

/// The parameter runs `positional-trap` reports.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Traps {
    /// Two or more `bool`s, and four or more of one primitive in a row.
    #[default]
    All,
    /// Two or more `bool`s only: a kernel passes runs of numbers.
    Bools,
}

impl Kernel {
    /// Whether the file at `path`, relative to the workspace's root, is
    /// kernel code.
    pub fn holds(&self, path: &str) -> bool {
        self.paths.iter().any(|pattern| holds(pattern, path))
    }
}

/// Whether `pattern` names `path` or a directory above it. `./` and a
/// doubled or trailing `/` change neither.
fn holds(pattern: &str, path: &str) -> bool {
    let parts = |text: &str| -> Vec<String> {
        text.split('/')
            .filter(|part| !matches!(*part, "" | "."))
            .map(str::to_owned)
            .collect()
    };
    let wanted = parts(pattern);
    let segments = parts(path);
    let Some(last) = wanted.len().checked_sub(1) else {
        return false;
    };
    wanted.len() <= segments.len()
        && wanted
            .iter()
            .zip(&segments)
            .enumerate()
            .all(|(at, (want, segment))| {
                fits(want, segment)
                    || (at == last
                        && at + 1 == segments.len()
                        && segment
                            .strip_suffix(".rs")
                            .is_some_and(|stem| fits(want, stem)))
            })
}

/// Whether `text` fits `glob`, where a `*` stands for any characters.
fn fits(glob: &str, text: &str) -> bool {
    match glob.split_once('*') {
        None => glob == text,
        Some((head, rest)) => {
            let Some(tail) = text.strip_prefix(head) else {
                return false;
            };
            (0..=tail.len())
                .filter(|&at| tail.is_char_boundary(at))
                .any(|at| fits(rest, &tail[at..]))
        }
    }
}

impl Settings {
    /// The settings of the workspace at `root`: its `deslop-please.toml`, or
    /// none.
    pub fn read(root: &Path) -> Result<Settings, Error> {
        let path = root.join(FILE);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Settings::default()),
            Err(e) => return Err(Error::new(format!("{}: {e}", path.display()))),
        };
        Settings::parse(&text).map_err(|e| Error::new(format!("{}: {e}", path.display())))
    }

    fn parse(text: &str) -> Result<Settings, Error> {
        let settings: Settings = toml::from_str(text).map_err(|e| Error::new(e.to_string()))?;
        if settings.test_only_pub.crate_tests == TestUse::OwnPackage {
            return Err(Error::new(
                "`test-only-pub.crate-tests` takes `report` or `justify`; `own-package` is for `external-tests`",
            ));
        }
        if settings
            .positional_trap
            .same_type_run
            .is_some_and(|run| run < 2)
        {
            return Err(Error::new(
                "`positional-trap.same-type-run` takes 2 or more: a run is of adjacent parameters",
            ));
        }
        if let Some(name) = settings
            .opt_in
            .iter()
            .find(|name| !OPT_IN.contains(&name.as_str()))
        {
            return Err(Error::new(format!(
                "`opt-in` names `{name}`, which is none of {}",
                OPT_IN.join(", ")
            )));
        }
        if let Some(name) = settings
            .high_fan_out
            .most_by_layer
            .keys()
            .find(|name| settings.layer(name).is_none())
        {
            return Err(Error::new(format!(
                "`high-fan-out.most-by-layer` names `{name}`, which is in no layer of `layers`"
            )));
        }
        Ok(settings)
    }

    /// Whether `opt-in` names `lint`.
    pub fn opts_in(&self, lint: &str) -> bool {
        self.opt_in.iter().any(|name| name == lint)
    }

    /// The layer of the crate named `name`, by package or crate name, if
    /// `layers` holds it.
    pub fn layer(&self, name: &str) -> Option<usize> {
        let name = name.replace('-', "_");
        self.layers.iter().position(|members| lists(members, &name))
    }

    /// The most modules a module of the crate named `name` may use, by
    /// `[high-fan-out]`'s `most-by-layer`, if it sets it.
    pub fn most_fan_out(&self, name: &str) -> Option<usize> {
        let layer = self.layer(name)?;
        self.high_fan_out
            .most_by_layer
            .iter()
            .filter_map(|(from, &most)| Some((self.layer(from)?, most)))
            .filter(|&(from, _)| from <= layer)
            .max_by_key(|&(from, _)| from)
            .map(|(_, most)| most)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layers_are_read_lowest_first_and_an_empty_file_has_none() {
        let toml = "\
# Ordered lowest to highest
layers = [
  [\"cards\", \"solver-ids\"],
  [\"solver-core\"], # middle
  [\"solver-cli\",],
]
";
        assert_eq!(
            Settings::parse(toml).unwrap().layers,
            [
                vec!["cards".to_owned(), "solver-ids".to_owned()],
                vec!["solver-core".to_owned()],
                vec!["solver-cli".to_owned()],
            ]
        );
        assert_eq!(Settings::parse("# empty\n").unwrap(), Settings::default());
    }

    #[test]
    fn a_kernel_path_names_a_file_or_directory_and_a_star_any_characters() {
        let kernel = Kernel {
            paths: vec![
                "./crates/v//src/kernel/".to_owned(),
                "src/regret_*".to_owned(),
            ],
            ..Kernel::default()
        };
        for held in [
            "crates/v/src/kernel.rs",
            "crates/v/src/kernel/mod.rs",
            "crates/v/src/kernel/simd/x86.rs",
            "src/regret_match.rs",
            "src/regret_table/rows.rs",
        ] {
            assert!(kernel.holds(held), "{held}");
        }
        for not in [
            "crates/v/src/kernels.rs",
            "crates/v/src/kernel_util.rs",
            "crates/v/src/kernel.rs/x.rs",
            "crates/v/src",
            "other/src/regret_match.rs",
            "src/regret.rs",
        ] {
            assert!(!kernel.holds(not), "{not}");
        }
    }

    #[test]
    fn a_layers_fan_out_limit_holds_from_its_layer_up_and_names_a_layer() {
        let settings = Settings::parse(
            "layers = [[\"ids\"], [\"core\", \"model\"], [\"app\"], [\"cli\"]]\n\
             [high-fan-out]\nmost-by-layer = { core = 20, app = 25 }\n",
        )
        .unwrap();
        let limits: Vec<Option<usize>> = ["ids", "model", "app", "cli", "other"]
            .into_iter()
            .map(|name| settings.most_fan_out(name))
            .collect();
        assert_eq!(limits, [None, Some(20), Some(25), Some(25), None]);
        let error = Settings::parse("[high-fan-out]\nmost-by-layer = { core = 20 }\n").unwrap_err();
        assert!(error.to_string().contains("`core`"), "{error}");
    }

    #[test]
    fn own_package_is_for_external_tests_only() {
        let settings = Settings::parse("[test-only-pub]\nexternal-tests = \"own-package\"\n");
        assert_eq!(
            settings.unwrap().test_only_pub.external_tests,
            TestUse::OwnPackage
        );
        let error =
            Settings::parse("[test-only-pub]\ncrate-tests = \"own-package\"\n").unwrap_err();
        assert!(error.to_string().contains("`external-tests`"), "{error}");
    }

    #[test]
    fn a_key_this_tool_does_not_know_is_an_error() {
        let error = Settings::parse("layer = [[\"a\"]]\n").unwrap_err();
        assert!(
            error.to_string().contains("unknown field `layer`"),
            "{error}"
        );
        let error = Settings::parse("[positional-trap]\nsame-type-run = 1\n").unwrap_err();
        assert!(error.to_string().contains("2 or more"), "{error}");
        let error = Settings::parse("opt-in = [\"rename-getter\", \"getter\"]\n").unwrap_err();
        assert!(error.to_string().contains("`getter`"), "{error}");
        let settings = Settings::parse("opt-in = [\"inner-split\"]\n").unwrap();
        assert!(settings.opts_in("inner-split") && !settings.opts_in("rename-getter"));
    }
}
