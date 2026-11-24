//! `wide-struct`: a non-DTO struct with more fields than a reader can scan at
//! once. Structs deriving `Serialize` or `Deserialize` are exempt so wire/config
//! schemas stay flat; internal stateful bags want cohesive sub-structs.

use std::collections::BTreeMap;

use super::{
    Diagnostic, Lint, Picture, counted, full_name, in_scope, item_location, listed,
    without_generics,
};
use crate::graph::{Graph, ThingId};
use crate::model::{ItemId, ItemKind, Model, NodeId};

/// The most non-marker fields one non-serde struct may hold.
const MOST_FIELDS: usize = 15;

/// The most variants one enum may hold before it wants splitting.
const MOST_VARIANTS: usize = 25;

pub fn check(model: &Model, graph: &Graph, scope: Option<NodeId>) -> Vec<Diagnostic> {
    let mut diagnostics = check_structs(model, graph, scope);
    diagnostics.extend(check_enums(model, graph, scope));
    diagnostics
}

fn check_structs(model: &Model, graph: &Graph, scope: Option<NodeId>) -> Vec<Diagnostic> {
    let tree = &model.tree;
    let mut fields: BTreeMap<ThingId, Vec<ItemId>> = BTreeMap::new();
    for (index, item) in model.items.iter().enumerate() {
        let of_struct = item
            .parent
            .is_some_and(|parent| model.items[parent.index()].kind == ItemKind::Type);
        if item.kind == ItemKind::Field
            && of_struct
            && !item.holds_nothing
            && !tree.is_test(item.owner)
            && item.file.is_none_or(|file| !model.files[file.index()].test)
            && let Some(thing) = graph.thing_of[index]
        {
            fields.entry(thing).or_default().push(ItemId::new(index));
        }
    }

    let mut diagnostics = Vec::new();
    for (thing, struct_fields) in fields {
        if struct_fields.len() <= MOST_FIELDS {
            continue;
        }
        let type_item = graph.things[thing.index()].item;
        if model.serde_types.contains(&type_item) {
            continue;
        }
        let home = graph.things[thing.index()].home;
        if !in_scope(tree, scope, home) {
            continue;
        }
        let key = full_name(model, graph, thing);
        let type_name = without_generics(&model.items[type_item.index()].name);
        let mut names: Vec<String> = struct_fields
            .iter()
            .map(|&f| {
                let n = &model.items[f.index()].name;
                n.rsplit("::").next().unwrap_or(n).to_owned()
            })
            .collect();
        names.sort();
        diagnostics.push(Diagnostic {
            lint: Lint::WideStruct,
            key,
            message: format!(
                "`{type_name}` is wide: it holds {}, more than {MOST_FIELDS}",
                counted(struct_fields.len(), "field")
            ),
            location: item_location(model, type_item),
            notes: vec![format!("fields: {}", listed(&names))],
            picture: Picture::Bar {
                label: "fields".to_owned(),
                length: struct_fields.len(),
                most: MOST_FIELDS,
            },
        });
    }
    diagnostics
}

fn check_enums(model: &Model, graph: &Graph, scope: Option<NodeId>) -> Vec<Diagnostic> {
    let tree = &model.tree;
    let mut variants: BTreeMap<ThingId, Vec<ItemId>> = BTreeMap::new();
    for (index, item) in model.items.iter().enumerate() {
        let of_enum = item
            .parent
            .is_some_and(|parent| model.items[parent.index()].kind == ItemKind::Type);
        if item.kind == ItemKind::Variant
            && of_enum
            && !tree.is_test(item.owner)
            && item.file.is_none_or(|file| !model.files[file.index()].test)
            && let Some(thing) = graph.thing_of[index]
        {
            variants.entry(thing).or_default().push(ItemId::new(index));
        }
    }

    let mut diagnostics = Vec::new();
    for (thing, enum_variants) in variants {
        if enum_variants.len() <= MOST_VARIANTS {
            continue;
        }
        let home = graph.things[thing.index()].home;
        if !in_scope(tree, scope, home) {
            continue;
        }
        let type_item = graph.things[thing.index()].item;
        let key = full_name(model, graph, thing);
        let type_name = without_generics(&model.items[type_item.index()].name);
        let mut names: Vec<String> = enum_variants
            .iter()
            .map(|&v| {
                let n = &model.items[v.index()].name;
                n.rsplit("::").next().unwrap_or(n).to_owned()
            })
            .collect();
        names.sort();
        diagnostics.push(Diagnostic {
            lint: Lint::WideEnum,
            key,
            message: format!(
                "`{type_name}` is wide: it has {}, more than {MOST_VARIANTS}",
                counted(enum_variants.len(), "variant")
            ),
            location: item_location(model, type_item),
            notes: vec![format!("variants: {}", listed(&names))],
            picture: Picture::Bar {
                label: "variants".to_owned(),
                length: enum_variants.len(),
                most: MOST_VARIANTS,
            },
        });
    }
    diagnostics
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::tests::Sample;

    fn report(sample: &Sample) -> Vec<(Lint, String)> {
        let graph = Graph::new(&sample.model);
        check(&sample.model, &graph, None)
            .into_iter()
            .map(|d| (d.lint, d.key))
            .collect()
    }

    #[test]
    fn non_serde_structs_with_more_than_fifteen_fields_are_wide() {
        let mut sample = Sample::new();
        let m = sample.module("walk");
        let ctx = sample.item(m, "SolveCtx", ItemKind::Type);
        for i in 0..MOST_FIELDS {
            sample.member(ctx, m, &format!("SolveCtx::f{i}"), ItemKind::Field);
        }
        let marker = sample.member(ctx, m, "SolveCtx::_marker", ItemKind::Field);
        sample.model.items[marker.index()].holds_nothing = true;
        assert_eq!(report(&sample), Vec::<(Lint, String)>::new());

        sample.member(ctx, m, "SolveCtx::f_extra", ItemKind::Field);
        assert_eq!(
            report(&sample),
            [(Lint::WideStruct, "p::walk::SolveCtx".into())]
        );

        sample.model.serde_types.insert(ctx);
        assert_eq!(report(&sample), Vec::<(Lint, String)>::new());
    }

    #[test]
    fn enums_with_more_than_twenty_five_variants_are_wide() {
        let mut sample = Sample::new();
        let m = sample.module("errors");
        let err = sample.item(m, "MegaError", ItemKind::Type);
        for i in 0..MOST_VARIANTS {
            sample.member(err, m, &format!("MegaError::V{i}"), ItemKind::Variant);
        }
        assert_eq!(report(&sample), Vec::<(Lint, String)>::new());
        sample.member(err, m, "MegaError::Extra", ItemKind::Variant);
        assert_eq!(
            report(&sample),
            [(Lint::WideEnum, "p::errors::MegaError".into())]
        );
    }
}
