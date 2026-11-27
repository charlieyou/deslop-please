//! Indirections that add nothing, each reported only when `opt-in` names
//! it, so that a codebase can judge its signal first:
//! - `rename-getter`: a method whose body only hands out one field of
//!   `self`, by reference or by copy, under its name or another, when it
//!   adds nothing to reading the field: only tests call it, or none; every
//!   caller sees the field, `pub` or in reach of its module; or its type is
//!   a record, with no method but getters and other fields as visible as
//!   it. Such a getter keeps `field-leak` quiet without hiding anything.
//!   `[rename-getter] all-private-fields` reports every one, also a
//!   read-only accessor of a field its type maintains.
//! - `one-use-args`: a struct built at one place, in the one function that
//!   passes it to another, which alone reads it: a parameter list moved
//!   into a type.
//! - `inner-split`: a private function, not recursive, called only by one
//!   other function, not generic, whose body is that one call: `run` and
//!   `run_inner`, one function split in two.
//! - `redundant-reexport`: a `pub use` of an item that is already public
//!   on another path of the crate: where it is defined, every module above
//!   it `pub`, or the shallowest public module's `pub use` that names it
//!   there, as for another crate's item any is.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::graph::Graph;
use crate::lint::{
    Diagnostic, EncapsulationLint, Lint, Location, Picture, clickable, in_scope, item_location,
    without_generics,
};
use crate::model::{FileId, ItemId, ItemKind, Model, NodeId, Visibility, Wrapper};
use crate::workspace::TargetKind;

pub fn check(model: &Model, graph: &Graph, scope: Option<NodeId>) -> Vec<Diagnostic> {
    let settings = &model.settings;
    let mut diagnostics = Vec::new();
    if settings.opts_in("rename-getter") {
        diagnostics.extend(rename_getters(model, graph, scope));
    }
    if settings.opts_in("one-use-args") {
        diagnostics.extend(one_use_args(model, graph, scope));
    }
    if settings.opts_in("inner-split") {
        diagnostics.extend(inner_splits(model, graph, scope));
    }
    if settings.opts_in("redundant-reexport") {
        diagnostics.extend(redundant_reexports(model, scope));
    }
    diagnostics
}

/// `crate::module::Type::name`, and where `item` is defined.
fn key(model: &Model, graph: &Graph, item: ItemId) -> (String, NodeId) {
    let home = graph.thing_of[item.index()].map_or(model.items[item.index()].owner, |thing| {
        graph.things[thing.index()].home
    });
    let name = without_generics(&model.items[item.index()].name);
    (format!("{}::{name}", model.tree.display(home)), home)
}

/// `items` of `Queue::items`.
fn last_name(model: &Model, item: ItemId) -> String {
    let name = without_generics(&model.items[item.index()].name);
    name.rsplit("::").next().unwrap_or(&name).to_owned()
}

fn rename_getters(model: &Model, graph: &Graph, scope: Option<NodeId>) -> Vec<Diagnostic> {
    let mut getters: Vec<(ItemId, ItemId, bool)> = model
        .wrappers
        .iter()
        .filter_map(|(&getter, wrapper)| match wrapper {
            Wrapper::Accessor {
                lent,
                copied,
                to_read: true,
                whole: false,
                bare: true,
            } => match (&lent[..], &copied[..]) {
                ([field], []) => Some((getter, *field, false)),
                ([], [field]) => Some((getter, *field, true)),
                _ => None,
            },
            _ => None,
        })
        .filter(|&(getter, _, _)| {
            let item = &model.items[getter.index()];
            !model.in_tests(item) && item.metrics.is_some_and(|metrics| metrics.params == 0)
        })
        .collect();
    getters.sort();
    // By getter, where the code that calls it is, and whether it is a
    // test's; a consumer's is nowhere in the workspace.
    let mut callers: HashMap<ItemId, Vec<Caller>> = getters
        .iter()
        .map(|&(getter, _, _)| (getter, Vec::new()))
        .collect();
    for reference in &model.references {
        if reference.holder != Some(reference.item)
            && let Some(found) = callers.get_mut(&reference.item)
        {
            found.push(Caller {
                at: Some((reference.file, reference.from)),
                test: reference.test,
            });
        }
    }
    for mention in &model.mentions {
        if let Some(found) = callers.get_mut(&mention.item) {
            found.push(Caller {
                at: mention.from,
                test: mention.test,
            });
        }
    }
    let every = model.settings.rename_getter.all_private_fields;
    let mut diagnostics = Vec::new();
    for (getter, field, copies) in getters {
        let (key, home) = key(model, graph, getter);
        if !in_scope(&model.tree, scope, home) {
            continue;
        }
        let name = without_generics(&model.items[getter.index()].name);
        let field_name = last_name(model, field);
        let Some(why) = needless(model, getter, field, &callers[&getter]).or_else(|| {
            every.then(|| {
                "what do its callers do with the field? that is the method to write".to_owned()
            })
        }) else {
            continue;
        };
        let renamed = last_name(model, getter) != field_name;
        let how = if copies { "a copy of" } else { "a borrow of" };
        let mut notes = vec![why];
        if renamed {
            notes.push(format!("it names `{field_name}` another way"));
        }
        diagnostics.push(Diagnostic {
            lint: Lint::Encapsulation(EncapsulationLint::RenameGetter),
            key: key.clone(),
            message: format!("`{name}` only hands out {how} `{field_name}`"),
            location: item_location(model, getter),
            notes,
            picture: Picture::Arrow {
                from: key,
                to: field_name,
                detail: if renamed {
                    "renames a field".to_owned()
                } else {
                    "hands out a field".to_owned()
                },
            },
        });
    }
    diagnostics
}

/// Where code that calls a getter is: its file and module, or `None` for a
/// consumer's.
struct Caller {
    at: Option<(FileId, NodeId)>,
    test: bool,
}

/// Why `getter`, which hands out `field`, adds nothing to reading the field,
/// given its `callers`, if it does not: no code but tests calls it, every
/// caller could read the field itself, or its type is a record whose other
/// fields are as visible as the getter. A getter that keeps a field its own
/// type maintains read-only to others is none of these.
fn needless(model: &Model, getter: ItemId, field: ItemId, callers: &[Caller]) -> Option<String> {
    let tree = &model.tree;
    // A published library's `pub` getter has callers no index shows.
    let getter_item = &model.items[getter.index()];
    let published = getter_item.visibility == Visibility::Pub
        && getter_item
            .file
            .is_some_and(|file| model.files[file.index()].target == Some(TargetKind::Library))
        && tree
            .package_of(getter_item.owner)
            .is_some_and(|package| tree.node(package).publish);
    if !published && callers.iter().all(|caller| caller.test) {
        return Some(if callers.is_empty() {
            "nothing calls it: remove it".to_owned()
        } else {
            "only tests call it: what do they need from the type?".to_owned()
        });
    }
    let item = &model.items[field.index()];
    let name = last_name(model, field);
    let ty = item.parent?;
    let ty_item = &model.items[ty.index()];
    let home = ty_item.owner;
    let package = tree.package_of(home);
    let target = ty_item
        .file
        .and_then(|file| model.files[file.index()].target);
    if item.visibility == Visibility::Pub {
        return Some(format!("`{name}` is `pub`: its callers can read it"));
    }
    // Where the field is surely visible: below its type's module, private;
    // below its parent, restricted, as `pub(super)` and all wider are.
    let seen_below = match item.visibility {
        Visibility::Restricted if Some(home) != package => tree.parent(home)?,
        _ => home,
    };
    let sees = |caller: &Caller| {
        caller.at.is_some_and(|(file, from)| {
            let file = &model.files[file.index()];
            tree.package_of(from) == package
                && file.target == target
                && tree.is_within(from, seen_below)
        })
    };
    if !published && callers.iter().all(sees) {
        return Some(format!(
            "every caller is in `{}`, which sees `{name}`: let them read it",
            tree.display(seen_below)
        ));
    }
    // A record: its other fields are as visible as the getter, and its
    // functions only hand out fields; no trait's function, constructor or
    // method keeps `field`.
    let mut others = model
        .items
        .iter()
        .enumerate()
        .filter(|&(at, other)| {
            other.kind == ItemKind::Field
                && other.parent == Some(ty)
                && !other.holds_nothing
                && at != field.index()
        })
        .peekable();
    let visibility = getter_item.visibility;
    let plain = others.peek().is_some() && others.all(|(_, other)| other.visibility >= visibility);
    let only_getters = model.items.iter().enumerate().all(|(at, method)| {
        method.kind != ItemKind::Function
            || method.parent != Some(ty)
            || matches!(
                model.wrappers.get(&ItemId::new(at)),
                Some(Wrapper::Accessor { .. })
            )
    });
    (plain && only_getters).then(|| {
        format!(
            "`{}`'s other fields are as visible and it has no method but getters: a record",
            last_name(model, ty)
        )
    })
}

/// The items whose definitions hold each reference to `items`, by item.
fn holders(model: &Model, items: &HashSet<ItemId>) -> HashMap<ItemId, Vec<(ItemId, bool)>> {
    let mut found: HashMap<ItemId, Vec<(ItemId, bool)>> = HashMap::new();
    for reference in &model.references {
        if items.contains(&reference.item)
            && let Some(holder) = reference.holder
        {
            found
                .entry(reference.item)
                .or_default()
                .push((holder, reference.in_signature));
        }
    }
    found
}

fn one_use_args(model: &Model, graph: &Graph, scope: Option<NodeId>) -> Vec<Diagnostic> {
    // By struct, its fields; a type with a method or trait `impl` of its
    // own is no bundle of arguments.
    let mut fields: BTreeMap<ItemId, Vec<ItemId>> = BTreeMap::new();
    let mut worked: HashSet<ItemId> = HashSet::new();
    for (index, item) in model.items.iter().enumerate() {
        let Some(parent) = item.parent else {
            continue;
        };
        match item.kind {
            ItemKind::Field if model.items[parent.index()].kind == ItemKind::Type => {
                fields.entry(parent).or_default().push(ItemId::new(index));
            }
            ItemKind::Function | ItemKind::Type | ItemKind::Constant => {
                worked.insert(parent);
            }
            _ => {}
        }
    }
    let mentioned: HashSet<ItemId> = model.mentions.iter().map(|m| m.item).collect();
    let structs: HashSet<ItemId> = fields
        .keys()
        .copied()
        .filter(|ty| !worked.contains(ty) && !mentioned.contains(ty))
        .filter(|ty| !model.in_tests(&model.items[ty.index()]))
        // A record read from a file or a command line is no parameter list.
        .filter(|&ty| !model.derives(ty, &["Deserialize", "Parser", "Args"]))
        .collect();
    let all_fields: HashSet<ItemId> = structs.iter().flat_map(|ty| fields[ty].clone()).collect();
    let uses = holders(model, &structs);
    let field_uses = holders(model, &all_fields);
    let calls = holders(
        model,
        &model
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.kind == ItemKind::Function)
            .map(|(index, _)| ItemId::new(index))
            .collect(),
    );
    let mut diagnostics = Vec::new();
    for &ty in fields.keys().filter(|ty| structs.contains(ty)) {
        let own = |holder: ItemId| holder == ty || model.items[holder.index()].parent == Some(ty);
        let by: Vec<(ItemId, bool)> = uses
            .get(&ty)
            .into_iter()
            .flatten()
            .copied()
            .filter(|&(holder, _)| !own(holder))
            .collect();
        // The callee names it in its signature, the caller once in its body.
        let callees: HashSet<ItemId> = by.iter().filter(|u| u.1).map(|u| u.0).collect();
        let builders: Vec<ItemId> = by
            .iter()
            .filter(|u| !callees.contains(&u.0))
            .map(|u| u.0)
            .collect();
        let ([callee], [builder]) = (
            &callees.iter().copied().collect::<Vec<_>>()[..],
            &builders[..],
        ) else {
            continue;
        };
        let functions = [callee, builder]
            .iter()
            .all(|f| model.items[f.index()].kind == ItemKind::Function);
        let called = calls
            .get(callee)
            .is_some_and(|callers| callers.iter().any(|&(caller, _)| caller == *builder));
        let read_there = fields[&ty].iter().all(|field| {
            field_uses
                .get(field)
                .into_iter()
                .flatten()
                .all(|&(holder, _)| holder == *callee || holder == *builder)
                && !mentioned.contains(field)
        });
        // A struct literal names its fields; a value parsed or defaulted
        // into the type is none built for the call.
        let built = fields[&ty].iter().any(|field| {
            field_uses
                .get(field)
                .is_some_and(|uses| uses.iter().any(|&(holder, _)| holder == *builder))
        });
        if !functions || !called || !read_there || !built {
            continue;
        }
        let (key, home) = key(model, graph, ty);
        if !in_scope(&model.tree, scope, home) {
            continue;
        }
        let name = without_generics(&model.items[ty.index()].name);
        let callee_name = without_generics(&model.items[callee.index()].name);
        let builder_name = without_generics(&model.items[builder.index()].name);
        diagnostics.push(Diagnostic {
            lint: Lint::Encapsulation(EncapsulationLint::OneUseArgs),
            key: key.clone(),
            message: format!(
                "`{name}` is built once, in `{builder_name}`, for `{callee_name}`, which alone reads it"
            ),
            location: item_location(model, ty),
            notes: vec![
                "a parameter list moved into a type: what do the two functions do together?"
                    .to_owned(),
            ],
            picture: Picture::Arrow {
                from: builder_name,
                to: callee_name,
                detail: format!("passes its arguments as one `{name}`"),
            },
        });
    }
    diagnostics
}

fn inner_splits(model: &Model, graph: &Graph, scope: Option<NodeId>) -> Vec<Diagnostic> {
    // By function, the items whose code names it; a recursive function, an
    // inner loop its caller starts, is no split.
    let mut callers: HashMap<ItemId, Vec<ItemId>> = HashMap::new();
    let mut named: HashSet<ItemId> = model.mentions.iter().map(|m| m.item).collect();
    // By function, how many functions its body names.
    let mut calls: HashMap<ItemId, HashSet<ItemId>> = HashMap::new();
    for reference in &model.references {
        match reference.holder {
            Some(holder) if holder == reference.item => {
                named.insert(reference.item);
            }
            Some(holder) => {
                callers.entry(reference.item).or_default().push(holder);
                if model.items[reference.item.index()].kind == ItemKind::Function {
                    calls.entry(holder).or_default().insert(reference.item);
                }
            }
            None => {
                named.insert(reference.item);
            }
        }
    }
    // A body that only passes on, as compiled as its callee is, and both as
    // safe: an `#[inline(always)]`, `#[cold]`, `#[target_feature]` or
    // `unsafe` split is on purpose.
    let plain = |id: ItemId| {
        let item = &model.items[id.index()];
        item.kind == ItemKind::Function
            && !item.trait_impl
            && !item
                .parent
                .is_some_and(|parent| model.traits.contains(&parent))
            && !item.extern_abi
            && !item.is_unsafe
            && !model.in_tests(item)
            && !model.in_example(item.file)
            && item.metrics.is_some_and(|metrics| {
                let codegen = metrics.codegen;
                !(codegen.cold || codegen.inline_always || codegen.target_feature)
            })
    };
    let mut found: Vec<(ItemId, ItemId)> = callers
        .iter()
        .filter_map(|(&callee, by)| match by[..] {
            [caller] if !named.contains(&callee) => Some((callee, caller)),
            _ => None,
        })
        .filter(|&(callee, caller)| {
            let item = &model.items[callee.index()];
            // A generic caller that converts, `p.as_ref()`, keeps one copy
            // of the callee for every type: no `passes_on`.
            let passes_on = model.items[caller.index()]
                .metrics
                .is_some_and(|metrics| metrics.passes_on);
            let only_call = calls.get(&caller).is_some_and(|called| called.len() == 1);
            // A method of another type, or a function of another module,
            // is no half of the caller: a field's method it calls on.
            let caller_item = &model.items[caller.index()];
            let beside = item.parent == caller_item.parent && item.owner == caller_item.owner;
            item.visibility == Visibility::Private
                && plain(callee)
                && plain(caller)
                && beside
                && passes_on
                && only_call
        })
        .collect();
    found.sort();
    let mut diagnostics = Vec::new();
    for (callee, caller) in found {
        let (key, home) = key(model, graph, callee);
        if !in_scope(&model.tree, scope, home) {
            continue;
        }
        let name = without_generics(&model.items[callee.index()].name);
        let caller_name = without_generics(&model.items[caller.index()].name);
        diagnostics.push(Diagnostic {
            lint: Lint::Encapsulation(EncapsulationLint::InnerSplit),
            key,
            message: format!(
                "`{name}` is called only by `{caller_name}`, which does little more than call it"
            ),
            location: item_location(model, callee),
            notes: vec![format!(
                "one function split in two: merge `{name}` into `{caller_name}`"
            )],
            picture: Picture::Arrow {
                from: caller_name,
                to: name,
                detail: "its only caller passes the call on".to_owned(),
            },
        });
    }
    diagnostics
}

fn redundant_reexports(model: &Model, scope: Option<NodeId>) -> Vec<Diagnostic> {
    let tree = &model.tree;
    // By module, whether it is declared `pub mod`.
    let declared: HashMap<NodeId, Visibility> = model
        .items
        .iter()
        .filter_map(|item| Some((item.module?, item.visibility)))
        .collect();
    // Whether every module from `module` up to its crate is `pub`.
    let public = |module: NodeId| {
        let package = tree.package_of(module);
        tree.ancestors(module)
            .take_while(|&at| Some(at) != package)
            .all(|at| declared.get(&at) == Some(&Visibility::Pub))
    };
    // By item and the crate that re-exports it: another crate's item is on
    // no path of this one's, whose users would need that crate too.
    let mut by_item: BTreeMap<(ItemId, Option<NodeId>), Vec<&crate::model::Reexport>> =
        BTreeMap::new();
    for reexport in &model.reexports {
        let file = &model.files[reexport.file.index()];
        if !file.test && !tree.is_test(reexport.module) && public(reexport.module) {
            let crate_of = tree.package_of(reexport.module);
            by_item
                .entry((reexport.item, crate_of))
                .or_default()
                .push(reexport);
        }
    }
    let mut diagnostics = Vec::new();
    for ((item, crate_of), mut reexports) in by_item {
        let defined = &model.items[item.index()];
        let package = tree.package_of(defined.owner);
        let own = package == crate_of;
        let at_home = own
            && defined.visibility == Visibility::Pub
            && defined.parent.is_none()
            && public(defined.owner);
        // Whether code in `from` surely can name the item where it is
        // defined: each module on the way down is `pub`, or declared in a
        // module `from` is in, as a private one is visible. Another crate's
        // item is `pub` where any `pub use` names it.
        let direct = |from: NodeId| {
            !own || tree
                .ancestors(defined.owner)
                .take_while(|&at| Some(at) != package && !tree.is_within(from, at))
                .all(|at| {
                    declared.get(&at) == Some(&Visibility::Pub)
                        || tree
                            .parent(at)
                            .is_some_and(|parent| tree.is_within(from, parent))
                })
        };
        reexports.sort_by_key(|r| (tree.node(r.module).depth, r.file, r.line));
        reexports.dedup_by_key(|r| r.module);
        let name = without_generics(&defined.name);
        let home_path = format!("{}::{name}", tree.display(defined.owner));
        // The path that stays: where it is defined, or the shallowest
        // `pub use` that names it there, which those that cannot go
        // through (E0364 keeps it `pub`).
        let (kept, kept_path, redundant): (Option<Location>, String, Vec<_>) = if at_home {
            (
                item_location(model, item),
                home_path.clone(),
                reexports.iter().collect(),
            )
        } else if let Some(first) = reexports.iter().position(|r| direct(r.module)) {
            let at = Location {
                file: model.files[reexports[first].file.index()].path.clone(),
                line: reexports[first].line,
            };
            let path = format!("{}::{name}", tree.display(reexports[first].module));
            let rest = reexports.iter().enumerate().filter(|&(at, _)| at != first);
            (Some(at), path, rest.map(|(_, r)| r).collect())
        } else {
            continue;
        };
        for reexport in redundant {
            if !in_scope(tree, scope, reexport.module) {
                continue;
            }
            let key = format!("{}::{name}", tree.display(reexport.module));
            let mut notes = vec![format!("`{home_path}` is already public as `{kept_path}`")];
            notes.extend(
                kept.as_ref()
                    .map(|at| format!("{}: its public path", clickable(at))),
            );
            diagnostics.push(Diagnostic {
                lint: Lint::Encapsulation(EncapsulationLint::RedundantReexport),
                key: key.clone(),
                message: format!("`pub use` of `{name}` adds a second public path to it"),
                location: Some(Location {
                    file: model.files[reexport.file.index()].path.clone(),
                    line: reexport.line,
                }),
                notes,
                picture: Picture::Arrow {
                    from: key,
                    to: kept_path.clone(),
                    detail: "re-exports what is already public".to_owned(),
                },
            });
        }
    }
    diagnostics
}
