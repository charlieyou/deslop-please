//! `deslop-please.toml` at the workspace's root: what a codebase tells the
//! lints about itself. Every key is optional, and a missing file is the
//! empty one, which keeps every lint at its default. A key this tool does not
//! know is an error, so that a misspelt policy is never silently ignored.

use std::path::Path;

use serde::Deserialize;

use crate::Error;

/// The file's name, at the workspace's root.
pub const FILE: &str = "deslop-please.toml";

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Settings {
    /// The crates by layer, lowest first: a normal dependency must go to a
    /// lower layer (`layer-violation`).
    pub layers: Vec<Vec<String>>,
    /// Directories of crates outside the workspace that use it, relative
    /// to its root: what they use of it is used from another crate.
    pub consumers: Vec<String>,
    /// Hot code held to limits of its own.
    pub kernel: Kernel,
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

    fn parse(text: &str) -> Result<Settings, toml::de::Error> {
        toml::from_str(text)
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
    fn a_key_this_tool_does_not_know_is_an_error() {
        let error = Settings::parse("layer = [[\"a\"]]\n").unwrap_err();
        assert!(
            error.to_string().contains("unknown field `layer`"),
            "{error}"
        );
    }
}
