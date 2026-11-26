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
    fn a_key_this_tool_does_not_know_is_an_error() {
        let error = Settings::parse("layer = [[\"a\"]]\n").unwrap_err();
        assert!(
            error.to_string().contains("unknown field `layer`"),
            "{error}"
        );
    }
}
