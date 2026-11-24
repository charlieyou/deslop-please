//! The plain model the graph is built from: the hierarchy, the workspace's
//! files, the items they define and what each is written for, every reference
//! from code to an item outside `use` declarations, and the functions that
//! only wrap others.

mod from_index;
mod tree;

pub use from_index::build;
pub use tree::{NodeId, NodeKind, Tree};

use std::collections::{HashMap, HashSet};

use crate::workspace::TargetKind;

#[derive(Debug, Default)]
pub struct Model {
    pub tree: Tree,
    /// By [`FileId`].
    pub files: Vec<File>,
    /// By [`ItemId`].
    pub items: Vec<Item>,
    pub references: Vec<Reference>,
    /// What each wrapper hands on. No chain of forwarders loops.
    pub wrappers: HashMap<ItemId, Wrapper>,
    /// `pub use <path>::*;` and `pub(...) use <path>::*;` glob re-exports.
    pub glob_reexports: Vec<GlobReexport>,
    /// Per-module count of `pub use` / `pub(...) use` declarations.
    pub module_reexports: Vec<ModuleReexports>,
    /// Normal dependencies declared by each workspace package, with usage info.
    pub crate_deps: Vec<CrateDep>,
    /// Optional crate layer table from `deslop-please.toml`, ordered lowest to highest.
    pub layers: Vec<Vec<String>>,
    /// Structs/enums deriving `Serialize` or `Deserialize`.
    pub serde_types: HashSet<ItemId>,
    /// Workspace `trait` items.
    pub traits: HashSet<ItemId>,
    /// Observed `impl Trait for Type` blocks keyed by the trait's [`ItemId`].
    pub trait_impls: HashMap<ItemId, Vec<TraitImpl>>,
}

/// One `impl Trait for Type` block observed in the workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TraitImpl {
    pub owner: NodeId,
    pub test: bool,
    /// True when `Type` is a concrete workspace type (false for std/external
    /// extension traits and blanket `impl<T> Trait for T`).
    pub local_self: bool,
}

/// Normalized structural fingerprint of a function body for clone detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BodyFingerprint {
    pub hash_a: u64,
    pub hash_b: u64,
    pub tokens: u32,
}

/// Positional argument-swap hazard in a function's parameter list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PositionalTrap {
    /// Two or more `bool` parameters.
    Booleans(u16),
    /// Four or more consecutive parameters of the same primitive type.
    RepeatedPrimitive { count: u16, kind: &'static str },
}

/// Structural metrics of a defined function.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FunctionMetrics {
    pub lines: u32,
    pub branches: u32,
    pub params: u16,
    pub returns_string_error: bool,
    pub body_fingerprint: Option<BodyFingerprint>,
    pub positional_trap: Option<PositionalTrap>,
}

/// An item's declared visibility, ordered from narrowest to widest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Visibility {
    #[default]
    Private,
    /// `pub(crate)`, `pub(super)`, or `pub(in ...)`.
    Restricted,
    /// Plain `pub`.
    Pub,
}

/// A `pub use <path>::*;` or `pub(...) use <path>::*;` declaration written in a module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobReexport {
    pub file: FileId,
    pub line: u32,
    pub from: NodeId,
    /// The path written before `::*`.
    pub path: String,
    /// The target module if it is a workspace module.
    pub target: Option<NodeId>,
    /// True for `pub(crate)` / `pub(super)` / `pub(in ...)`, false for plain `pub`.
    pub restricted: bool,
    pub test: bool,
}

/// Count of `pub use` / `pub(...) use` declarations written in a module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleReexports {
    pub file: FileId,
    pub line: u32,
    pub module: NodeId,
    pub count: usize,
    pub test: bool,
}

/// A normal `[dependencies]` entry of a workspace package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrateDep {
    pub package: NodeId,
    pub package_name: String,
    pub dep_name: String,
    pub dep_crate: String,
    pub optional: bool,
    pub used: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FileId(u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ItemId(u32);

#[derive(Debug)]
pub struct File {
    /// Relative to the workspace's root, `/`-separated.
    pub path: String,
    pub module: NodeId,
    pub test: bool,
    pub target: Option<TargetKind>,
    /// Every line of its text, blank and comment lines too.
    pub lines: usize,
}

#[derive(Debug)]
pub struct Item {
    /// How its module writes it: `Ready`, `Ready::new`, `<u64 as Display>::fmt`.
    pub name: String,
    /// The module it is written in.
    pub owner: NodeId,
    /// The module it is, for a module.
    pub module: Option<NodeId>,
    pub file: Option<FileId>,
    /// 1-based line of its definition, or 0 if not known.
    pub line: u32,
    pub kind: ItemKind,
    /// What it is written for: a type, for its fields, variants, associated
    /// items and methods, wherever their `impl` block is; a variant, for its
    /// fields. `None` for a free item, and for what an `impl` block of a type
    /// the crate does not define holds.
    pub parent: Option<ItemId>,
    /// For a free function: the workspace type of its first parameter, if it
    /// names one (so `fn op(store: &mut Store, ..)` is attributed to `Store`).
    pub first_param_type: Option<ItemId>,
    pub visibility: Visibility,
    /// A function of a trait's `impl` block.
    pub trait_impl: bool,
    /// A function with a `self` parameter.
    pub takes_self: bool,
    /// A function declared `unsafe fn`.
    pub is_unsafe: bool,
    /// An item declared with an explicit `extern "..."` ABI.
    pub extern_abi: bool,
    /// A field of a marker type, `PhantomData` or `PhantomPinned`, which
    /// holds nothing.
    pub holds_nothing: bool,
    /// Structural size and parameter metrics for defined functions.
    pub metrics: Option<FunctionMetrics>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    Module,
    /// A struct, enum, union, trait, or type alias.
    Type,
    /// An `impl` block, which code names as `Self`.
    Impl,
    Function,
    Field,
    Variant,
    /// A constant or static.
    Constant,
    Other,
}

#[derive(Debug)]
pub struct Reference {
    pub file: FileId,
    pub line: u32,
    /// The module it is written in.
    pub from: NodeId,
    /// The innermost item whose definition holds it: a function, type, field,
    /// variant or constant. `None` in an `impl` block's header.
    pub holder: Option<ItemId>,
    pub item: ItemId,
    /// For a field: the code only reads its value. It takes a copy, or it
    /// borrows plain data to read, or calls a method on a number; it neither
    /// writes the field nor reaches into it. For a call of an accessor,
    /// directly or through forwarders: the code reads so what it lends.
    pub copies: bool,
    /// For a field: the code writes it, borrows it mutably, or calls a
    /// mutating method on it (not a plain read, shared borrow, or read index).
    pub mutates: bool,
    pub test: bool,
}

/// A function that adds nothing of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wrapper {
    /// Calls `target` and nothing else, passing its parameters on.
    /// `fields`: the fields of `self` it calls that function on.
    Forwarder { target: ItemId, fields: Vec<ItemId> },
    /// Hands out or overwrites fields of `self`, computing nothing: `lent`
    /// are those it lends or overwrites, `copied` those whose values it only
    /// hands out. `to_read`: it takes `&self` and overwrites nothing, so
    /// what it lends can only be read, and code that calls it does with the
    /// fields what it does with the call. `whole`: takes `self` by value
    /// (e.g. a consuming builder `fn with_x(mut self, x: X) -> Self`).
    Accessor {
        lent: Vec<ItemId>,
        copied: Vec<ItemId>,
        to_read: bool,
        whole: bool,
    },
}

impl FileId {
    pub fn new(index: usize) -> FileId {
        FileId(u32::try_from(index).expect("fewer than 2^32 files"))
    }

    pub fn index(self) -> usize {
        self.0 as usize
    }
}

impl ItemId {
    pub fn new(index: usize) -> ItemId {
        ItemId(u32::try_from(index).expect("fewer than 2^32 items"))
    }

    pub fn index(self) -> usize {
        self.0 as usize
    }
}

#[cfg(test)]
pub mod tests {
    //! Small models for the graph's and the lints' tests.

    use super::*;

    /// A model of one crate, `p`, with one file for each module.
    pub struct Sample {
        pub model: Model,
    }

    impl Sample {
        pub fn new() -> Sample {
            let mut tree = Tree::new();
            tree.child(tree.root(), NodeKind::Package, "p");
            Sample {
                model: Model {
                    tree,
                    ..Model::default()
                },
            }
        }

        /// The module at `path` below the crate, `""` for the crate itself.
        pub fn module(&mut self, path: &str) -> NodeId {
            let tree = &mut self.model.tree;
            let mut node = tree.child(tree.root(), NodeKind::Package, "p");
            for name in path.split("::").filter(|name| !name.is_empty()) {
                node = tree.child(node, NodeKind::Module, name);
            }
            if !self.model.files.iter().any(|file| file.module == node) {
                self.model.files.push(File {
                    path: format!("src/{}.rs", path.replace("::", "/")),
                    module: node,
                    test: false,
                    target: Some(TargetKind::Library),
                    lines: 1,
                });
            }
            node
        }

        /// A file of `module`'s at `path`, of `lines` lines.
        pub fn file(&mut self, module: NodeId, path: &str, lines: usize) {
            self.model.files.push(File {
                path: path.into(),
                module,
                test: false,
                target: Some(TargetKind::Library),
                lines,
            });
        }

        pub fn item(&mut self, owner: NodeId, name: &str, kind: ItemKind) -> ItemId {
            let file = self
                .model
                .files
                .iter()
                .position(|file| file.module == owner);
            self.model.items.push(Item {
                name: name.into(),
                owner,
                module: None,
                file: file.map(FileId::new),
                line: 1,
                kind,
                parent: None,
                first_param_type: None,
                visibility: Visibility::Private,
                trait_impl: false,
                takes_self: false,
                is_unsafe: false,
                extern_abi: false,
                holds_nothing: false,
                metrics: None,
            });
            ItemId::new(self.model.items.len() - 1)
        }

        /// An item of `parent`'s, written in `owner`.
        pub fn member(
            &mut self,
            parent: ItemId,
            owner: NodeId,
            name: &str,
            kind: ItemKind,
        ) -> ItemId {
            let id = self.item(owner, name, kind);
            self.model.items[id.index()].parent = Some(parent);
            id
        }

        /// A reference to `item` in `holder`'s code, on a line of its own.
        pub fn refer(&mut self, holder: ItemId, item: ItemId) {
            let from = self.model.items[holder.index()].owner;
            let file = self.model.files.iter().position(|file| file.module == from);
            self.model.references.push(Reference {
                file: FileId::new(file.expect("a module of the sample's")),
                line: u32::try_from(self.model.references.len()).unwrap() + 1,
                from,
                holder: Some(holder),
                item,
                copies: false,
                mutates: false,
                test: false,
            });
        }

        /// The same, where the code only takes a copy of `item`'s value.
        pub fn read(&mut self, holder: ItemId, item: ItemId) {
            self.refer(holder, item);
            self.model.references.last_mut().expect("just added").copies = true;
        }
    }
}
