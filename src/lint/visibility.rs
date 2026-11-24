//! `unused-pub` and `over-public`: workspace-wide visibility checks for private
//! (`publish = false`) library crates. `rustc`'s `dead_code` and
//! `unreachable_pub` compile one crate at a time and assume every reachable
//! `pub` item in a library has external callers; across a closed workspace,
//! `unused-pub` finds `pub` items with no non-test callers anywhere, and
//! `over-public` finds `pub` items that are only ever used inside their own
//! library target and can be narrowed to `pub(crate)`.

use std::collections::{BTreeMap, HashSet};

use super::{
    Diagnostic, HELD_IN_MIND, Lint, Picture, counted, in_scope, item_location, without_generics,
};
use crate::graph::{Graph, ThingId};
use crate::model::{ItemId, ItemKind, Model, NodeId, Visibility};
use crate::workspace::TargetKind;

#[derive(Default)]
struct Usage {
    prod_used: bool,
    test_used: bool,
    /// Used by another package, or by a non-library target (`bin`, `example`,
    /// `test`, `bench`) of the same package, which Cargo compiles as an
    /// external crate that cannot see `pub(crate)`.
    external_to_lib: bool,
    /// Used from outside the item's own home module (and outside its submodule
    /// subtree for non-root modules) within the same target.
    outside_home: bool,
    internal_modules: BTreeMap<NodeId, usize>,
}

pub fn check(model: &Model, graph: &Graph, scope: Option<NodeId>) -> Vec<Diagnostic> {
    let tree = &model.tree;
    let mut item_usage: Vec<Usage> = (0..model.items.len()).map(|_| Usage::default()).collect();
    let mut thing_usage: Vec<Usage> = (0..graph.things.len()).map(|_| Usage::default()).collect();

    for reference in &model.references {
        let target = reference.item;
        let target_item = &model.items[target.index()];
        let target_pkg = tree.package_of(target_item.owner);
        let target_file_target = target_item.file.and_then(|f| model.files[f.index()].target);
        let ref_file = &model.files[reference.file.index()];
        let ref_pkg = tree.package_of(ref_file.module);
        let external = ref_pkg != target_pkg || ref_file.target != Some(TargetKind::Library);
        let same_target = ref_pkg == target_pkg && ref_file.target == target_file_target;

        // Item-level usage (ignore direct self-recursion).
        if reference.holder != Some(target) {
            let item_home = home_of(model, graph, target);
            let outside =
                !same_target || is_outside_home(tree, reference.from, item_home, target_pkg);
            record(
                &mut item_usage[target.index()],
                reference.test,
                external,
                outside,
                reference.from,
            );
        }

        // Thing-level usage (for types: any external reference to the type or
        // any of its fields, variants, or methods counts as using the type).
        if let Some(target_thing) = graph.thing_of[target.index()] {
            let holder_thing = reference
                .holder
                .and_then(|holder| graph.thing_of[holder.index()]);
            if holder_thing != Some(target_thing) {
                let thing_home = graph.things[target_thing.index()].home;
                let outside =
                    !same_target || is_outside_home(tree, reference.from, thing_home, target_pkg);
                record(
                    &mut thing_usage[target_thing.index()],
                    reference.test,
                    external,
                    outside,
                    reference.from,
                );
            }
        }
    }

    let candidates: Vec<(ItemId, NodeId)> = (0..model.items.len())
        .filter_map(|index| {
            let id = ItemId::new(index);
            let pkg = qualifying_package(model, id)?;
            Some((id, pkg))
        })
        .collect();

    // First pass: find types that are themselves `unused-pub` or `over-public`
    // so we don't also emit redundant findings on every one of their methods.
    let mut suppressed_parents: HashSet<ItemId> = HashSet::new();
    let mut diagnostics = Vec::new();
    for &(id, pkg) in &candidates {
        if model.items[id.index()].kind != ItemKind::Type {
            continue;
        }
        let Some(usage) = usage_for(model, graph, id, &item_usage, &thing_usage) else {
            continue;
        };
        if let Some(diag) = classify(model, graph, id, pkg, usage, scope) {
            suppressed_parents.insert(id);
            diagnostics.push(diag);
        } else if is_over_scoped(model, id, usage) {
            suppressed_parents.insert(id);
        }
    }

    // Second pass: functions and constants.
    for &(id, pkg) in &candidates {
        let item = &model.items[id.index()];
        if item.kind == ItemKind::Type {
            continue;
        }
        if item
            .parent
            .is_some_and(|parent| suppressed_parents.contains(&parent))
        {
            continue;
        }
        let Some(usage) = usage_for(model, graph, id, &item_usage, &thing_usage) else {
            continue;
        };
        if let Some(diag) = classify(model, graph, id, pkg, usage, scope) {
            diagnostics.push(diag);
        }
    }

    diagnostics
}

fn is_outside_home(
    tree: &crate::model::Tree,
    from: NodeId,
    home: NodeId,
    pkg: Option<NodeId>,
) -> bool {
    if from == home {
        false
    } else if Some(home) == pkg {
        true
    } else {
        !tree.is_within(from, home)
    }
}

fn is_over_scoped(model: &Model, id: ItemId, usage: &Usage) -> bool {
    match model.items[id.index()].visibility {
        Visibility::Pub => !usage.prod_used || !usage.external_to_lib,
        Visibility::Restricted => usage.prod_used && !usage.outside_home,
        Visibility::Private => false,
    }
}

fn classify(
    model: &Model,
    graph: &Graph,
    id: ItemId,
    pkg: NodeId,
    usage: &Usage,
    scope: Option<NodeId>,
) -> Option<Diagnostic> {
    if !is_over_scoped(model, id, usage) {
        return None;
    }
    if !in_scope(&model.tree, scope, home_of(model, graph, id)) {
        return None;
    }
    Some(build_diagnostic(model, graph, id, pkg, usage))
}

fn record(usage: &mut Usage, test: bool, external: bool, outside_home: bool, from: NodeId) {
    if test {
        usage.test_used = true;
    } else {
        usage.prod_used = true;
        if !external {
            *usage.internal_modules.entry(from).or_default() += 1;
        }
    }
    if external {
        usage.external_to_lib = true;
    }
    if outside_home {
        usage.outside_home = true;
    }
}

fn qualifying_package(model: &Model, id: ItemId) -> Option<NodeId> {
    let tree = &model.tree;
    let item = &model.items[id.index()];
    if item.visibility == Visibility::Private || item.trait_impl || item.extern_abi {
        return None;
    }
    if !matches!(
        item.kind,
        ItemKind::Type | ItemKind::Function | ItemKind::Constant
    ) {
        return None;
    }
    let file = item.file?;
    let file_info = &model.files[file.index()];
    if file_info.test || tree.is_test(item.owner) {
        return None;
    }
    if item.visibility == Visibility::Pub && file_info.target != Some(TargetKind::Library) {
        return None;
    }
    let pkg = tree.package_of(item.owner)?;
    if tree.node(pkg).publish {
        return None;
    }
    if let Some(parent) = item.parent {
        let parent_item = &model.items[parent.index()];
        if parent_item.kind != ItemKind::Type || parent_item.visibility < item.visibility {
            return None;
        }
    }
    Some(pkg)
}

fn usage_for<'a>(
    model: &Model,
    graph: &Graph,
    id: ItemId,
    item_usage: &'a [Usage],
    thing_usage: &'a [Usage],
) -> Option<&'a Usage> {
    let item = &model.items[id.index()];
    if item.kind == ItemKind::Type {
        let thing: ThingId = graph.thing_of[id.index()]?;
        Some(&thing_usage[thing.index()])
    } else {
        Some(&item_usage[id.index()])
    }
}

fn home_of(model: &Model, graph: &Graph, id: ItemId) -> NodeId {
    graph.thing_of[id.index()].map_or(model.items[id.index()].owner, |thing| {
        graph.things[thing.index()].home
    })
}

fn build_diagnostic(
    model: &Model,
    graph: &Graph,
    id: ItemId,
    pkg: NodeId,
    usage: &Usage,
) -> Diagnostic {
    let tree = &model.tree;
    let item = &model.items[id.index()];
    let name = without_generics(&item.name);
    let home = home_of(model, graph, id);
    let key = format!("{}::{name}", tree.display(home));
    let location = item_location(model, id);

    if item.visibility == Visibility::Restricted {
        let home_name = tree.display(home);
        Diagnostic {
            lint: Lint::OverPublic,
            key: key.clone(),
            message: format!(
                "`{name}` is `pub(crate)`/`pub(super)` but only used inside `{home_name}`"
            ),
            location,
            notes: vec![format!("narrow `{name}` to private")],
            picture: Picture::Arrow {
                from: key,
                to: home_name,
                detail: "only referenced inside its own module subtree (narrow to private)"
                    .to_owned(),
            },
        }
    } else if !usage.prod_used {
        let reason = if usage.test_used {
            "only used in tests"
        } else {
            "never referenced in the workspace"
        };
        Diagnostic {
            lint: Lint::UnusedPub,
            key: key.clone(),
            message: format!("`{name}` is `pub` but has no non-test callers ({reason})"),
            location,
            notes: vec![format!("defined in private crate `{}`", tree.display(pkg))],
            picture: Picture::Arrow {
                from: key,
                to: "(no non-test callers)".to_owned(),
                detail: if usage.test_used {
                    "gate behind #[cfg(test)] or move to test support".to_owned()
                } else {
                    "remove dead item or narrow visibility".to_owned()
                },
            },
        }
    } else {
        let mut modules: Vec<(usize, String)> = usage
            .internal_modules
            .iter()
            .map(|(&m, &count)| (count, tree.display(m)))
            .collect();
        modules.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        let total_modules = modules.len();
        let total_calls: usize = modules.iter().map(|(c, _)| *c).sum();
        if modules.len() > HELD_IN_MIND {
            let more = modules.split_off(HELD_IN_MIND);
            let count = more.iter().map(|(c, _)| c).sum();
            modules.push((count, format!("in {}", counted(more.len(), "more module"))));
        }
        let pkg_name = tree.display(pkg);
        Diagnostic {
            lint: Lint::OverPublic,
            key: key.clone(),
            message: format!(
                "`{name}` is `pub` but only used inside `{pkg_name}` ({} in {})",
                counted(total_calls, "use"),
                counted(total_modules, "module")
            ),
            location,
            notes: vec![format!(
                "no external crate, binary, example, or integration test references `{name}`"
            )],
            picture: Picture::Spread {
                owner: key,
                field: "pub".to_owned(),
                modules,
                others: vec!["narrow to pub(crate) or private".to_owned()],
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{NodeKind, tests::Sample};

    fn private_sample() -> Sample {
        let mut sample = Sample::new();
        let p = sample.module("");
        sample.model.tree.node_mut(p).publish = false;
        sample
    }

    fn report(sample: &Sample) -> Vec<(Lint, String)> {
        let graph = Graph::new(&sample.model);
        check(&sample.model, &graph, None)
            .into_iter()
            .map(|d| (d.lint, d.key))
            .collect()
    }

    #[test]
    fn published_crates_are_not_checked() {
        let mut sample = Sample::new();
        let m = sample.module("api");
        let f = sample.item(m, "unused", ItemKind::Function);
        sample.model.items[f.index()].visibility = Visibility::Pub;
        assert_eq!(report(&sample), Vec::<(Lint, String)>::new());
    }

    #[test]
    fn unused_pub_and_over_public_fire_in_private_crates() {
        let mut sample = private_sample();
        let (api, caller) = (sample.module("api"), sample.module("caller"));
        let dead = sample.item(api, "dead", ItemKind::Function);
        let internal = sample.item(api, "internal", ItemKind::Function);
        let helper = sample.item(caller, "run", ItemKind::Function);
        sample.model.items[dead.index()].visibility = Visibility::Pub;
        sample.model.items[internal.index()].visibility = Visibility::Pub;
        sample.refer(helper, internal);

        assert_eq!(
            report(&sample),
            vec![
                (Lint::UnusedPub, "p::api::dead".into()),
                (Lint::OverPublic, "p::api::internal".into()),
            ]
        );
    }

    #[test]
    fn cross_crate_or_binary_callers_justify_pub() {
        let mut sample = private_sample();
        let api = sample.module("api");
        let used_by_other = sample.item(api, "shared", ItemKind::Function);
        let used_by_bin = sample.item(api, "entry", ItemKind::Function);
        sample.model.items[used_by_other.index()].visibility = Visibility::Pub;
        sample.model.items[used_by_bin.index()].visibility = Visibility::Pub;

        // Another package calls `shared`.
        let root = sample.model.tree.root();
        let other_pkg = sample.model.tree.child(root, NodeKind::Package, "app");
        sample.file(other_pkg, "app/src/main.rs", 1);
        let main_fn = sample.item(other_pkg, "main", ItemKind::Function);
        sample.refer(main_fn, used_by_other);

        // A binary target inside the same package calls `entry`.
        let pkg = sample.module("");
        sample.file(pkg, "src/main.rs", 1);
        let bin_file = crate::model::FileId::new(sample.model.files.len() - 1);
        sample.model.files[bin_file.index()].target = Some(TargetKind::Binary);
        let bin_fn = sample.item(pkg, "bin_main", ItemKind::Function);
        sample.model.items[bin_fn.index()].file = Some(bin_file);
        sample.refer(bin_fn, used_by_bin);
        sample.model.references.last_mut().unwrap().file = bin_file;

        assert_eq!(report(&sample), Vec::<(Lint, String)>::new());
    }

    #[test]
    fn dead_type_suppresses_its_pub_methods() {
        let mut sample = private_sample();
        let api = sample.module("api");
        let ty = sample.item(api, "Ghost", ItemKind::Type);
        let method = sample.member(ty, api, "Ghost::boo", ItemKind::Function);
        sample.model.items[ty.index()].visibility = Visibility::Pub;
        sample.model.items[method.index()].visibility = Visibility::Pub;
        assert_eq!(
            report(&sample),
            vec![(Lint::UnusedPub, "p::api::Ghost".into())]
        );
    }
}
