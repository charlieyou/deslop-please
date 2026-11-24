//! `long-function` and `wide-signature`: ratchetable complexity ceilings on
//! individual functions that cannot be bypassed by inline `#[allow(...)]`.

use super::{Diagnostic, HELD_IN_MIND, Lint, Picture, in_scope, item_location, without_generics};
use crate::graph::Graph;
use crate::model::{ItemId, ItemKind, Model, NodeId};

/// The most lines one non-test function may span before it wants splitting.
const MOST_LINES: usize = 150;

/// The most branching points (`if`, `match`, loops, `&&`, `||`) in one
/// non-test function.
const MOST_BRANCHES: usize = 30;

/// The most non-`self` parameters one non-test function may take.
const MOST_PARAMS: usize = HELD_IN_MIND;

pub fn check(model: &Model, graph: &Graph, scope: Option<NodeId>) -> Vec<Diagnostic> {
    let tree = &model.tree;
    let mut diagnostics = Vec::new();
    for (index, item) in model.items.iter().enumerate() {
        if item.kind != ItemKind::Function {
            continue;
        }
        let Some(metrics) = item.metrics else {
            continue;
        };
        if tree.is_test(item.owner) || item.file.is_some_and(|file| model.files[file.index()].test)
        {
            continue;
        }
        let id = ItemId::new(index);
        let home =
            graph.thing_of[index].map_or(item.owner, |thing| graph.things[thing.index()].home);
        if !in_scope(tree, scope, home) {
            continue;
        }
        let name = without_generics(&item.name);
        let key = format!("{}::{name}", tree.display(home));
        let location = item_location(model, id);

        let lines = metrics.lines as usize;
        let branches = metrics.branches as usize;
        if lines > MOST_LINES || branches > MOST_BRANCHES {
            let (label, length, most, message) = if lines > MOST_LINES {
                (
                    "lines",
                    lines,
                    MOST_LINES,
                    format!(
                        "`{name}` is long: {lines} lines ({branches} branches), more than {MOST_LINES} lines"
                    ),
                )
            } else {
                (
                    "branches",
                    branches,
                    MOST_BRANCHES,
                    format!(
                        "`{name}` is complex: {branches} branches ({lines} lines), more than {MOST_BRANCHES} branches"
                    ),
                )
            };
            diagnostics.push(Diagnostic {
                lint: Lint::LongFunction,
                key: key.clone(),
                message,
                location: location.clone(),
                notes: vec![format!("{lines} lines, {branches} branches")],
                picture: Picture::Bar {
                    label: label.to_owned(),
                    length,
                    most,
                },
            });
        }

        let params = metrics.params as usize;
        if !item.trait_impl && !item.extern_abi && params > MOST_PARAMS {
            diagnostics.push(Diagnostic {
                lint: Lint::WideSignature,
                key,
                message: format!("`{name}` takes {params} parameters, more than {MOST_PARAMS}"),
                location,
                notes: vec!["group related parameters into a cohesive struct".to_owned()],
                picture: Picture::Bar {
                    label: "params".to_owned(),
                    length: params,
                    most: MOST_PARAMS,
                },
            });
        }
    }
    diagnostics
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{FunctionMetrics, tests::Sample};

    fn report(sample: &Sample) -> Vec<(Lint, String)> {
        let graph = Graph::new(&sample.model);
        check(&sample.model, &graph, None)
            .into_iter()
            .map(|d| (d.lint, d.key))
            .collect()
    }

    #[test]
    fn long_complex_and_wide_signature_functions_are_flagged() {
        let mut sample = Sample::new();
        let m = sample.module("engine");
        let ok = sample.item(m, "ok", ItemKind::Function);
        sample.model.items[ok.index()].metrics = Some(FunctionMetrics {
            lines: 150,
            branches: 30,
            params: 7,
        });
        let long = sample.item(m, "long", ItemKind::Function);
        sample.model.items[long.index()].metrics = Some(FunctionMetrics {
            lines: 151,
            branches: 5,
            params: 2,
        });
        let branchy = sample.item(m, "branchy", ItemKind::Function);
        sample.model.items[branchy.index()].metrics = Some(FunctionMetrics {
            lines: 80,
            branches: 31,
            params: 8,
        });
        assert_eq!(
            report(&sample),
            vec![
                (Lint::LongFunction, "p::engine::long".into()),
                (Lint::LongFunction, "p::engine::branchy".into()),
                (Lint::WideSignature, "p::engine::branchy".into()),
            ]
        );
    }
}
