//! `high-fan-out` and `feature-envy`: coupling concentration lints over the
//! workspace graph.
//! - `high-fan-out`: a module directly referencing items from more than 15
//!   distinct sibling or foreign workspace modules (excluding its own direct
//!   children and its own ancestors).
//! - `feature-envy`: a non-test inherent method on one type that uses 5 or more
//!   distinct members (fields, methods, variants) of a single foreign type from
//!   another module, more than twice as many as its own type/module, and
//!   strictly more than any other foreign type.

use std::collections::{BTreeMap, BTreeSet};

use super::{
    Diagnostic, EncapsulationLint, GraphLint, Lint, Picture, counted, full_name, in_scope,
    item_location, listed, module_location, without_generics,
};
use crate::graph::{Graph, ThingId};
use crate::model::{ItemId, ItemKind, Model, NodeId};

/// The most distinct sibling/foreign workspace modules one module may depend on.
const MOST_FAN_OUT: usize = 15;

/// Both fan-in and fan-out at or above this threshold mark a module as a `god-hub`.
const MIN_HUB_DEGREE: usize = 10;

/// The fewest distinct member items of a single foreign type that trigger
/// `feature-envy`.
const MIN_ENVY_ITEMS: usize = 5;

pub fn check(model: &Model, graph: &Graph, scope: Option<NodeId>) -> Vec<Diagnostic> {
    let mut diagnostics = check_fan_out(model, graph, scope);
    diagnostics.extend(check_feature_envy(model, graph, scope));
    diagnostics.extend(check_layers(model, scope));
    diagnostics.extend(check_unused_deps(model, scope));
    diagnostics
}

fn check_fan_out(model: &Model, graph: &Graph, scope: Option<NodeId>) -> Vec<Diagnostic> {
    let tree = &model.tree;
    let mut deps: BTreeMap<NodeId, BTreeSet<NodeId>> = BTreeMap::new();
    let mut fan_in: BTreeMap<NodeId, BTreeSet<NodeId>> = BTreeMap::new();
    for reference in model.references.iter().filter(|r| !r.test) {
        let item = &model.items[reference.item.index()];
        if item.kind == ItemKind::Module
            || tree.is_test(item.owner)
            || item.file.is_some_and(|file| model.files[file.index()].test)
        {
            continue;
        }
        let target_mod = graph.thing_of[reference.item.index()]
            .map_or(item.owner, |thing| graph.things[thing.index()].home);
        let from = reference.from;
        if from == target_mod
            || tree.parent(target_mod) == Some(from)
            || tree.is_within(from, target_mod)
        {
            continue;
        }
        deps.entry(from).or_default().insert(target_mod);
        if !tree.is_within(target_mod, from) {
            fan_in.entry(target_mod).or_default().insert(from);
        }
    }

    let mut diagnostics = Vec::new();
    for (module, targets) in &deps {
        if !in_scope(tree, scope, *module) {
            continue;
        }
        let name = tree.display(*module);
        if targets.len() > MOST_FAN_OUT {
            let mut target_names: Vec<String> = targets.iter().map(|&t| tree.display(t)).collect();
            target_names.sort();
            diagnostics.push(Diagnostic {
                lint: Lint::Graph(GraphLint::HighFanOut),
                key: name.clone(),
                message: format!(
                    "`{name}` has high fan-out: it uses {}, more than {MOST_FAN_OUT}",
                    counted(targets.len(), "module")
                ),
                location: module_location(model, *module),
                notes: vec![format!("depends on: {}", listed(&target_names))],
                picture: Picture::Bar {
                    label: "uses".to_owned(),
                    length: targets.len(),
                    most: MOST_FAN_OUT,
                },
            });
        }
        if targets.len() >= MIN_HUB_DEGREE
            && let Some(callers) = fan_in.get(module)
            && callers.len() >= MIN_HUB_DEGREE
        {
            let mut caller_names: Vec<String> = callers.iter().map(|&c| tree.display(c)).collect();
            caller_names.sort();
            let mut target_names: Vec<String> = targets.iter().map(|&t| tree.display(t)).collect();
            target_names.sort();
            diagnostics.push(Diagnostic {
                lint: Lint::Graph(GraphLint::GodHub),
                key: name.clone(),
                message: format!(
                    "`{name}` is a god hub: {} depend on it and it depends on {}",
                    counted(callers.len(), "module"),
                    counted(targets.len(), "module"),
                ),
                location: module_location(model, *module),
                notes: vec![
                    format!("used by: {}", listed(&caller_names)),
                    format!("depends on: {}", listed(&target_names)),
                ],
                picture: Picture::Bar {
                    label: "degree".to_owned(),
                    length: callers.len() + targets.len(),
                    most: MIN_HUB_DEGREE * 2 - 1,
                },
            });
        }
    }
    diagnostics
}

#[derive(Default)]
struct FunctionUses {
    own: BTreeSet<ItemId>,
    foreign: BTreeMap<ThingId, BTreeSet<ItemId>>,
}

fn check_feature_envy(model: &Model, graph: &Graph, scope: Option<NodeId>) -> Vec<Diagnostic> {
    let tree = &model.tree;
    let mut by_fn: BTreeMap<ItemId, FunctionUses> = BTreeMap::new();

    for usage in &graph.uses {
        let reference = &model.references[usage.reference];
        let Some(holder) = reference.holder else {
            continue;
        };
        let fn_item = &model.items[holder.index()];
        if fn_item.kind != ItemKind::Function
            || fn_item.trait_impl
            || fn_item.parent.is_none()
            || model.wrappers.contains_key(&holder)
        {
            continue;
        }
        let Some(used_thing) = graph.thing_of[usage.item.index()] else {
            continue;
        };
        let fn_thing = graph.thing_of[holder.index()];
        let fn_home = fn_thing.map_or(fn_item.owner, |t| graph.things[t.index()].home);
        let used_root = graph.things[used_thing.index()].item;
        let used_home = graph.things[used_thing.index()].home;

        let entry = by_fn.entry(holder).or_default();
        if Some(used_thing) == fn_thing || used_home == fn_home {
            entry.own.insert(usage.item);
            continue;
        }
        if model.items[used_root.index()].kind == ItemKind::Type
            && usage.item != used_root
            && matches!(
                model.items[usage.item.index()].kind,
                ItemKind::Function | ItemKind::Field
            )
            && (model.items[usage.item.index()].kind == ItemKind::Function || !usage.copies)
        {
            entry
                .foreign
                .entry(used_thing)
                .or_default()
                .insert(usage.item);
        }
    }

    let mut diagnostics = Vec::new();
    for (holder, uses) in by_fn {
        let own_items = &uses.own;
        let mut ranked: Vec<(usize, ThingId, &BTreeSet<ItemId>)> = uses
            .foreign
            .iter()
            .map(|(&ty, items)| (items.len(), ty, items))
            .collect();
        ranked.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        let Some(&(top_count, top_type, top_items)) = ranked.first() else {
            continue;
        };
        let second_count = ranked.get(1).map_or(0, |&(c, ..)| c);
        if top_count < MIN_ENVY_ITEMS
            || top_count <= own_items.len() * 2
            || top_count <= second_count
        {
            continue;
        }
        let fn_item = &model.items[holder.index()];
        let fn_home =
            graph.thing_of[holder.index()].map_or(fn_item.owner, |t| graph.things[t.index()].home);
        if !in_scope(tree, scope, fn_home) {
            continue;
        }
        let fn_name = without_generics(&fn_item.name);
        let key = format!("{}::{fn_name}", tree.display(fn_home));
        let target_full = full_name(model, graph, top_type);
        let target_name =
            without_generics(&model.items[graph.things[top_type.index()].item.index()].name);
        let mut member_names: Vec<String> = top_items
            .iter()
            .map(|&m| without_generics(&model.items[m.index()].name))
            .collect();
        member_names.sort();
        diagnostics.push(Diagnostic {
            lint: Lint::Encapsulation(EncapsulationLint::FeatureEnvy),
            key: key.clone(),
            message: format!(
                "`{fn_name}` shows feature envy toward `{target_name}`: uses {} of it vs {} own",
                counted(top_count, "item"),
                own_items.len()
            ),
            location: item_location(model, holder),
            notes: vec![format!(
                "uses of `{target_name}`: {}",
                listed(&member_names)
            )],
            picture: Picture::Arrow {
                from: key,
                to: target_full,
                detail: format!(
                    "uses {} distinct items of `{target_name}` vs {} own",
                    top_count,
                    own_items.len()
                ),
            },
        });
    }
    diagnostics
}

fn check_layers(model: &Model, scope: Option<NodeId>) -> Vec<Diagnostic> {
    if model.layers.is_empty() {
        return Vec::new();
    }
    let tree = &model.tree;
    let mut layer_of: BTreeMap<&str, usize> = BTreeMap::new();
    for (index, members) in model.layers.iter().enumerate() {
        for name in members {
            layer_of.insert(name.as_str(), index);
        }
    }
    let mut diagnostics = Vec::new();
    for dep in &model.crate_deps {
        if !in_scope(tree, scope, dep.package) {
            continue;
        }
        let pkg_crate = tree.node(dep.package).name.as_str();
        let Some(&from_layer) = layer_of
            .get(dep.package_name.as_str())
            .or_else(|| layer_of.get(pkg_crate))
        else {
            continue;
        };
        let Some(&to_layer) = layer_of
            .get(dep.dep_name.as_str())
            .or_else(|| layer_of.get(dep.dep_crate.as_str()))
        else {
            continue;
        };
        if to_layer >= from_layer {
            let key = format!("{} -> {}", dep.package_name, dep.dep_name);
            let relation = if to_layer == from_layer {
                format!("same layer ({from_layer})")
            } else {
                format!("higher layer ({to_layer} > {from_layer})")
            };
            diagnostics.push(Diagnostic {
                lint: Lint::Graph(GraphLint::LayerViolation),
                key,
                message: format!(
                    "`{}` (layer {from_layer}) depends on `{}` in {relation}",
                    dep.package_name, dep.dep_name
                ),
                location: module_location(model, dep.package),
                notes: vec![
                    "dependencies must flow strictly downward to earlier layers in `deslop-please.toml`"
                        .to_owned(),
                ],
                picture: Picture::Arrow {
                    from: format!("{} (L{from_layer})", dep.package_name),
                    to: format!("{} (L{to_layer})", dep.dep_name),
                    detail: format!("violates layer boundary ({relation})"),
                },
            });
        }
    }
    diagnostics
}

fn check_unused_deps(model: &Model, scope: Option<NodeId>) -> Vec<Diagnostic> {
    let tree = &model.tree;
    let mut diagnostics = Vec::new();
    for dep in &model.crate_deps {
        if dep.used || dep.optional || !in_scope(tree, scope, dep.package) {
            continue;
        }
        let key = format!("{}::{}", dep.package_name, dep.dep_name);
        diagnostics.push(Diagnostic {
            lint: Lint::Graph(GraphLint::UnusedDep),
            key,
            message: format!(
                "`{}` declares normal dependency `{}` but never references it in non-test code",
                dep.package_name, dep.dep_name
            ),
            location: module_location(model, dep.package),
            notes: vec![
                "remove from `[dependencies]` or move to `[dev-dependencies]` if only used by tests"
                    .to_owned(),
            ],
            picture: Picture::Arrow {
                from: dep.package_name.clone(),
                to: dep.dep_name.clone(),
                detail: "declared in [dependencies] with no non-test reference".to_owned(),
            },
        });
    }
    diagnostics
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CrateDep, tests::Sample};

    fn report(sample: &Sample) -> Vec<(Lint, String)> {
        let graph = Graph::new(&sample.model);
        check(&sample.model, &graph, None)
            .into_iter()
            .map(|d| (d.lint, d.key))
            .collect()
    }

    #[test]
    fn high_fan_out_counts_distinct_sibling_and_foreign_modules() {
        let mut sample = Sample::new();
        let hub = sample.module("hub");
        let run = sample.item(hub, "run", ItemKind::Function);
        for i in 0..MOST_FAN_OUT {
            let dep = sample.module(&format!("dep{i}"));
            let item = sample.item(dep, &format!("T{i}"), ItemKind::Type);
            sample.refer(run, item);
        }
        // Direct child module does not count against fan-out.
        let child = sample.module("hub::sub");
        let child_item = sample.item(child, "Sub", ItemKind::Type);
        sample.refer(run, child_item);
        assert_eq!(report(&sample), Vec::<(Lint, String)>::new());

        let extra = sample.module("dep_extra");
        let extra_item = sample.item(extra, "Extra", ItemKind::Type);
        sample.refer(run, extra_item);
        assert_eq!(
            report(&sample),
            vec![(Lint::Graph(GraphLint::HighFanOut), "p::hub".into())]
        );
    }

    #[test]
    fn feature_envy_flags_methods_reaching_deeply_into_one_foreign_type() {
        let mut sample = Sample::new();
        let (a, b) = (sample.module("a"), sample.module("b"));
        let owner = sample.item(a, "Owner", ItemKind::Type);
        let foreign = sample.item(b, "Target", ItemKind::Type);
        let glue = sample.member(owner, a, "Owner::glue", ItemKind::Function);
        for i in 0..MIN_ENVY_ITEMS {
            let m = sample.member(foreign, b, &format!("Target::m{i}"), ItemKind::Function);
            sample.refer(glue, m);
        }
        assert_eq!(
            report(&sample),
            vec![(
                Lint::Encapsulation(EncapsulationLint::FeatureEnvy),
                "p::a::Owner::glue".into()
            )]
        );
    }

    #[test]
    fn god_hub_flags_modules_with_high_fan_in_and_high_fan_out() {
        let mut sample = Sample::new();
        let hub = sample.module("hub");
        let hub_fn = sample.item(hub, "run", ItemKind::Function);
        for i in 0..MIN_HUB_DEGREE {
            let dep = sample.module(&format!("dep{i}"));
            let item = sample.item(dep, &format!("D{i}"), ItemKind::Type);
            sample.refer(hub_fn, item);
            let caller = sample.module(&format!("caller{i}"));
            let cfn = sample.item(caller, "call", ItemKind::Function);
            sample.refer(cfn, hub_fn);
        }
        assert_eq!(
            report(&sample),
            vec![(Lint::Graph(GraphLint::GodHub), "p::hub".into())]
        );
    }

    #[test]
    fn layer_violation_and_unused_dep_fire_on_crate_deps() {
        let mut sample = Sample::new();
        let pkg = sample.module("");
        sample.model.layers = vec![vec!["core".into()], vec!["app".into()]];
        sample.model.crate_deps = vec![
            CrateDep {
                package: pkg,
                package_name: "core".into(),
                dep_name: "app".into(),
                dep_crate: "app".into(),
                optional: false,
                used: true,
            },
            CrateDep {
                package: pkg,
                package_name: "core".into(),
                dep_name: "serde".into(),
                dep_crate: "serde".into(),
                optional: false,
                used: false,
            },
        ];
        assert_eq!(
            report(&sample),
            vec![
                (Lint::Graph(GraphLint::LayerViolation), "core -> app".into()),
                (Lint::Graph(GraphLint::UnusedDep), "core::serde".into()),
            ]
        );
    }
}
