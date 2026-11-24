//! `wide-module`: a leaf module (one with no non-test child modules, which
//! `shallow-module` skips) that exposes more working free functions to callers
//! outside itself than a reader can scan at once.

use std::collections::{BTreeMap, BTreeSet};

use super::{
    Diagnostic, Lint, Picture, counted, in_scope, listed, module_location, without_generics,
};
use crate::graph::Graph;
use crate::model::{ItemId, ItemKind, Model, NodeId, NodeKind};

/// The most working free functions one leaf module shows to callers outside it.
const MOST_SHOWN: usize = 15;

pub fn check(model: &Model, graph: &Graph, scope: Option<NodeId>) -> Vec<Diagnostic> {
    let tree = &model.tree;
    let mut parents = BTreeSet::new();
    for id in tree.ids() {
        if !tree.is_test(id)
            && let Some(parent) = tree.parent(id)
        {
            parents.insert(parent);
        }
    }
    let is_leaf = |node: NodeId| {
        !tree.is_test(node) && tree.node(node).kind == NodeKind::Module && !parents.contains(&node)
    };

    let mut shown: BTreeMap<NodeId, (BTreeSet<ItemId>, BTreeSet<NodeId>)> = BTreeMap::new();
    for usage in &graph.uses {
        let item = &model.items[usage.item.index()];
        if item.kind != ItemKind::Function
            || item.trait_impl
            || item.parent.is_some()
            || model.wrappers.contains_key(&usage.item)
        {
            continue;
        }
        let home = item.owner;
        if !is_leaf(home) {
            continue;
        }
        let caller_mod = model.references[usage.reference].from;
        if tree.is_within(caller_mod, home) {
            continue;
        }
        let entry = shown.entry(home).or_default();
        entry.0.insert(usage.item);
        entry.1.insert(caller_mod);
    }

    let mut diagnostics = Vec::new();
    for (module, (functions, caller_modules)) in shown {
        if functions.len() <= MOST_SHOWN || !in_scope(tree, scope, module) {
            continue;
        }
        let name = tree.display(module);
        let mut fn_names: Vec<String> = functions
            .iter()
            .map(|&f| without_generics(&model.items[f.index()].name))
            .collect();
        fn_names.sort();
        let mut mod_names: Vec<String> = caller_modules.iter().map(|&m| tree.display(m)).collect();
        mod_names.sort();
        diagnostics.push(Diagnostic {
            lint: Lint::WideModule,
            key: name.clone(),
            message: format!(
                "`{name}` is wide: it shows {} to outsiders, more than {MOST_SHOWN}",
                counted(functions.len(), "free function")
            ),
            location: module_location(model, module),
            notes: vec![
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
            ],
            picture: Picture::Bar {
                label: "shows".to_owned(),
                length: functions.len(),
                most: MOST_SHOWN,
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
    fn a_leaf_module_showing_more_than_fifteen_free_functions_is_wide() {
        let mut sample = Sample::new();
        let (utils, app) = (sample.module("utils"), sample.module("app"));
        let run = sample.item(app, "run", ItemKind::Function);
        for i in 0..MOST_SHOWN {
            let f = sample.item(utils, &format!("f{i}"), ItemKind::Function);
            sample.refer(run, f);
        }
        assert_eq!(keys(&sample), Vec::<String>::new());

        let extra = sample.item(utils, "f_extra", ItemKind::Function);
        sample.refer(run, extra);
        assert_eq!(keys(&sample), ["p::utils"]);

        // Once `utils` has a non-test child module, `shallow-module` judges it instead.
        sample.module("utils::sub");
        assert_eq!(keys(&sample), Vec::<String>::new());
    }
}
