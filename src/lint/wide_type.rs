//! `wide-type`: a type that exposes more working functions to outsiders than a
//! reader can take in at once, even when the module around it hides enough
//! other helpers to pass `shallow-module`. A type's functions are its inherent
//! methods (not wrappers and not trait `impl`s) and any free functions in its
//! own module whose first parameter is that type, so turning methods into free
//! functions changes nothing while passive error/config types are never blamed.

use std::collections::{BTreeMap, BTreeSet};

use super::{
    Diagnostic, Lint, Picture, counted, full_name, in_scope, item_location, listed,
    without_generics,
};
use crate::graph::{Graph, ThingId};
use crate::model::{ItemId, ItemKind, Model, NodeId};

/// The most working functions one type shows to code outside it. Matches
/// `wide-directory`'s 15-item limit: past fifteen, a type's interface is
/// searched rather than read and wants smaller collaborating types.
const MOST_SHOWN: usize = 15;

pub fn check(model: &Model, graph: &Graph, scope: Option<NodeId>) -> Vec<Diagnostic> {
    let working = |item: ItemId| {
        let function = &model.items[item.index()];
        function.kind == ItemKind::Function
            && !function.trait_impl
            && !model.wrappers.contains_key(&item)
    };
    // By type thing: (shown working functions, external caller modules).
    let mut shown: BTreeMap<ThingId, (BTreeSet<ItemId>, BTreeSet<NodeId>)> = BTreeMap::new();
    for usage in &graph.uses {
        if !working(usage.item) {
            continue;
        }
        let Some(owner_thing) = graph.thing_of[usage.item.index()] else {
            continue;
        };
        let owner_item = &model.items[graph.things[owner_thing.index()].item.index()];
        let caller_mod = model.references[usage.reference].from;
        if owner_item.kind == ItemKind::Type {
            if usage.user != Some(owner_thing) {
                let entry = shown.entry(owner_thing).or_default();
                entry.0.insert(usage.item);
                entry.1.insert(caller_mod);
            }
        } else if graph.things[owner_thing.index()].item == usage.item
            && let Some(param_ty) = model.items[usage.item.index()].first_param_type
            && let Some(ty) = graph.thing_of[param_ty.index()]
        {
            let fn_home = graph.things[owner_thing.index()].home;
            if graph.things[ty.index()].home == fn_home
                && usage.user != Some(ty)
                && usage.user != Some(owner_thing)
            {
                let entry = shown.entry(ty).or_default();
                entry.0.insert(usage.item);
                entry.1.insert(caller_mod);
            }
        }
    }

    let mut diagnostics = Vec::new();
    for (thing, (functions, caller_modules)) in shown {
        if functions.len() <= MOST_SHOWN {
            continue;
        }
        let home = graph.things[thing.index()].home;
        if !in_scope(&model.tree, scope, home) {
            continue;
        }
        diagnostics.push(diagnostic(model, graph, thing, &functions, &caller_modules));
    }
    diagnostics
}

fn diagnostic(
    model: &Model,
    graph: &Graph,
    thing: ThingId,
    functions: &BTreeSet<ItemId>,
    caller_modules: &BTreeSet<NodeId>,
) -> Diagnostic {
    let type_item = graph.things[thing.index()].item;
    let key = full_name(model, graph, thing);
    let type_name = without_generics(&model.items[type_item.index()].name);
    let mut fn_names: Vec<String> = functions
        .iter()
        .map(|&f| without_generics(&model.items[f.index()].name))
        .collect();
    fn_names.sort();
    let mut mod_names: Vec<String> = caller_modules
        .iter()
        .map(|&m| model.tree.display(m))
        .collect();
    mod_names.sort();
    let notes = vec![
        format!(
            "shows {}: {}",
            counted(functions.len(), "function"),
            listed(&fn_names)
        ),
        format!(
            "used from {}: {}",
            counted(caller_modules.len(), "module"),
            listed(&mod_names)
        ),
    ];
    Diagnostic {
        lint: Lint::WideType,
        key,
        message: format!(
            "`{type_name}` is wide: it shows {} to outsiders, more than {MOST_SHOWN}",
            counted(functions.len(), "function")
        ),
        location: item_location(model, type_item),
        notes,
        picture: Picture::Bar {
            label: "shows".to_owned(),
            length: functions.len(),
            most: MOST_SHOWN,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Wrapper;
    use crate::model::tests::Sample;

    fn keys(sample: &Sample) -> Vec<String> {
        let graph = Graph::new(&sample.model);
        check(&sample.model, &graph, None)
            .into_iter()
            .map(|d| d.key)
            .collect()
    }

    #[test]
    fn a_type_showing_more_than_fifteen_working_functions_is_wide() {
        let mut sample = Sample::new();
        let (store, app) = (sample.module("store"), sample.module("app"));
        let ty = sample.item(store, "Store", ItemKind::Type);
        let run = sample.item(app, "run", ItemKind::Function);
        for i in 0..MOST_SHOWN {
            let m = sample.member(ty, store, &format!("Store::m{i}"), ItemKind::Function);
            sample.refer(run, m);
        }
        // Private helpers called only by `Store` itself do not widen its interface.
        let m0 = ItemId::new(2);
        for i in 0..10 {
            let helper = sample.member(ty, store, &format!("Store::h{i}"), ItemKind::Function);
            sample.refer(m0, helper);
        }
        assert_eq!(keys(&sample), Vec::<String>::new());

        let extra = sample.member(ty, store, "Store::m_extra", ItemKind::Function);
        sample.refer(run, extra);
        assert_eq!(keys(&sample), ["p::store::Store"]);
    }

    #[test]
    fn free_functions_in_the_types_module_count_and_wrappers_do_not() {
        let mut sample = Sample::new();
        let (store, app) = (sample.module("store"), sample.module("app"));
        let ty = sample.item(store, "Store", ItemKind::Type);
        let run = sample.item(app, "run", ItemKind::Function);
        let mut last_method = ty;
        for i in 0..14 {
            let m = sample.member(ty, store, &format!("Store::m{i}"), ItemKind::Function);
            sample.refer(run, m);
            last_method = m;
        }
        // A wrapper calling an existing method adds nothing.
        let fwd = sample.member(ty, store, "Store::fwd", ItemKind::Function);
        sample.model.wrappers.insert(
            fwd,
            Wrapper::Forwarder {
                target: last_method,
                fields: Vec::new(),
            },
        );
        sample.refer(run, fwd);
        assert_eq!(keys(&sample), Vec::<String>::new());

        // Two free functions in `store` whose first parameter is `Store` push it past 15.
        for name in ["f1", "f2"] {
            let f = sample.item(store, name, ItemKind::Function);
            sample.model.items[f.index()].first_param_type = Some(ty);
            sample.refer(f, ty);
            sample.refer(run, f);
        }
        assert_eq!(keys(&sample), ["p::store::Store"]);
    }
}
