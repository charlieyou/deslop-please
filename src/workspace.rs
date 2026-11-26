//! The Cargo workspace around the current directory, as `cargo metadata` describes it.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use crate::Error;
use crate::settings::Settings;

pub struct Workspace {
    pub root: PathBuf,
    pub target_directory: PathBuf,
    pub packages: Vec<Package>,
    /// What its `deslop-please.toml` says.
    pub settings: Settings,
}

pub struct Package {
    pub name: String,
    pub version: String,
    /// How code names the package's library: `my_crate` for `my-crate`.
    pub crate_name: String,
    /// Relative to the workspace's root, `/`-separated; empty for the root itself.
    pub directory: String,
    /// Whether Cargo allows publishing the package (`false` for `publish = false`).
    pub publish: bool,
    pub targets: Vec<Target>,
    /// Dependencies declared in this package's `Cargo.toml`.
    pub dependencies: Vec<Dependency>,
    /// Dependency names referenced in `[features]` (e.g. `dep:foo` or `foo/bar`).
    pub feature_deps: HashSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dependency {
    pub name: String,
    /// How Rust code imports the dependency (`rename` if set, else `name` with `-` -> `_`).
    pub crate_name: String,
    /// Normal `[dependencies]` (excludes dev and build dependencies).
    pub normal: bool,
    pub optional: bool,
}

pub struct Target {
    pub kind: TargetKind,
    /// The crate's root file, relative to the workspace's root.
    pub root: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TargetKind {
    Library,
    Binary,
    Example,
    Test,
    Bench,
    BuildScript,
}

impl TargetKind {
    pub fn is_test(self) -> bool {
        matches!(self, TargetKind::Test | TargetKind::Bench)
    }
}

impl Workspace {
    pub fn discover() -> Result<Workspace, Error> {
        let cwd = std::env::current_dir()
            .map_err(|e| Error::new(format!("cannot read current directory: {e}")))?;
        Workspace::discover_in(&cwd)
    }

    pub fn discover_in(dir: &Path) -> Result<Workspace, Error> {
        let output = Command::new("cargo")
            .args(["metadata", "--format-version", "1", "--no-deps"])
            .current_dir(dir)
            .output()
            .map_err(|e| Error::new(format!("cannot run cargo: {e}")))?;
        if !output.status.success() {
            return Err(Error::new(format!(
                "not inside a Cargo workspace: cargo metadata says: {}",
                cargo_error(&output.stderr)
            )));
        }
        let metadata: Value = serde_json::from_slice(&output.stdout)
            .map_err(|e| Error::new(format!("cargo metadata's output is not JSON: {e}")))?;
        Workspace::from_metadata(&metadata)
    }

    fn from_metadata(metadata: &Value) -> Result<Workspace, Error> {
        let root = PathBuf::from(text(metadata, "workspace_root")?);
        let target_directory = PathBuf::from(text(metadata, "target_directory")?);
        let packages: Vec<Package> = list(metadata, "packages")?
            .iter()
            .map(|package| Package::from_metadata(package, &root))
            .collect::<Result<_, _>>()?;
        let settings = Settings::read(&root)?;
        let unlayered: Vec<&str> = packages
            .iter()
            .map(|package| package.name.as_str())
            .filter(|name| settings.layer(name).is_none())
            .collect();
        if settings.every_member_layered && !unlayered.is_empty() {
            return Err(Error::new(format!(
                "{}: `layers` leaves out {}, and `every-member-layered` wants each member in a layer",
                crate::settings::FILE,
                unlayered.join(", ")
            )));
        }
        Ok(Workspace {
            root,
            target_directory,
            packages,
            settings,
        })
    }

    /// The files whose change can change the index: every package's Rust
    /// sources and manifest, and the lock file.
    pub fn source_files(&self) -> Result<Vec<String>, Error> {
        let mut files = vec!["Cargo.toml".to_owned(), "Cargo.lock".to_owned()];
        for package in &self.packages {
            let directory = self.root.join(&package.directory);
            collect_sources(&directory, &self.root, &self.target_directory, &mut files)?;
        }
        files.sort();
        files.dedup();
        Ok(files)
    }
}

impl Package {
    fn from_metadata(package: &Value, root: &Path) -> Result<Package, Error> {
        let name = text(package, "name")?.to_owned();
        let manifest = relative(Path::new(text(package, "manifest_path")?), root)?;
        let directory = manifest
            .rsplit_once('/')
            .map_or("", |(directory, _)| directory)
            .to_owned();
        let mut targets = Vec::new();
        let mut library_name = None;
        for target in list(package, "targets")? {
            let kind = target_kind(target)?;
            if kind == TargetKind::Library {
                library_name = Some(text(target, "name")?.to_owned());
            }
            let root = relative(Path::new(text(target, "src_path")?), root)?;
            targets.push(Target { kind, root });
        }
        let publish = package
            .get("publish")
            .and_then(Value::as_array)
            .is_none_or(|registries| !registries.is_empty());
        let dependencies = parse_dependencies(package);
        let feature_deps = parse_feature_deps(package);
        Ok(Package {
            crate_name: library_name.unwrap_or_else(|| name.replace('-', "_")),
            version: text(package, "version")?.to_owned(),
            name,
            directory,
            publish,
            targets,
            dependencies,
            feature_deps,
        })
    }
}

fn parse_dependencies(package: &Value) -> Vec<Dependency> {
    let Some(deps) = package.get("dependencies").and_then(Value::as_array) else {
        return Vec::new();
    };
    deps.iter()
        .filter_map(|dep| {
            let name = dep.get("name")?.as_str()?.to_owned();
            let rename = dep.get("rename").and_then(Value::as_str);
            let crate_name = rename.unwrap_or(&name).replace('-', "_");
            let normal = dep.get("kind").is_none_or(Value::is_null);
            let optional = dep
                .get("optional")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            Some(Dependency {
                name,
                crate_name,
                normal,
                optional,
            })
        })
        .collect()
}

fn parse_feature_deps(package: &Value) -> HashSet<String> {
    let mut deps = HashSet::new();
    let Some(features) = package.get("features").and_then(Value::as_object) else {
        return deps;
    };
    for values in features.values() {
        let Some(entries) = values.as_array() else {
            continue;
        };
        for entry in entries.iter().filter_map(Value::as_str) {
            if let Some(name) = entry.strip_prefix("dep:") {
                deps.insert(name.to_owned());
            } else if let Some((head, _)) = entry.split_once('/') {
                deps.insert(head.trim_end_matches('?').to_owned());
            }
        }
    }
    deps
}

fn target_kind(target: &Value) -> Result<TargetKind, Error> {
    let kinds = list(target, "kind")?;
    let first = kinds.first().and_then(Value::as_str).unwrap_or_default();
    Ok(match first {
        "bin" => TargetKind::Binary,
        "example" => TargetKind::Example,
        "test" => TargetKind::Test,
        "bench" => TargetKind::Bench,
        "custom-build" => TargetKind::BuildScript,
        "lib" | "rlib" | "dylib" | "cdylib" | "staticlib" | "proc-macro" => TargetKind::Library,
        other => {
            return Err(Error::new(format!(
                "cargo metadata has a target of an unknown kind: {other:?}"
            )));
        }
    })
}

fn collect_sources(
    directory: &Path,
    root: &Path,
    target: &Path,
    files: &mut Vec<String>,
) -> Result<(), Error> {
    let unreadable = |e| Error::new(format!("cannot read {}: {e}", directory.display()));
    for entry in std::fs::read_dir(directory).map_err(unreadable)? {
        let entry = entry.map_err(unreadable)?;
        let path = entry.path();
        let hidden = path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with('.'));
        if hidden || path == target {
            continue;
        }
        // Not through symbolic links, which can loop.
        let file_type = entry.file_type().map_err(unreadable)?;
        if file_type.is_dir() {
            collect_sources(&path, root, target, files)?;
        } else if file_type.is_file()
            && (path.extension().is_some_and(|extension| extension == "rs")
                || path.file_name().is_some_and(|name| name == "Cargo.toml"))
        {
            files.push(relative(&path, root)?);
        }
    }
    Ok(())
}

fn relative(path: &Path, root: &Path) -> Result<String, Error> {
    let inside = path.strip_prefix(root).map_err(|_| {
        Error::new(format!(
            "{} is outside the workspace's root {}",
            path.display(),
            root.display()
        ))
    })?;
    let parts: Vec<_> = inside
        .components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect();
    Ok(parts.join("/"))
}

fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str, Error> {
    value[key].as_str().ok_or_else(|| missing(key))
}

fn list<'a>(value: &'a Value, key: &str) -> Result<&'a Vec<Value>, Error> {
    value[key].as_array().ok_or_else(|| missing(key))
}

fn missing(key: &str) -> Error {
    Error::new(format!("cargo metadata's output has no {key:?}"))
}

/// What cargo wrote to `stderr` when it failed, without the lines that say
/// it waited for a lock another cargo held.
pub fn cargo_error(stderr: &[u8]) -> String {
    String::from_utf8_lossy(stderr)
        .lines()
        .filter(|line| {
            !line
                .trim_start()
                .starts_with("Blocking waiting for file lock")
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_packages_their_directories_and_targets() {
        let metadata = serde_json::json!({
            "workspace_root": "/w",
            "target_directory": "/w/target",
            "packages": [{
                "name": "my-tool",
                "version": "0.2.0",
                "manifest_path": "/w/tools/my-tool/Cargo.toml",
                "targets": [
                    {"kind": ["lib"], "name": "my_tool", "src_path": "/w/tools/my-tool/src/lib.rs"},
                    {"kind": ["test"], "name": "it", "src_path": "/w/tools/my-tool/tests/it.rs"}
                ]
            }, {
                "name": "root-bin",
                "version": "1.0.0",
                "publish": [],
                "manifest_path": "/w/Cargo.toml",
                "targets": [{"kind": ["bin"], "name": "root-bin", "src_path": "/w/src/main.rs"}]
            }]
        });
        let workspace = Workspace::from_metadata(&metadata).unwrap();
        let tool = &workspace.packages[0];
        assert_eq!(
            (
                tool.directory.as_str(),
                tool.crate_name.as_str(),
                tool.publish
            ),
            ("tools/my-tool", "my_tool", true)
        );
        assert_eq!(tool.targets[1].kind, TargetKind::Test);
        assert_eq!(tool.targets[1].root, "tools/my-tool/tests/it.rs");
        let root_bin = &workspace.packages[1];
        assert_eq!(
            (
                root_bin.directory.as_str(),
                root_bin.crate_name.as_str(),
                root_bin.publish
            ),
            ("", "root_bin", false)
        );
    }
}
