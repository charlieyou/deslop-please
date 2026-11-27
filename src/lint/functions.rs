//! `long-function` and `wide-signature`: ratchetable complexity ceilings on
//! individual functions that cannot be bypassed by inline `#[allow(...)]`.

use std::collections::{HashMap, HashSet};

use super::{
    Diagnostic, HELD_IN_MIND, HygieneLint, Lint, Location, Picture, SizeLint, in_scope,
    item_location, without_generics,
};
use crate::graph::Graph;
use crate::model::{
    FunctionMetrics, Item, ItemId, ItemKind, Model, NodeId, PositionalTrap, Visibility,
};
use crate::settings::{Settings, Traps, lists};

/// The most lines of code, from `fn` to its closing brace, that one non-test
/// function may hold before it wants splitting.
const MOST_LINES: usize = 150;

/// The most branching points (`if`, `match`, loops, `&&`, `||`) in one
/// non-test function.
const MOST_BRANCHES: usize = 30;

/// The most non-`self` parameters one non-test function may take.
const MOST_PARAMS: usize = HELD_IN_MIND;

/// The limits one function is held to: the defaults, or a kernel's.
struct Limits {
    lines: usize,
    params: usize,
    traps: Traps,
    /// The shortest run of parameters of one type, any type, that is a trap.
    same_type_run: Option<usize>,
    /// Why they are not the defaults.
    note: Option<&'static str>,
}

impl Limits {
    fn of(settings: &Settings, path: Option<&str>) -> Limits {
        let kernel = &settings.kernel;
        if path.is_some_and(|path| kernel.holds(path)) {
            Limits {
                lines: kernel.most_lines.unwrap_or(MOST_LINES),
                params: kernel
                    .most_params
                    .or(settings.wide_signature.most_params)
                    .unwrap_or(MOST_PARAMS),
                traps: kernel.positional_trap,
                same_type_run: settings
                    .positional_trap
                    .same_type_run
                    .filter(|_| kernel.positional_trap == Traps::All),
                note: Some("held to the kernel's limits, which deslop-please.toml sets"),
            }
        } else {
            Limits {
                lines: MOST_LINES,
                params: settings.wide_signature.most_params.unwrap_or(MOST_PARAMS),
                traps: Traps::All,
                same_type_run: settings.positional_trap.same_type_run,
                note: None,
            }
        }
    }

    /// `notes`, and why the limits are not the defaults.
    fn noted(&self, mut notes: Vec<String>) -> Vec<String> {
        notes.extend(self.note.map(str::to_owned));
        notes
    }
}

pub fn check(model: &Model, graph: &Graph, scope: Option<NodeId>) -> Vec<Diagnostic> {
    let tree = &model.tree;
    let called_once = called_once(model);
    let mut diagnostics = Vec::new();
    for (index, item) in model.items.iter().enumerate() {
        if item.kind != ItemKind::Function {
            continue;
        }
        let Some(metrics) = item.metrics else {
            continue;
        };
        if model.in_tests(item) || model.in_example(item.file) {
            continue;
        }
        let id = ItemId::new(index);
        let home =
            graph.thing_of[index].map_or(item.owner, |thing| graph.things[thing.index()].home);
        if !in_scope(tree, scope, home) {
            continue;
        }
        let name = without_generics(&item.name);
        let found = Found {
            key: format!("{}::{name}", tree.display(home)),
            location: item_location(model, id),
            limits: Limits::of(
                &model.settings,
                item.file
                    .map(|file| model.files[file.index()].path.as_str()),
            ),
            name,
        };
        diagnostics.extend(long(&found, &metrics));
        if item.trait_impl || item.extern_abi {
            continue;
        }
        // A struct of its parameters would be built once, for one call.
        let spared = model.settings.wide_signature.single_call_site
            && item.visibility == Visibility::Private
            && called_once.contains(&id);
        if !spared {
            diagnostics.extend(wide(&found, &metrics));
        }
        diagnostics.extend(trap(&found, &metrics));
        if metrics.returns_string_error && at_boundary(model, item) {
            diagnostics.push(string_error(&found));
        }
    }
    diagnostics
}

/// The functions that one place calls or names, apart from their own
/// bodies: a recursive call is no call site.
fn called_once(model: &Model) -> HashSet<ItemId> {
    let mut sites: HashMap<ItemId, usize> = HashMap::new();
    for reference in &model.references {
        if reference.holder != Some(reference.item) {
            *sites.entry(reference.item).or_default() += 1;
        }
    }
    for mention in &model.mentions {
        *sites.entry(mention.item).or_default() += 1;
    }
    sites
        .into_iter()
        .filter(|&(_, count)| count == 1)
        .map(|(item, _)| item)
        .collect()
}

/// A function that may be reported: how it is reported, and its limits.
struct Found {
    name: String,
    key: String,
    location: Option<Location>,
    limits: Limits,
}

fn long(found: &Found, metrics: &FunctionMetrics) -> Option<Diagnostic> {
    let Found { name, limits, .. } = found;
    let lines = metrics.lines as usize;
    let branches = metrics.branches as usize;
    let (label, length, most, message) = if lines > limits.lines {
        (
            "lines",
            lines,
            limits.lines,
            format!(
                "`{name}` is long: {lines} lines ({branches} branches), more than {} lines",
                limits.lines
            ),
        )
    } else if branches > MOST_BRANCHES {
        (
            "branches",
            branches,
            MOST_BRANCHES,
            format!(
                "`{name}` is complex: {branches} branches ({lines} lines), more than {MOST_BRANCHES} branches"
            ),
        )
    } else {
        return None;
    };
    Some(Diagnostic {
        lint: Lint::Size(SizeLint::LongFunction),
        key: found.key.clone(),
        message,
        location: found.location.clone(),
        notes: limits.noted(vec![format!("{lines} lines, {branches} branches")]),
        picture: Picture::Bar {
            label: label.to_owned(),
            length,
            most,
        },
    })
}

fn wide(found: &Found, metrics: &FunctionMetrics) -> Option<Diagnostic> {
    let Found { name, limits, .. } = found;
    let params = metrics.params as usize;
    (params > limits.params).then(|| Diagnostic {
        lint: Lint::Size(SizeLint::WideSignature),
        key: found.key.clone(),
        message: format!(
            "`{name}` takes {params} parameters, more than {}",
            limits.params
        ),
        location: found.location.clone(),
        notes: limits.noted(vec![
            "group related parameters into a cohesive struct".to_owned(),
        ]),
        picture: Picture::Bar {
            label: "params".to_owned(),
            length: params,
            most: limits.params,
        },
    })
}

fn trap(found: &Found, metrics: &FunctionMetrics) -> Option<Diagnostic> {
    let name = &found.name;
    let (message, note, label, length, most) = match metrics.positional_trap {
        Some(PositionalTrap::Booleans(count)) => (
            format!("`{name}` takes {count} `bool` parameters (positional flag trap)"),
            "replace boolean flags with named enums or an options struct".to_owned(),
            "bools",
            count as usize,
            1usize,
        ),
        Some(PositionalTrap::RepeatedPrimitive { count, kind })
            if found.limits.traps == Traps::All =>
        {
            (
                format!("`{name}` takes {count} consecutive `{kind}` parameters"),
                format!("group adjacent `{kind}` parameters into a named struct or newtypes"),
                kind,
                count as usize,
                3usize,
            )
        }
        _ => {
            let run = usize::from(metrics.same_type_run);
            let shortest = found.limits.same_type_run;
            let most = shortest.filter(|&most| run >= most)?;
            (
                format!("`{name}` takes {run} adjacent parameters of one type"),
                "group adjacent parameters of one type into a named struct or newtypes".to_owned(),
                "same type",
                run,
                most - 1,
            )
        }
    };
    Some(Diagnostic {
        lint: Lint::Size(SizeLint::PositionalTrap),
        key: found.key.clone(),
        message,
        location: found.location.clone(),
        notes: vec![note],
        picture: Picture::Bar {
            label: label.to_owned(),
            length,
            most,
        },
    })
}

/// Whether `function` is where `[string-error]` wants typed errors.
fn at_boundary(model: &Model, function: &Item) -> bool {
    let settings = &model.settings.string_error;
    let application = model
        .tree
        .package_of(function.owner)
        .is_some_and(|package| lists(&settings.application_crates, &model.tree.node(package).name));
    match function.visibility {
        Visibility::Pub => true,
        Visibility::Restricted => !application,
        Visibility::Private => !application && !settings.boundary_only,
    }
}

fn string_error(found: &Found) -> Diagnostic {
    Diagnostic {
        lint: Lint::Hygiene(HygieneLint::StringError),
        key: found.key.clone(),
        message: format!(
            "`{}` returns a stringly-typed `Result<_, String>`",
            found.name
        ),
        location: found.location.clone(),
        notes: vec!["return a typed error enum instead of `String`".to_owned()],
        picture: Picture::Arrow {
            from: found.key.clone(),
            to: "Result<_, String>".to_owned(),
            detail: "use a typed error enum".to_owned(),
        },
    }
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
    fn long_complex_and_wide_signature_functions_are_flagged() {
        let mut sample = Sample::new();
        let m = sample.module("engine");
        let ok = sample.item(m, "ok", ItemKind::Function);
        sample.model.items[ok.index()].metrics = Some(FunctionMetrics {
            lines: 150,
            branches: 30,
            params: 7,
            ..FunctionMetrics::default()
        });
        let long = sample.item(m, "long", ItemKind::Function);
        sample.model.items[long.index()].metrics = Some(FunctionMetrics {
            lines: 151,
            branches: 5,
            params: 2,
            ..FunctionMetrics::default()
        });
        let branchy = sample.item(m, "branchy", ItemKind::Function);
        sample.model.items[branchy.index()].metrics = Some(FunctionMetrics {
            lines: 80,
            branches: 31,
            params: 8,
            returns_string_error: true,
            positional_trap: Some(PositionalTrap::Booleans(2)),
            ..FunctionMetrics::default()
        });
        assert_eq!(
            report(&sample),
            vec![
                (Lint::Size(SizeLint::LongFunction), "p::engine::long".into()),
                (
                    Lint::Size(SizeLint::LongFunction),
                    "p::engine::branchy".into()
                ),
                (
                    Lint::Size(SizeLint::WideSignature),
                    "p::engine::branchy".into()
                ),
                (
                    Lint::Size(SizeLint::PositionalTrap),
                    "p::engine::branchy".into()
                ),
                (
                    Lint::Hygiene(HygieneLint::StringError),
                    "p::engine::branchy".into()
                ),
            ]
        );
    }
}
