//! `pass-through`: shallow shims that add a second path without adding
//! abstraction. Catches three forms:
//! 1. An accessor (`Wrapper::Accessor`) beside fields that are already at least
//!    as visible as the accessor (e.g. a getter on a `pub` field).
//! 2. A forwarder (`Wrapper::Forwarder`) that forwards unchanged without hiding
//!    a private field or crossing an `unsafe` boundary.
//! 3. A `pub use <path>::*;` glob re-export of an external crate, non-child
//!    module, or already-`pub` child module (private-child splits like
//!    `mod parts; pub use parts::*;` are allowed).

use std::collections::HashMap;

use super::{
    Diagnostic, Lint, Location, Picture, in_scope, item_location, listed, without_generics,
};
use crate::graph::Graph;
use crate::model::{ItemId, ItemKind, Model, NodeId, Visibility, Wrapper};

pub fn check(model: &Model, graph: &Graph, scope: Option<NodeId>) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    check_wrappers(model, graph, scope, &mut diagnostics);
    check_glob_reexports(model, scope, &mut diagnostics);
    diagnostics
}

fn check_wrappers(
    model: &Model,
    graph: &Graph,
    scope: Option<NodeId>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let tree = &model.tree;
    let mut wrappers: Vec<(ItemId, &Wrapper)> = model
        .wrappers
        .iter()
        .map(|(&item, wrapper)| (item, wrapper))
        .collect();
    wrappers.sort_by_key(|&(item, _)| item);

    for (func, wrapper) in wrappers {
        let item = &model.items[func.index()];
        if item.trait_impl
            || tree.is_test(item.owner)
            || item.file.is_some_and(|file| model.files[file.index()].test)
        {
            continue;
        }
        let home = graph.thing_of[func.index()]
            .map_or(item.owner, |thing| graph.things[thing.index()].home);
        if !in_scope(tree, scope, home) {
            continue;
        }
        match wrapper {
            Wrapper::Accessor {
                lent,
                copied,
                whole,
                ..
            } => {
                // Consuming builders (`fn with_x(mut self, x: X) -> Self`) are
                // ergonomic chaining helpers, not redundant accessors.
                if *whole || item.visibility == Visibility::Private {
                    continue;
                }
                let exposed: Vec<ItemId> = lent.iter().chain(copied).copied().collect();
                let redundant = !exposed.is_empty()
                    && exposed.iter().all(|&field| {
                        let vis = model.items[field.index()].visibility;
                        vis != Visibility::Private && vis >= item.visibility
                    });
                if redundant {
                    let name = without_generics(&item.name);
                    let key = format!("{}::{name}", tree.display(home));
                    let field_names: Vec<String> = exposed
                        .iter()
                        .map(|&f| {
                            let n = &model.items[f.index()].name;
                            n.rsplit("::").next().unwrap_or(n).to_owned()
                        })
                        .collect();
                    let target = listed(&field_names);
                    diagnostics.push(Diagnostic {
                        lint: Lint::PassThrough,
                        key: key.clone(),
                        message: format!(
                            "`{name}` is a redundant accessor: `{target}` is already exposed"
                        ),
                        location: item_location(model, func),
                        notes: vec![
                            "remove the accessor or make the field private with behaviour on its owner"
                                .to_owned(),
                        ],
                        picture: Picture::Arrow {
                            from: key,
                            to: target,
                            detail: "redundant accessor beside exposed field".to_owned(),
                        },
                    });
                }
            }
            Wrapper::Forwarder { target, fields } => {
                let target_item = &model.items[target.index()];
                // A safe wrapper around an `unsafe fn` is a real safety boundary.
                if !item.is_unsafe && target_item.is_unsafe {
                    continue;
                }
                let hides_private_field = !fields.is_empty()
                    && fields.iter().any(|&field| {
                        let vis = model.items[field.index()].visibility;
                        vis == Visibility::Private || vis < item.visibility
                    });
                if hides_private_field {
                    continue;
                }
                let name = without_generics(&item.name);
                let target_name = without_generics(&target_item.name);
                if fields.is_empty() {
                    let same_module = item.owner == target_item.owner;
                    let same_type = item.parent.is_some() && item.parent == target_item.parent;
                    let both_free = item.parent.is_none() && target_item.parent.is_none();
                    let same_name = name.rsplit("::").next().unwrap_or(&name)
                        == target_name.rsplit("::").next().unwrap_or(&target_name);
                    if !same_module && !same_type && !(both_free && same_name) {
                        continue;
                    }
                }
                let target_home = graph.thing_of[target.index()]
                    .map_or(target_item.owner, |thing| graph.things[thing.index()].home);
                let target_full = format!("{}::{target_name}", tree.display(target_home));
                let key = format!("{}::{name}", tree.display(home));
                let detail = if fields.is_empty() {
                    "forwards arguments unchanged".to_owned()
                } else {
                    "forwards through an already-exposed field".to_owned()
                };
                diagnostics.push(Diagnostic {
                    lint: Lint::PassThrough,
                    key: key.clone(),
                    message: format!("`{name}` is a pass-through to `{target_name}`"),
                    location: item_location(model, func),
                    notes: vec![format!("calls `{target_full}` with the same arguments")],
                    picture: Picture::Arrow {
                        from: key,
                        to: target_full,
                        detail,
                    },
                });
            }
        }
    }
}

fn check_glob_reexports(model: &Model, scope: Option<NodeId>, diagnostics: &mut Vec<Diagnostic>) {
    let tree = &model.tree;
    let mut module_visibility: HashMap<NodeId, Visibility> = HashMap::new();
    for item in &model.items {
        if item.kind == ItemKind::Module
            && let Some(module) = item.module
        {
            let entry = module_visibility
                .entry(module)
                .or_insert(Visibility::Private);
            if item.visibility > *entry {
                *entry = item.visibility;
            }
        }
    }

    for glob in &model.glob_reexports {
        if glob.test || !in_scope(tree, scope, glob.from) {
            continue;
        }
        // `mod parts; pub use parts::*;` is a private file-split, not a leaky
        // re-export facade.
        let private_child = glob.target.is_some_and(|target| {
            tree.parent(target) == Some(glob.from)
                && module_visibility.get(&target).copied().unwrap_or_default() != Visibility::Pub
        });
        if private_child {
            continue;
        }
        let from_name = tree.display(glob.from);
        let key = format!("{from_name}::{}::*", glob.path);
        let reason = match glob.target {
            Some(target) if tree.parent(target) == Some(glob.from) => {
                "target child module is already `pub` (duplicate export surface)"
            }
            Some(_) => "re-exports another module's entire namespace",
            None => "re-exports an external crate's entire namespace",
        };
        diagnostics.push(Diagnostic {
            lint: Lint::PassThrough,
            key,
            message: format!("`{from_name}` glob re-exports `{}::*`", glob.path),
            location: Some(Location {
                file: model.files[glob.file.index()].path.clone(),
                line: glob.line,
            }),
            notes: vec![reason.to_owned()],
            picture: Picture::Arrow {
                from: from_name,
                to: format!("{}::*", glob.path),
                detail: reason.to_owned(),
            },
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{FileId, GlobReexport, tests::Sample};

    fn keys(sample: &Sample) -> Vec<String> {
        let graph = Graph::new(&sample.model);
        check(&sample.model, &graph, None)
            .into_iter()
            .map(|d| d.key)
            .collect()
    }

    #[test]
    fn accessor_beside_pub_field_is_pass_through_but_private_field_is_not() {
        let mut sample = Sample::new();
        let m = sample.module("canon");
        let ty = sample.item(m, "Canon", ItemKind::Type);
        let pub_field = sample.member(ty, m, "Canon::engine", ItemKind::Field);
        let priv_field = sample.member(ty, m, "Canon::secret", ItemKind::Field);
        sample.model.items[pub_field.index()].visibility = Visibility::Pub;

        let pub_getter = sample.member(ty, m, "Canon::engine", ItemKind::Function);
        sample.model.items[pub_getter.index()].visibility = Visibility::Pub;
        sample.model.wrappers.insert(
            pub_getter,
            Wrapper::Accessor {
                lent: vec![pub_field],
                copied: vec![],
                to_read: true,
                whole: false,
            },
        );

        let priv_getter = sample.member(ty, m, "Canon::secret", ItemKind::Function);
        sample.model.items[priv_getter.index()].visibility = Visibility::Pub;
        sample.model.wrappers.insert(
            priv_getter,
            Wrapper::Accessor {
                lent: vec![priv_field],
                copied: vec![],
                to_read: true,
                whole: false,
            },
        );

        assert_eq!(keys(&sample), ["p::canon::Canon::engine"]);
    }

    #[test]
    fn pure_forwarder_and_pub_field_forwarder_are_flagged_but_private_delegation_is_not() {
        let mut sample = Sample::new();
        let m = sample.module("queue");
        let outer = sample.item(m, "Outer", ItemKind::Type);
        let inner = sample.item(m, "Inner", ItemKind::Type);
        let target = sample.member(inner, m, "Inner::push", ItemKind::Function);
        let priv_field = sample.member(outer, m, "Outer::slots", ItemKind::Field);
        let pub_field = sample.member(outer, m, "Outer::exposed", ItemKind::Field);
        sample.model.items[pub_field.index()].visibility = Visibility::Pub;

        // Delegating to a private field hides `Inner` and is allowed.
        let hidden_fwd = sample.member(outer, m, "Outer::push", ItemKind::Function);
        sample.model.items[hidden_fwd.index()].visibility = Visibility::Pub;
        sample.model.wrappers.insert(
            hidden_fwd,
            Wrapper::Forwarder {
                target,
                fields: vec![priv_field],
            },
        );

        // Delegating to a pub field is a pass-through.
        let pub_fwd = sample.member(outer, m, "Outer::push_exposed", ItemKind::Function);
        sample.model.items[pub_fwd.index()].visibility = Visibility::Pub;
        sample.model.wrappers.insert(
            pub_fwd,
            Wrapper::Forwarder {
                target,
                fields: vec![pub_field],
            },
        );

        // Pure trampoline with no fields is a pass-through.
        let alias = sample.item(m, "alias_push", ItemKind::Function);
        sample.model.items[alias.index()].visibility = Visibility::Pub;
        sample.model.wrappers.insert(
            alias,
            Wrapper::Forwarder {
                target,
                fields: vec![],
            },
        );

        assert_eq!(
            keys(&sample),
            ["p::queue::Outer::push_exposed", "p::queue::alias_push"]
        );
    }

    #[test]
    fn private_child_glob_reexport_is_allowed_but_pub_child_and_external_are_flagged() {
        let mut sample = Sample::new();
        let root = sample.module("");
        let priv_child = sample.module("parts");
        let pub_child = sample.module("dto");
        let priv_mod = sample.item(root, "parts", ItemKind::Module);
        sample.model.items[priv_mod.index()].module = Some(priv_child);
        let pub_mod = sample.item(root, "dto", ItemKind::Module);
        sample.model.items[pub_mod.index()].module = Some(pub_child);
        sample.model.items[pub_mod.index()].visibility = Visibility::Pub;

        let file = FileId::new(0);
        sample.model.glob_reexports = vec![
            GlobReexport {
                file,
                line: 1,
                from: root,
                path: "parts".into(),
                target: Some(priv_child),
                test: false,
            },
            GlobReexport {
                file,
                line: 2,
                from: root,
                path: "dto".into(),
                target: Some(pub_child),
                test: false,
            },
            GlobReexport {
                file,
                line: 3,
                from: root,
                path: "other_crate".into(),
                target: None,
                test: false,
            },
        ];

        assert_eq!(keys(&sample), ["p::dto::*", "p::other_crate::*"]);
    }
}
