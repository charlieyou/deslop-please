//! `unused-pub` and `over-public`: workspace-wide visibility checks for private
//! (`publish = false`) library crates. `rustc`'s `dead_code` and
//! `unreachable_pub` compile one crate at a time and assume every reachable
//! `pub` item in a library has external callers; across a closed workspace,
//! `unused-pub` finds `pub` items with no non-test callers anywhere, and
//! `over-public` finds `pub` items that are only ever used inside their own
//! library target and can be narrowed to `pub(crate)`.

use std::collections::{BTreeMap, HashMap, HashSet};

use super::{
    Diagnostic, HELD_IN_MIND, Lint, Picture, SurfaceLint, counted, in_scope, item_location,
    module_location, without_generics,
};
use crate::graph::{Graph, ThingId};
use crate::model::{ItemId, ItemKind, Model, NodeId, Visibility};
use crate::workspace::TargetKind;

#[derive(Default)]
struct Usage {
    prod_used: bool,
    test_used: bool,
    /// Non-test usage by another package or non-library target (`bin`, `example`).
    prod_external_to_lib: bool,
    /// Test usage by another package or test/bench target (`tests/`, `benches/`).
    test_external_to_lib: bool,
    /// Non-test usage outside the item's own home module subtree within the same target.
    prod_outside_home: bool,
    /// Test usage outside the item's own home module subtree.
    test_outside_home: bool,
    internal_modules: BTreeMap<NodeId, usize>,
}

struct AccessSite {
    test: bool,
    external: bool,
    outside_home: bool,
    from: NodeId,
}

struct UsageTables {
    item_usage: Vec<Usage>,
    thing_usage: Vec<Usage>,
    mod_usage: HashMap<NodeId, Usage>,
    non_empty_mods: HashSet<NodeId>,
}

pub fn check(model: &Model, graph: &Graph, scope: Option<NodeId>) -> Vec<Diagnostic> {
    let tables = collect_usage(model, graph);
    let candidates: Vec<(ItemId, NodeId)> = (0..model.items.len())
        .filter_map(|index| {
            let id = ItemId::new(index);
            let pkg = qualifying_package(model, id)?;
            Some((id, pkg))
        })
        .collect();

    let mut diagnostics = check_modules(model, &candidates, &tables, scope);
    diagnostics.extend(check_items(model, graph, &candidates, &tables, scope));
    diagnostics
}

fn collect_usage(model: &Model, graph: &Graph) -> UsageTables {
    let tree = &model.tree;
    let mut item_usage: Vec<Usage> = (0..model.items.len()).map(|_| Usage::default()).collect();
    let mut thing_usage: Vec<Usage> = (0..graph.things.len()).map(|_| Usage::default()).collect();
    let mut mod_usage: HashMap<NodeId, Usage> = HashMap::new();
    let mut non_empty_mods: HashSet<NodeId> = HashSet::new();

    for item in &model.items {
        if item.kind != ItemKind::Module
            && !tree.is_test(item.owner)
            && item.file.is_none_or(|f| !model.files[f.index()].test)
            && let Some(pkg) = tree.package_of(item.owner)
        {
            let mut cur = Some(item.owner);
            while let Some(node) = cur {
                if node == pkg {
                    break;
                }
                non_empty_mods.insert(node);
                cur = tree.parent(node);
            }
        }
    }

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
            let outside_home =
                !same_target || is_outside_home(tree, reference.from, item_home, target_pkg);
            record(
                &mut item_usage[target.index()],
                AccessSite {
                    test: reference.test,
                    external,
                    outside_home,
                    from: reference.from,
                },
            );
            // Attribute usage up through containing modules in the same package.
            let mod_start = target_item.module.unwrap_or(item_home);
            if let Some(pkg) = target_pkg {
                let mut cur = Some(mod_start);
                while let Some(node) = cur {
                    if node == pkg {
                        break;
                    }
                    let mod_outside =
                        !same_target || is_outside_home(tree, reference.from, node, Some(pkg));
                    if mod_outside || external {
                        record(
                            mod_usage.entry(node).or_default(),
                            AccessSite {
                                test: reference.test,
                                external,
                                outside_home: mod_outside,
                                from: reference.from,
                            },
                        );
                    }
                    cur = tree.parent(node);
                }
            }
        }

        // Thing-level usage (for types: any external reference to the type or
        // any of its fields, variants, or methods counts as using the type).
        if let Some(target_thing) = graph.thing_of[target.index()] {
            let holder_thing = reference
                .holder
                .and_then(|holder| graph.thing_of[holder.index()]);
            if holder_thing != Some(target_thing) {
                let thing_home = graph.things[target_thing.index()].home;
                let outside_home =
                    !same_target || is_outside_home(tree, reference.from, thing_home, target_pkg);
                record(
                    &mut thing_usage[target_thing.index()],
                    AccessSite {
                        test: reference.test,
                        external,
                        outside_home,
                        from: reference.from,
                    },
                );
            }
        }
    }

    UsageTables {
        item_usage,
        thing_usage,
        mod_usage,
        non_empty_mods,
    }
}

fn check_modules(
    model: &Model,
    candidates: &[(ItemId, NodeId)],
    tables: &UsageTables,
    scope: Option<NodeId>,
) -> Vec<Diagnostic> {
    let tree = &model.tree;
    let mut mod_candidates: Vec<(u32, NodeId, ItemId, NodeId)> = candidates
        .iter()
        .filter_map(|&(id, pkg)| {
            let item = &model.items[id.index()];
            if item.kind != ItemKind::Module || item.visibility != Visibility::Pub {
                return None;
            }
            let module = item.module?;
            if module == pkg || !tables.non_empty_mods.contains(&module) {
                return None;
            }
            Some((tree.node(module).depth, module, id, pkg))
        })
        .collect();
    mod_candidates.sort_by_key(|&(depth, module, _, _)| (depth, module));
    mod_candidates.dedup_by_key(|&mut (_, module, _, _)| module);

    let mut suppressed_mods: HashSet<NodeId> = HashSet::new();
    let mut diagnostics = Vec::new();
    let empty_usage = Usage::default();
    for (_, module, id, pkg) in mod_candidates {
        if tree
            .ancestors(module)
            .any(|anc| suppressed_mods.contains(&anc))
        {
            suppressed_mods.insert(module);
            continue;
        }
        let usage = tables.mod_usage.get(&module).unwrap_or(&empty_usage);
        if is_over_scoped(model, id, usage) {
            suppressed_mods.insert(module);
            if in_scope(tree, scope, module) {
                diagnostics.push(build_module_diagnostic(model, module, pkg, usage));
            }
        }
    }
    diagnostics
}

fn check_items(
    model: &Model,
    graph: &Graph,
    candidates: &[(ItemId, NodeId)],
    tables: &UsageTables,
    scope: Option<NodeId>,
) -> Vec<Diagnostic> {
    let mut suppressed_parents: HashSet<ItemId> = HashSet::new();
    let mut diagnostics = Vec::new();
    for &(id, pkg) in candidates {
        if model.items[id.index()].kind != ItemKind::Type {
            continue;
        }
        let Some(usage) = usage_for(model, graph, id, &tables.item_usage, &tables.thing_usage)
        else {
            continue;
        };
        if let Some(diag) = classify(model, graph, id, pkg, usage, scope) {
            suppressed_parents.insert(id);
            diagnostics.push(diag);
        } else if is_over_scoped(model, id, usage) {
            suppressed_parents.insert(id);
        }
    }

    for &(id, pkg) in candidates {
        let item = &model.items[id.index()];
        if matches!(item.kind, ItemKind::Type | ItemKind::Module) {
            continue;
        }
        if item
            .parent
            .is_some_and(|parent| suppressed_parents.contains(&parent))
        {
            continue;
        }
        let Some(usage) = usage_for(model, graph, id, &tables.item_usage, &tables.thing_usage)
        else {
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
        Visibility::Pub => !usage.prod_used || !usage.prod_external_to_lib,
        Visibility::Restricted => {
            (!usage.prod_used && usage.test_used) || (usage.prod_used && !usage.prod_outside_home)
        }
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

fn record(usage: &mut Usage, site: AccessSite) {
    if site.test {
        usage.test_used = true;
        if site.external {
            usage.test_external_to_lib = true;
        }
        if site.outside_home {
            usage.test_outside_home = true;
        }
    } else {
        usage.prod_used = true;
        if site.external {
            usage.prod_external_to_lib = true;
        } else {
            *usage.internal_modules.entry(site.from).or_default() += 1;
        }
        if site.outside_home {
            usage.prod_outside_home = true;
        }
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
        ItemKind::Type
            | ItemKind::Function
            | ItemKind::Constant
            | ItemKind::Field
            | ItemKind::Module
    ) {
        return None;
    }
    if item.kind == ItemKind::Field {
        if item.holds_nothing {
            return None;
        }
        let parent = item.parent?;
        if model.serde_types.contains(&parent) {
            return None;
        }
    }
    if item.kind == ItemKind::Module {
        let module = item.module?;
        if tree.is_test(module) {
            return None;
        }
        if item.file.is_some_and(|f| {
            let fi = &model.files[f.index()];
            fi.test || fi.target != Some(TargetKind::Library)
        }) {
            return None;
        }
        let pkg = tree.package_of(module)?;
        if tree.node(pkg).publish {
            return None;
        }
        return Some(pkg);
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

fn build_module_diagnostic(
    model: &Model,
    module: NodeId,
    pkg: NodeId,
    usage: &Usage,
) -> Diagnostic {
    let tree = &model.tree;
    let key = tree.display(module);
    let name = &tree.node(module).name;
    let pkg_name = tree.display(pkg);
    let location = module_location(model, module);
    if !usage.prod_used && usage.test_used {
        Diagnostic {
            lint: Lint::Surface(SurfaceLint::TestOnlyPub),
            key: key.clone(),
            message: format!("`pub mod {name}` is only referenced by tests"),
            location,
            notes: vec![format!("defined in private crate `{pkg_name}`")],
            picture: Picture::Arrow {
                from: key,
                to: "(tests only)".to_owned(),
                detail: "gate behind #[cfg(test)] or narrow to `pub(crate) mod`".to_owned(),
            },
        }
    } else if !usage.prod_used {
        Diagnostic {
            lint: Lint::Surface(SurfaceLint::UnusedPub),
            key: key.clone(),
            message: format!("`pub mod {name}` has no non-test callers outside itself"),
            location,
            notes: vec![format!("defined in private crate `{pkg_name}`")],
            picture: Picture::Arrow {
                from: key,
                to: "(no callers)".to_owned(),
                detail: "narrow to private `mod` or remove dead module".to_owned(),
            },
        }
    } else if usage.test_external_to_lib {
        Diagnostic {
            lint: Lint::Surface(SurfaceLint::TestOnlyPub),
            key: key.clone(),
            message: format!(
                "`pub mod {name}` is `pub` only for external tests; all non-test callers are inside `{pkg_name}`"
            ),
            location,
            notes: vec!["narrow to `pub(crate) mod` or private `mod`".to_owned()],
            picture: Picture::Arrow {
                from: key,
                to: pkg_name,
                detail: "external callers are exclusively tests".to_owned(),
            },
        }
    } else {
        Diagnostic {
            lint: Lint::Surface(SurfaceLint::OverPublic),
            key: key.clone(),
            message: format!(
                "`pub mod {name}` has no external non-test callers; only used inside `{pkg_name}`"
            ),
            location,
            notes: vec!["narrow to `pub(crate) mod` or private `mod`".to_owned()],
            picture: Picture::Arrow {
                from: key,
                to: pkg_name,
                detail: "no external crate or binary references items in this module".to_owned(),
            },
        }
    }
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
        if !usage.prod_used && usage.test_used {
            Diagnostic {
                lint: Lint::Surface(SurfaceLint::TestOnlyPub),
                key: key.clone(),
                message: format!(
                    "`{name}` is `pub(crate)`/`pub(super)` but only referenced by tests"
                ),
                location,
                notes: vec!["gate behind `#[cfg(test)]` or move to test support".to_owned()],
                picture: Picture::Arrow {
                    from: key,
                    to: "(tests only)".to_owned(),
                    detail: "only referenced by test code".to_owned(),
                },
            }
        } else if usage.test_outside_home {
            Diagnostic {
                lint: Lint::Surface(SurfaceLint::TestOnlyPub),
                key: key.clone(),
                message: format!(
                    "`{name}` is `pub(crate)`/`pub(super)` only for tests outside `{home_name}`"
                ),
                location,
                notes: vec![format!(
                    "all non-test callers live inside `{home_name}`; move test hooks under `#[cfg(test)]`"
                )],
                picture: Picture::Arrow {
                    from: key,
                    to: home_name,
                    detail: "only widened outside home module for tests".to_owned(),
                },
            }
        } else {
            Diagnostic {
                lint: Lint::Surface(SurfaceLint::OverPublic),
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
        }
    } else if !usage.prod_used && usage.test_used {
        Diagnostic {
            lint: Lint::Surface(SurfaceLint::TestOnlyPub),
            key: key.clone(),
            message: format!("`{name}` is `pub` but only referenced by tests"),
            location,
            notes: vec![format!("defined in private crate `{}`", tree.display(pkg))],
            picture: Picture::Arrow {
                from: key,
                to: "(tests only)".to_owned(),
                detail: "gate behind #[cfg(test)] or move to test support".to_owned(),
            },
        }
    } else if !usage.prod_used {
        Diagnostic {
            lint: Lint::Surface(SurfaceLint::UnusedPub),
            key: key.clone(),
            message: format!(
                "`{name}` is `pub` but has no non-test callers (never referenced in the workspace)"
            ),
            location,
            notes: vec![format!("defined in private crate `{}`", tree.display(pkg))],
            picture: Picture::Arrow {
                from: key,
                to: "(no callers)".to_owned(),
                detail: "remove dead item or narrow visibility".to_owned(),
            },
        }
    } else if usage.test_external_to_lib {
        let pkg_name = tree.display(pkg);
        Diagnostic {
            lint: Lint::Surface(SurfaceLint::TestOnlyPub),
            key: key.clone(),
            message: format!(
                "`{name}` is `pub` only for external tests; all non-test callers are inside `{pkg_name}`"
            ),
            location,
            notes: vec![
                "narrow to `pub(crate)` and move unit tests inline, or gate test helpers behind a test feature"
                    .to_owned(),
            ],
            picture: Picture::Arrow {
                from: key,
                to: pkg_name,
                detail: "external callers are exclusively tests".to_owned(),
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
            lint: Lint::Surface(SurfaceLint::OverPublic),
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
                (Lint::Surface(SurfaceLint::UnusedPub), "p::api::dead".into()),
                (
                    Lint::Surface(SurfaceLint::OverPublic),
                    "p::api::internal".into()
                ),
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
            vec![(
                Lint::Surface(SurfaceLint::UnusedPub),
                "p::api::Ghost".into()
            )]
        );
    }

    #[test]
    fn test_only_pub_catches_test_only_and_external_test_only_exports() {
        let mut sample = private_sample();
        let (api, caller) = (sample.module("api"), sample.module("caller"));
        let only_tests = sample.item(api, "helper_for_tests", ItemKind::Function);
        let prod_plus_ext_test = sample.item(api, "internal_plus_it", ItemKind::Function);
        sample.model.items[only_tests.index()].visibility = Visibility::Pub;
        sample.model.items[prod_plus_ext_test.index()].visibility = Visibility::Pub;

        // Internal prod caller for `internal_plus_it`.
        let prod_fn = sample.item(caller, "run", ItemKind::Function);
        sample.refer(prod_fn, prod_plus_ext_test);

        // Integration test target calling both.
        let pkg = sample.module("");
        sample.file(pkg, "tests/it.rs", 1);
        let test_file = crate::model::FileId::new(sample.model.files.len() - 1);
        sample.model.files[test_file.index()].target = Some(TargetKind::Test);
        sample.model.files[test_file.index()].test = true;
        let it_fn = sample.item(pkg, "test_it", ItemKind::Function);
        sample.model.items[it_fn.index()].file = Some(test_file);
        sample.refer(it_fn, only_tests);
        sample.model.references.last_mut().unwrap().file = test_file;
        sample.model.references.last_mut().unwrap().test = true;
        sample.refer(it_fn, prod_plus_ext_test);
        sample.model.references.last_mut().unwrap().file = test_file;
        sample.model.references.last_mut().unwrap().test = true;

        assert_eq!(
            report(&sample),
            vec![
                (
                    Lint::Surface(SurfaceLint::TestOnlyPub),
                    "p::api::helper_for_tests".into()
                ),
                (
                    Lint::Surface(SurfaceLint::TestOnlyPub),
                    "p::api::internal_plus_it".into()
                ),
            ]
        );
    }

    #[test]
    fn pub_mod_and_pub_fields_are_checked_for_over_public() {
        let mut sample = private_sample();
        let root = sample.module("");
        let internal_mod = sample.module("detail");
        let mod_item = sample.item(root, "detail", ItemKind::Module);
        sample.model.items[mod_item.index()].module = Some(internal_mod);
        sample.model.items[mod_item.index()].visibility = Visibility::Pub;

        let caller = sample.module("caller");
        let ty = sample.item(internal_mod, "Cfg", ItemKind::Type);
        sample.model.items[ty.index()].visibility = Visibility::Pub;
        let pub_field = sample.member(ty, internal_mod, "Cfg::secret", ItemKind::Field);
        sample.model.items[pub_field.index()].visibility = Visibility::Pub;

        // External crate uses `Cfg` (so `Cfg` and `detail` are justified? No, let's keep
        // `detail` internal-only and test `Cfg` in `api` with an internal-only `pub` field).
        let caller_fn = sample.item(caller, "use_detail", ItemKind::Function);
        sample.refer(caller_fn, ty);
        sample.refer(caller_fn, pub_field);

        // Second module `api` has an externally-used type with an internal-only `pub` field.
        let api = sample.module("api");
        let api_ty = sample.item(api, "Public", ItemKind::Type);
        sample.model.items[api_ty.index()].visibility = Visibility::Pub;
        let internal_field = sample.member(api_ty, api, "Public::local", ItemKind::Field);
        sample.model.items[internal_field.index()].visibility = Visibility::Pub;
        sample.refer(caller_fn, internal_field);

        let tree_root = sample.model.tree.root();
        let other = sample.model.tree.child(tree_root, NodeKind::Package, "app");
        sample.file(other, "app/src/main.rs", 1);
        let main_fn = sample.item(other, "main", ItemKind::Function);
        sample.refer(main_fn, api_ty);

        let found = report(&sample);
        assert!(
            found.contains(&(Lint::Surface(SurfaceLint::OverPublic), "p::detail".into())),
            "{found:?}"
        );
        assert!(
            found.contains(&(
                Lint::Surface(SurfaceLint::OverPublic),
                "p::api::Public::local".into()
            )),
            "{found:?}"
        );
    }
}
