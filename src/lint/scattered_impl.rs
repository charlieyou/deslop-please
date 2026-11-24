//! `scattered-impl`: a type whose inherent `impl` blocks are spread across
//! multiple modules. Splitting inherent methods into sibling or unrelated
//! modules turns one type into a cross-cutting god handle and breeds module
//! cycles; even inside a type's own submodule tree, spreading inherent `impl`s
//! across more than seven modules is too much to hold in mind.

use std::collections::BTreeMap;

use super::{
    Diagnostic, EncapsulationLint, HELD_IN_MIND, Lint, Location, Picture, SurfaceLint, clickable,
    counted, full_name, in_scope, item_location, without_generics,
};
use crate::graph::{Graph, ThingId};
use crate::model::{ItemId, ItemKind, Model, NodeId};

/// The most submodules inside a type's own home subtree that may hold its
/// inherent `impl` blocks before they are considered scattered.
const MOST_SUBMODULES: usize = HELD_IN_MIND;

struct ImplSite {
    count: usize,
    first: Option<Location>,
}

pub fn check(model: &Model, graph: &Graph, scope: Option<NodeId>) -> Vec<Diagnostic> {
    let mut diagnostics = check_scattered_impls(model, graph, scope);
    diagnostics.extend(check_single_impl_traits(model, scope));
    diagnostics
}

fn check_scattered_impls(model: &Model, graph: &Graph, scope: Option<NodeId>) -> Vec<Diagnostic> {
    let tree = &model.tree;
    let mut by_type: BTreeMap<ThingId, BTreeMap<NodeId, ImplSite>> = BTreeMap::new();
    for (index, item) in model.items.iter().enumerate() {
        if !matches!(item.kind, ItemKind::Function | ItemKind::Constant) || item.trait_impl {
            continue;
        }
        if tree.is_test(item.owner) || item.file.is_some_and(|file| model.files[file.index()].test)
        {
            continue;
        }
        let Some(thing) = graph.thing_of[index] else {
            continue;
        };
        let root_item = graph.things[thing.index()].item;
        if root_item.index() == index || model.items[root_item.index()].kind != ItemKind::Type {
            continue;
        }
        let entry = by_type
            .entry(thing)
            .or_default()
            .entry(item.owner)
            .or_insert(ImplSite {
                count: 0,
                first: None,
            });
        entry.count += 1;
        if let Some(loc) = item_location(model, crate::model::ItemId::new(index))
            && entry.first.as_ref().is_none_or(|cur| &loc < cur)
        {
            entry.first = Some(loc);
        }
    }

    let mut diagnostics = Vec::new();
    for (thing, modules) in by_type {
        let home = graph.things[thing.index()].home;
        let outside = modules
            .keys()
            .filter(|&&module| !tree.is_within(module, home))
            .count();
        let scattered = (modules.len() >= 2 && outside > 0) || modules.len() > MOST_SUBMODULES;
        if !scattered {
            continue;
        }
        let concerned = in_scope(tree, scope, home)
            || modules.keys().any(|&module| in_scope(tree, scope, module));
        if concerned {
            diagnostics.push(diagnostic(model, graph, thing, home, outside, &modules));
        }
    }
    diagnostics
}

fn diagnostic(
    model: &Model,
    graph: &Graph,
    thing: ThingId,
    home: NodeId,
    outside: usize,
    modules: &BTreeMap<NodeId, ImplSite>,
) -> Diagnostic {
    let tree = &model.tree;
    let type_item = graph.things[thing.index()].item;
    let key = full_name(model, graph, thing);
    let type_name = without_generics(&model.items[type_item.index()].name);

    let mut ranked: Vec<(usize, String, Option<&Location>)> = modules
        .iter()
        .map(|(&module, site)| (site.count, tree.display(module), site.first.as_ref()))
        .collect();
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));

    let mut notes: Vec<String> = ranked
        .iter()
        .take(HELD_IN_MIND)
        .map(|(count, name, loc)| match loc {
            Some(loc) => format!("{}: `{name}` ({})", clickable(loc), counted(*count, "item")),
            None => format!("`{name}` ({})", counted(*count, "item")),
        })
        .collect();
    if ranked.len() > HELD_IN_MIND {
        notes.push(format!(
            "and {}",
            counted(ranked.len() - HELD_IN_MIND, "more module")
        ));
    }

    let mut spread_modules: Vec<(usize, String)> = ranked
        .iter()
        .map(|(count, name, _)| (*count, name.clone()))
        .collect();
    if spread_modules.len() > HELD_IN_MIND {
        let more = spread_modules.split_off(HELD_IN_MIND);
        let count = more.iter().map(|(c, _)| c).sum();
        spread_modules.push((count, format!("in {}", counted(more.len(), "more module"))));
    }

    let detail = if outside > 0 {
        format!(" ({outside} outside `{}`)", tree.display(home))
    } else {
        String::new()
    };

    Diagnostic {
        lint: Lint::Encapsulation(EncapsulationLint::ScatteredImpl),
        key: key.clone(),
        message: format!(
            "`{type_name}` has inherent impls scattered across {}{detail}",
            counted(modules.len(), "module")
        ),
        location: item_location(model, type_item),
        notes,
        picture: Picture::Spread {
            owner: key,
            field: "impl".to_owned(),
            modules: spread_modules,
            others: vec![format!("home: {}", tree.display(home))],
        },
    }
}

fn check_single_impl_traits(model: &Model, scope: Option<NodeId>) -> Vec<Diagnostic> {
    let tree = &model.tree;
    let mut traits: Vec<ItemId> = model.traits.iter().copied().collect();
    traits.sort();

    let mut diagnostics = Vec::new();
    for id in traits {
        let item = &model.items[id.index()];
        if tree.is_test(item.owner) || item.file.is_some_and(|f| model.files[f.index()].test) {
            continue;
        }
        let Some(pkg) = tree.package_of(item.owner) else {
            continue;
        };
        if tree.node(pkg).publish || !in_scope(tree, scope, item.owner) {
            continue;
        }
        let impls = model.trait_impls.get(&id).map(Vec::as_slice).unwrap_or(&[]);
        // Extension traits on std/external types or blanket `impl<T> Trait for T`
        // legitimately have a single impl block.
        if impls.iter().any(|im| !im.local_self) {
            continue;
        }
        if impls.len() > 1 {
            continue;
        }
        let name = without_generics(&item.name);
        let key = format!("{}::{name}", tree.display(item.owner));
        let detail = if impls.is_empty() {
            "0 impls in workspace".to_owned()
        } else {
            format!("only 1 impl (in `{}`)", tree.display(impls[0].owner))
        };
        diagnostics.push(Diagnostic {
            lint: Lint::Surface(SurfaceLint::SingleImplTrait),
            key: key.clone(),
            message: format!("trait `{name}` has {detail}"),
            location: item_location(model, id),
            notes: vec![
                "inline the concrete type instead of maintaining a single-implementation trait"
                    .to_owned(),
            ],
            picture: Picture::Arrow {
                from: key,
                to: "(concrete type)".to_owned(),
                detail,
            },
        });
    }
    diagnostics
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::tests::Sample;

    fn keys(sample: &Sample) -> Vec<String> {
        let graph = Graph::new(&sample.model);
        check(&sample.model, &graph, None)
            .into_iter()
            .map(|d| d.key)
            .collect()
    }

    #[test]
    fn inherent_impls_in_sibling_modules_are_scattered() {
        let mut sample = Sample::new();
        let (home, admin) = (sample.module("home"), sample.module("admin"));
        let store = sample.item(home, "Store", ItemKind::Type);
        sample.member(store, home, "Store::open", ItemKind::Function);
        assert_eq!(keys(&sample), Vec::<String>::new());

        // A trait impl outside home is normal and not scattered.
        let trait_fn = sample.member(store, admin, "<Store as Drop>::drop", ItemKind::Function);
        sample.model.items[trait_fn.index()].trait_impl = true;
        assert_eq!(keys(&sample), Vec::<String>::new());

        // Adding an inherent impl in a sibling module scatters it.
        sample.member(store, admin, "Store::gc", ItemKind::Function);
        assert_eq!(keys(&sample), ["p::home::Store"]);
    }

    #[test]
    fn single_impl_trait_flags_private_speculative_traits_and_spares_extensions() {
        let mut sample = Sample::new();
        let pkg = sample.module("");
        sample.model.tree.node_mut(pkg).publish = false;
        let m = sample.module("engine");
        let solo = sample.item(m, "Solo", ItemKind::Type);
        let ext = sample.item(m, "StrExt", ItemKind::Type);
        let multi = sample.item(m, "Multi", ItemKind::Type);
        sample.model.traits.extend([solo, ext, multi]);
        sample.model.trait_impls.insert(
            solo,
            vec![crate::model::TraitImpl {
                owner: m,
                test: false,
                local_self: true,
            }],
        );
        sample.model.trait_impls.insert(
            ext,
            vec![crate::model::TraitImpl {
                owner: m,
                test: false,
                local_self: false,
            }],
        );
        sample.model.trait_impls.insert(
            multi,
            vec![
                crate::model::TraitImpl {
                    owner: m,
                    test: false,
                    local_self: true,
                },
                crate::model::TraitImpl {
                    owner: m,
                    test: true,
                    local_self: true,
                },
            ],
        );
        assert_eq!(keys(&sample), ["p::engine::Solo"]);
    }

    #[test]
    fn inherent_impls_within_home_submodules_are_allowed_up_to_seven() {
        let mut sample = Sample::new();
        let home = sample.module("store");
        let ty = sample.item(home, "Store", ItemKind::Type);
        sample.member(ty, home, "Store::new", ItemKind::Function);
        for i in 1..HELD_IN_MIND {
            let sub = sample.module(&format!("store::sub{i}"));
            sample.member(ty, sub, &format!("Store::m{i}"), ItemKind::Function);
        }
        assert_eq!(keys(&sample), Vec::<String>::new());

        let eighth = sample.module("store::sub_extra");
        sample.member(ty, eighth, "Store::m_extra", ItemKind::Function);
        assert_eq!(keys(&sample), ["p::store::Store"]);
    }

    #[test]
    fn a_single_moved_impl_module_is_not_scattered() {
        let mut sample = Sample::new();
        let (home, other) = (sample.module("home"), sample.module("other"));
        let ty = sample.item(home, "Timers", ItemKind::Type);
        sample.member(ty, other, "Timers::arm", ItemKind::Function);
        sample.member(ty, other, "Timers::fire", ItemKind::Function);
        assert_eq!(keys(&sample), Vec::<String>::new());
    }
}
