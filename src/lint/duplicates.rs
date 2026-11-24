//! `duplicate-code` and `duplicate-struct`: copy-pasted functions with identical
//! normalized bodies and parallel shadow structs with identical field sets.

use std::collections::BTreeMap;

use super::{
    Diagnostic, HELD_IN_MIND, HygieneLint, Lint, Location, Picture, clickable, counted, full_name,
    in_scope, item_location, listed, without_generics,
};
use crate::graph::{Graph, ThingId};
use crate::model::{BodyFingerprint, ItemId, ItemKind, Model, NodeId};

/// Minimum non-marker fields required before two structs with identical field
/// names are flagged as `duplicate-struct`.
const MIN_STRUCT_FIELDS: usize = 4;

pub fn check(model: &Model, graph: &Graph, scope: Option<NodeId>) -> Vec<Diagnostic> {
    let mut diagnostics = check_functions(model, graph, scope);
    diagnostics.extend(check_structs(model, graph, scope));
    diagnostics
}

fn check_functions(model: &Model, graph: &Graph, scope: Option<NodeId>) -> Vec<Diagnostic> {
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
            lint: Lint::Hygiene(HygieneLint::DuplicateCode),
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

fn check_structs(model: &Model, graph: &Graph, scope: Option<NodeId>) -> Vec<Diagnostic> {
    let tree = &model.tree;
    let mut by_thing: BTreeMap<ThingId, Vec<String>> = BTreeMap::new();
    for (index, item) in model.items.iter().enumerate() {
        let of_struct = item
            .parent
            .is_some_and(|parent| model.items[parent.index()].kind == ItemKind::Type);
        if item.kind == ItemKind::Field
            && of_struct
            && !item.holds_nothing
            && !tree.is_test(item.owner)
            && item.file.is_none_or(|f| !model.files[f.index()].test)
            && let Some(thing) = graph.thing_of[index]
        {
            let leaf = item.name.rsplit("::").next().unwrap_or(&item.name);
            by_thing.entry(thing).or_default().push(leaf.to_owned());
        }
    }

    // Group structs with identical `(is_serde, sorted_field_names)` so intentional
    // wire-DTO vs domain pairs stay separate while shadow clones in the same layer match.
    struct StructCopy {
        key: String,
        home: NodeId,
        location: Option<Location>,
    }
    let mut groups: BTreeMap<(bool, Vec<String>), Vec<StructCopy>> = BTreeMap::new();
    for (thing, mut fields) in by_thing {
        if fields.len() < MIN_STRUCT_FIELDS {
            continue;
        }
        fields.sort();
        let type_item = graph.things[thing.index()].item;
        let home = graph.things[thing.index()].home;
        let is_serde = model.serde_types.contains(&type_item);
        let key = full_name(model, graph, thing);
        let location = item_location(model, type_item);
        groups
            .entry((is_serde, fields))
            .or_default()
            .push(StructCopy {
                key,
                home,
                location,
            });
    }

    let mut diagnostics = Vec::new();
    for ((_, fields), mut entries) in groups {
        entries.sort_by(|a, b| a.key.cmp(&b.key).then_with(|| a.location.cmp(&b.location)));
        entries.dedup_by(|a, b| a.key == b.key);
        if entries.len() < 2 {
            continue;
        }
        if !entries.iter().any(|e| in_scope(tree, scope, e.home)) {
            continue;
        }
        let key = entries
            .iter()
            .map(|e| e.key.as_str())
            .collect::<Vec<_>>()
            .join(",");
        let first_loc = entries.iter().find_map(|e| e.location.clone());
        let mut notes: Vec<String> = entries
            .iter()
            .take(HELD_IN_MIND)
            .map(|e| match &e.location {
                Some(loc) => format!("{}: `{}`", clickable(loc), e.key),
                None => format!("`{}`", e.key),
            })
            .collect();
        notes.push(format!("shared fields: {}", listed(&fields)));
        diagnostics.push(Diagnostic {
            lint: Lint::Hygiene(HygieneLint::DuplicateStruct),
            key,
            message: format!(
                "{} duplicate the same {} fields",
                counted(entries.len(), "struct"),
                fields.len()
            ),
            location: first_loc,
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

    #[test]
    fn duplicate_struct_flags_same_layer_shadow_structs_and_spares_serde_vs_domain() {
        let mut sample = Sample::new();
        let (a, b) = (sample.module("a"), sample.module("b"));
        let s1 = sample.item(a, "ConfigA", ItemKind::Type);
        let s2 = sample.item(b, "ConfigB", ItemKind::Type);
        let dto = sample.item(b, "ConfigJson", ItemKind::Type);
        sample.model.serde_types.insert(dto);
        for ty in [s1, s2, dto] {
            let home = sample.model.items[ty.index()].owner;
            let ty_name = sample.model.items[ty.index()].name.clone();
            for field in ["ante", "bb", "sb", "stack"] {
                sample.member(ty, home, &format!("{ty_name}::{field}"), ItemKind::Field);
            }
        }
        let graph = Graph::new(&sample.model);
        let keys: Vec<(Lint, String)> = check(&sample.model, &graph, None)
            .into_iter()
            .map(|d| (d.lint, d.key))
            .collect();
        assert_eq!(
            keys,
            vec![(
                Lint::Hygiene(HygieneLint::DuplicateStruct),
                "p::a::ConfigA,p::b::ConfigB".into()
            )]
        );
    }
}
