//! `duplicate-code`: non-trivial non-test functions whose normalized token
//! streams are identical across different locations.

use std::collections::BTreeMap;

use super::{
    Diagnostic, HELD_IN_MIND, Lint, Location, Picture, clickable, counted, in_scope, item_location,
    without_generics,
};
use crate::graph::Graph;
use crate::model::{BodyFingerprint, ItemId, ItemKind, Model, NodeId};

pub fn check(model: &Model, graph: &Graph, scope: Option<NodeId>) -> Vec<Diagnostic> {
    let tree = &model.tree;
    let mut groups: BTreeMap<BodyFingerprint, Vec<(String, NodeId, Location, ItemId)>> =
        BTreeMap::new();

    for (index, item) in model.items.iter().enumerate() {
        if item.kind != ItemKind::Function || item.trait_impl {
            continue;
        }
        let Some(fp) = item.metrics.and_then(|m| m.body_fingerprint) else {
            continue;
        };
        if tree.is_test(item.owner) || item.file.is_some_and(|file| model.files[file.index()].test)
        {
            continue;
        }
        let id = ItemId::new(index);
        let Some(location) = item_location(model, id) else {
            continue;
        };
        let home =
            graph.thing_of[index].map_or(item.owner, |thing| graph.things[thing.index()].home);
        let key = format!("{}::{}", tree.display(home), without_generics(&item.name));
        groups
            .entry(fp)
            .or_default()
            .push((key, home, location, id));
    }

    let mut diagnostics = Vec::new();
    for (fp, mut entries) in groups {
        entries.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.2.cmp(&b.2)));
        // Deduplicate by exact location so macro expansions don't self-collide.
        entries.dedup_by(|a, b| a.2 == b.2);
        if entries.len() < 2 {
            continue;
        }
        if !entries
            .iter()
            .any(|(_, home, _, _)| in_scope(tree, scope, *home))
        {
            continue;
        }
        let mut keys: Vec<String> = entries.iter().map(|(k, _, _, _)| k.clone()).collect();
        keys.dedup();
        let key = keys.join(",");
        let first_loc = entries[0].2.clone();
        let mut notes: Vec<String> = entries
            .iter()
            .take(HELD_IN_MIND)
            .map(|(name, _, loc, _)| format!("{}: `{name}`", clickable(loc)))
            .collect();
        if entries.len() > HELD_IN_MIND {
            notes.push(format!(
                "and {}",
                counted(entries.len() - HELD_IN_MIND, "more copy")
            ));
        }
        diagnostics.push(Diagnostic {
            lint: Lint::DuplicateCode,
            key,
            message: format!(
                "{} share an identical normalized body ({} tokens)",
                counted(entries.len(), "function"),
                fp.tokens
            ),
            location: Some(first_loc),
            notes,
            picture: Picture::Bar {
                label: "copies".to_owned(),
                length: entries.len(),
                most: 1,
            },
        });
    }
    diagnostics
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{FunctionMetrics, tests::Sample};

    #[test]
    fn groups_functions_with_identical_fingerprint_and_skips_trait_impls() {
        let mut sample = Sample::new();
        let a = sample.module("a");
        let b = sample.module("b");
        let fp = BodyFingerprint {
            hash_a: 1,
            hash_b: 2,
            tokens: 60,
        };
        let f1 = sample.item(a, "compute", ItemKind::Function);
        let f2 = sample.item(b, "compute_copy", ItemKind::Function);
        let tr = sample.item(b, "Display::fmt", ItemKind::Function);
        sample.model.items[f1.index()].line = 10;
        sample.model.items[f2.index()].line = 20;
        sample.model.items[tr.index()].line = 30;
        sample.model.items[tr.index()].trait_impl = true;
        for id in [f1, f2, tr] {
            sample.model.items[id.index()].metrics = Some(FunctionMetrics {
                body_fingerprint: Some(fp),
                ..FunctionMetrics::default()
            });
        }
        let graph = Graph::new(&sample.model);
        let keys: Vec<String> = check(&sample.model, &graph, None)
            .into_iter()
            .map(|d| d.key)
            .collect();
        assert_eq!(keys, ["p::a::compute,p::b::compute_copy"]);
    }
}
