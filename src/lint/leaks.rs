//! `field-leak`: code that works on a field of a type it is not part of. A
//! struct guards its fields when it keeps at least one of them to itself and
//! has a method of its own with a `self` parameter, not a wrapper, that uses
//! them. A marker field, a `PhantomData`, holds nothing to keep and plays no
//! part. Then each of its fields that an outsider writes, borrows, indexes or
//! calls a method on, directly or through a getter or setter, is known
//! outside it. Reading a value is no leak, with or without a getter: a copy
//! of a field, or a borrow of plain data. Whoever reads a value depends on
//! what it is, not on how the struct keeps it. Neither is calling a method
//! that passes the call on to a field, whose callers never see the field.
//! Its insiders are its own definition and methods, wherever their `impl`
//! blocks are, and the things in its tangle. Visibility keywords play no
//! part.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use super::{
    Diagnostic, HELD_IN_MIND, Lint, Location, Picture, clickable, counted, in_scope, listed,
    without_generics,
};
use crate::graph::{Graph, ThingId, Use};
use crate::model::{ItemId, ItemKind, Model, NodeId, Visibility};

/// What `field-leak` reads of the workspace: each struct's fields, the
/// uses of each field by outsiders, and the structs with a method of their
/// own that works on their fields.
struct Survey<'a> {
    fields: BTreeMap<ThingId, Vec<ItemId>>,
    outside: HashMap<ItemId, Vec<&'a Use>>,
    working: HashSet<ThingId>,
}

impl<'a> Survey<'a> {
    fn of(model: &Model, graph: &'a Graph) -> Survey<'a> {
        let mut fields: BTreeMap<ThingId, Vec<ItemId>> = BTreeMap::new();
        for (index, item) in model.items.iter().enumerate() {
            let of_struct = item
                .parent
                .is_some_and(|parent| model.items[parent.index()].kind == ItemKind::Type);
            if item.kind == ItemKind::Field
                && of_struct
                && !item.holds_nothing
                && let Some(thing) = graph.thing_of[index]
            {
                fields.entry(thing).or_default().push(ItemId::new(index));
            }
        }
        let mut outside: HashMap<ItemId, Vec<&Use>> = HashMap::new();
        let mut working: HashSet<ThingId> = HashSet::new();
        for usage in &graph.uses {
            let field = &model.items[usage.item.index()];
            let Some(owner) =
                graph.thing_of[usage.item.index()].filter(|_| field.kind == ItemKind::Field)
            else {
                continue;
            };
            if model.in_example(Some(model.references[usage.reference].file)) {
                continue;
            }
            let tangle = graph.tangle_of[owner.index()];
            let insider = usage.user.is_some_and(|user| {
                user == owner || (tangle.is_some() && graph.tangle_of[user.index()] == tangle)
            }) || (field.visibility == Visibility::Private
                && at_home(model, graph, usage));
            if !insider {
                outside.entry(usage.item).or_default().push(usage);
            } else if usage.user == Some(owner) && works(model, usage) {
                working.insert(owner);
            }
        }
        Survey {
            fields,
            outside,
            working,
        }
    }

    /// Whether `field-leak` reports fields of `thing`, a struct with
    /// `fields`, if `narrowed`, some of them, were one step less visible:
    /// private from `pub(crate)`, `pub(crate)` from `pub`.
    fn reports(&self, model: &Model, graph: &Graph, thing: ThingId, narrowed: &[ItemId]) -> bool {
        let fields = &self.fields[&thing];
        let policy = &model.settings.field_leak;
        let visibility = |field: ItemId| {
            let visibility = model.items[field.index()].visibility;
            match (narrowed.contains(&field), visibility) {
                (true, Visibility::Pub) => Visibility::Restricted,
                (true, _) => Visibility::Private,
                (false, visibility) => visibility,
            }
        };
        // A field narrowed to private is its home module's to use.
        let outsiders = |field: ItemId| -> Vec<&Use> {
            let uses = self.outside.get(&field).into_iter().flatten().copied();
            if narrowed.contains(&field) && visibility(field) == Visibility::Private {
                uses.filter(|usage| !at_home(model, graph, usage)).collect()
            } else {
                uses.collect()
            }
        };
        let kept = fields.iter().any(|&field| outsiders(field).is_empty());
        let plain = policy.plain_data
            && fields
                .iter()
                .all(|&field| visibility(field) == Visibility::Pub);
        if !kept || !self.working.contains(&thing) || plain {
            return false;
        }
        let serde = model.is_serde(graph.things[thing.index()].item);
        fields
            .iter()
            .any(|&field| !leaks(model, outsiders(field), serde).is_empty())
    }
}

/// Of `uses`, a field's by outsiders, those it leaks by: those that do
/// more than read its value, and of a serde schema's, only when one
/// mutates it.
fn leaks<'a>(model: &Model, uses: Vec<&'a Use>, serde: bool) -> Vec<&'a Use> {
    let uses: Vec<&Use> = uses.into_iter().filter(|usage| !usage.copies).collect();
    if serde
        && !uses
            .iter()
            .any(|usage| model.references[usage.reference].mutates)
    {
        return Vec::new();
    }
    uses
}

/// Whether `usage` is in the home module of the type it uses a field of,
/// which `home-module` makes an insider for a private field.
fn at_home(model: &Model, graph: &Graph, usage: &Use) -> bool {
    let Some(owner) = graph.thing_of[usage.item.index()] else {
        return false;
    };
    model.settings.field_leak.home_module
        && model.references[usage.reference].from == graph.things[owner.index()].home
}

/// Of `over`, fields that `over-public` would ask to narrow, those it must
/// not: narrowed one step, each would make `field-leak` report fields of
/// its struct, which it does not now. With `home-module`, a struct that
/// keeps no field keeps one that only its module uses, once it is private,
/// and then its other fields that outsiders use leak. When narrowing all
/// of a struct's together makes no report, none is held: each that only
/// its module uses is kept, but none is left to leak.
pub fn narrowing_leaks(model: &Model, graph: &Graph, over: &HashSet<ItemId>) -> HashSet<ItemId> {
    let survey = Survey::of(model, graph);
    let mut held = HashSet::new();
    for (&thing, fields) in &survey.fields {
        let candidates: Vec<ItemId> = fields
            .iter()
            .copied()
            .filter(|field| over.contains(field))
            .collect();
        if candidates.is_empty()
            || survey.reports(model, graph, thing, &[])
            || !survey.reports(model, graph, thing, &candidates)
        {
            continue;
        }
        // Narrowing what is left together then makes no report either: a
        // field that one alone makes kept would make it alone, with more
        // left to leak.
        held.extend(
            candidates
                .iter()
                .copied()
                .filter(|&field| survey.reports(model, graph, thing, &[field])),
        );
    }
    held
}

pub fn check(model: &Model, graph: &Graph, scope: Option<NodeId>) -> Vec<Diagnostic> {
    let survey = Survey::of(model, graph);
    let policy = &model.settings.field_leak;
    let mut diagnostics = Vec::new();
    for (&thing, fields) in &survey.fields {
        let outside = &survey.outside;
        let kept = fields.iter().any(|field| !outside.contains_key(field));
        let type_item = graph.things[thing.index()].item;
        let is_serde = model.is_serde(type_item);
        // Every field `pub`: a `pub(crate)` one is still guarded from
        // other crates, and widening a type's last private field to it
        // must not silence the lint.
        let plain = policy.plain_data
            && fields
                .iter()
                .all(|field| model.items[field.index()].visibility == Visibility::Pub);
        if !kept || !survey.working.contains(&thing) || plain {
            if let Some(diag) = shared_mutable_bag(model, graph, thing, fields, outside, scope) {
                diagnostics.push(diag);
            }
            continue;
        }
        for &field in fields {
            let all = outside.get(&field).cloned().unwrap_or_default();
            let uses = leaks(model, all, is_serde);
            if uses.is_empty() {
                continue;
            }
            let home = graph.things[thing.index()].home;
            let concerned = in_scope(&model.tree, scope, home)
                || uses.iter().any(|usage| {
                    in_scope(&model.tree, scope, model.references[usage.reference].from)
                });
            if concerned {
                let others = others(model, field, fields, outside);
                diagnostics.push(diagnostic(model, graph, thing, field, &uses, others));
            }
        }
    }
    diagnostics
}

/// `shared-mutable-bag`: a struct skipped by `field-leak` (keeps no fields or
/// has no `&self` methods) whose fields are mutated from 2 or more modules
/// outside its home module.
fn shared_mutable_bag(
    model: &Model,
    graph: &Graph,
    thing: ThingId,
    fields: &[ItemId],
    outside: &HashMap<ItemId, Vec<&Use>>,
    scope: Option<NodeId>,
) -> Option<Diagnostic> {
    let tree = &model.tree;
    let home = graph.things[thing.index()].home;
    let item = graph.things[thing.index()].item;
    if model.settings.shared_mutable_bag.deserialized_records
        && model.derives(item, &["Deserialize"])
    {
        return None;
    }
    let mut mutating_modules: BTreeMap<NodeId, BTreeSet<Location>> = BTreeMap::new();
    let mut mutated_fields: BTreeSet<String> = BTreeSet::new();
    let mut sites: BTreeMap<Location, String> = BTreeMap::new();

    for &field in fields {
        for usage in outside.get(&field).into_iter().flatten() {
            let reference = &model.references[usage.reference];
            if !reference.mutates
                || (reference.owned && model.settings.shared_mutable_bag.owned_locals)
                || tree.is_within(reference.from, home)
            {
                continue;
            }
            let at = Location {
                file: model.files[reference.file.index()].path.clone(),
                line: reference.line,
            };
            mutating_modules
                .entry(reference.from)
                .or_default()
                .insert(at.clone());
            mutated_fields.insert(bare_name(model, field));
            let user = match reference.holder {
                Some(holder) => {
                    let h = &model.items[holder.index()];
                    format!("{}::{}", tree.display(h.owner), without_generics(&h.name))
                }
                None => tree.display(reference.from),
            };
            sites
                .entry(at)
                .or_insert(format!("`{user}` mutates `{}`", bare_name(model, field)));
        }
    }
    if mutating_modules.len() < 2 {
        return None;
    }
    let concerned = in_scope(tree, scope, home)
        || mutating_modules
            .keys()
            .any(|&module| in_scope(tree, scope, module));
    if !concerned {
        return None;
    }

    let owner = &graph.things[thing.index()];
    let type_name = without_generics(&model.items[owner.item.index()].name);
    let key = format!("{}::{type_name}", tree.display(home));
    let mut notes: Vec<String> = sites
        .iter()
        .take(HELD_IN_MIND)
        .map(|(at, desc)| format!("{}: {desc}", clickable(at)))
        .collect();
    if sites.len() > HELD_IN_MIND {
        notes.push(format!(
            "and {}",
            counted(sites.len() - HELD_IN_MIND, "more place")
        ));
    }
    let mut modules: Vec<(usize, String)> = mutating_modules
        .iter()
        .map(|(&m, locs)| (locs.len(), tree.display(m)))
        .collect();
    modules.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let module_count = modules.len();
    if modules.len() > HELD_IN_MIND {
        let more = modules.split_off(HELD_IN_MIND);
        let count = more.iter().map(|(c, _)| c).sum();
        modules.push((count, format!("in {}", counted(more.len(), "more module"))));
    }
    let field_list: Vec<String> = mutated_fields.into_iter().collect();
    Some(Diagnostic {
        lint: Lint::Encapsulation(super::EncapsulationLint::SharedMutableBag),
        key: key.clone(),
        message: format!(
            "`{type_name}` is a shared mutable bag: {} mutated across {}",
            counted(field_list.len(), "field"),
            counted(module_count, "module")
        ),
        location: sites.keys().next().cloned(),
        notes,
        picture: Picture::Spread {
            owner: key,
            field: "mut".to_owned(),
            modules,
            others: vec![format!("mutated fields: {}", listed(&field_list))],
        },
    })
}

/// Whether `usage` is by a method with a `self` parameter: one that works on
/// its type's fields.
fn works(model: &Model, usage: &Use) -> bool {
    model.references[usage.reference]
        .holder
        .map(|holder| &model.items[holder.index()])
        .is_some_and(|holder| holder.kind == ItemKind::Function && holder.takes_self)
}

/// What a picture of a leak of `field` says of the other `fields` of its
/// type: those that are known outside too, and those it keeps.
fn others(
    model: &Model,
    field: ItemId,
    fields: &[ItemId],
    outside: &HashMap<ItemId, Vec<&Use>>,
) -> Vec<String> {
    let (mut known, mut kept) = (Vec::new(), Vec::new());
    for &other in fields.iter().filter(|&&other| other != field) {
        match outside.get(&other) {
            None => kept.push(bare_name(model, other)),
            Some(uses) if uses.iter().any(|usage| !usage.copies) => {
                known.push(bare_name(model, other));
            }
            Some(_) => {}
        }
    }
    let mut others = Vec::new();
    if !known.is_empty() {
        others.push(format!("also known outside: {}", listed(&known)));
    }
    if !kept.is_empty() {
        others.push(format!("kept: {}", listed(&kept)));
    }
    others
}

/// `items` for `RunQueue::items`.
fn bare_name(model: &Model, field: ItemId) -> String {
    let name = &model.items[field.index()].name;
    name.rsplit("::").next().unwrap_or(name).to_owned()
}

fn diagnostic(
    model: &Model,
    graph: &Graph,
    thing: ThingId,
    field: ItemId,
    uses: &[&Use],
    others: Vec<String>,
) -> Diagnostic {
    let tree = &model.tree;
    let mut sites: BTreeMap<Location, String> = BTreeMap::new();
    let mut places: BTreeMap<NodeId, BTreeSet<Location>> = BTreeMap::new();
    for usage in uses {
        let reference = &model.references[usage.reference];
        let user = match reference.holder {
            Some(holder) => {
                let holder = &model.items[holder.index()];
                format!(
                    "{}::{}",
                    tree.display(holder.owner),
                    without_generics(&holder.name)
                )
            }
            None => tree.display(reference.from),
        };
        let through = usage.through.map_or(String::new(), |wrapper| {
            let name = without_generics(&model.items[wrapper.index()].name);
            format!(", through `{name}`")
        });
        let at = Location {
            file: model.files[reference.file.index()].path.clone(),
            line: reference.line,
        };
        places.entry(reference.from).or_default().insert(at.clone());
        sites.entry(at).or_insert(format!("`{user}`{through}"));
    }
    let owner = &graph.things[thing.index()];
    let type_name = without_generics(&model.items[owner.item.index()].name);
    let field_name = &model.items[field.index()].name;
    let mut notes: Vec<String> = sites
        .iter()
        .take(HELD_IN_MIND)
        .map(|(at, user)| format!("{}: {user}", clickable(at)))
        .collect();
    if sites.len() > HELD_IN_MIND {
        notes.push(format!(
            "and {}",
            counted(sites.len() - HELD_IN_MIND, "more place")
        ));
    }
    let mut modules: Vec<(usize, String)> = places
        .iter()
        .map(|(&module, places)| (places.len(), tree.display(module)))
        .collect();
    modules.sort_by(|(a, name_a), (b, name_b)| b.cmp(a).then_with(|| name_a.cmp(name_b)));
    let module_count = modules.len();
    if module_count > HELD_IN_MIND {
        let more = modules.split_off(HELD_IN_MIND);
        let count = more.iter().map(|(places, _)| places).sum();
        modules.push((count, format!("in {}", counted(more.len(), "more module"))));
    }
    Diagnostic {
        lint: Lint::Encapsulation(super::EncapsulationLint::FieldLeak),
        key: format!("{}::{field_name}", tree.display(owner.home)),
        message: format!(
            "`{field_name}` is known outside `{type_name}`: {} in {}",
            counted(sites.len(), "place"),
            counted(module_count, "module")
        ),
        location: sites.keys().next().cloned(),
        notes,
        picture: Picture::Spread {
            owner: format!("{}::{type_name}", tree.display(owner.home)),
            field: bare_name(model, field),
            modules,
            others,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::tests::Sample;

    /// `RunQueue { items, head, limit }` in `queue`, whose `full(&self)`
    /// reads every field, and a scheduler in `sched` that reads `items`.
    fn queue() -> (Sample, ItemId, ItemId) {
        let mut sample = Sample::new();
        let (queue, sched) = (sample.module("queue"), sample.module("sched"));
        let run_queue = sample.item(queue, "RunQueue", ItemKind::Type);
        let [items, head, limit] = ["items", "head", "limit"].map(|field| {
            sample.member(
                run_queue,
                queue,
                &format!("RunQueue::{field}"),
                ItemKind::Field,
            )
        });
        let full = sample.member(run_queue, queue, "RunQueue::full", ItemKind::Function);
        sample.model.items[full.index()].takes_self = true;
        for field in [items, head, limit] {
            sample.refer(full, field);
        }
        let admit = sample.item(sched, "admit", ItemKind::Function);
        sample.refer(admit, items);
        (sample, admit, head)
    }

    fn keys(sample: &Sample) -> Vec<String> {
        let graph = Graph::new(&sample.model);
        let mut keys: Vec<String> = check(&sample.model, &graph, None)
            .into_iter()
            .map(|d| d.key)
            .collect();
        keys.sort();
        keys
    }

    #[test]
    fn a_field_an_outsider_uses_leaks_from_a_struct_that_guards_its_others() {
        let (sample, _, _) = queue();
        assert_eq!(keys(&sample), ["p::queue::RunQueue::items"]);
        let graph = Graph::new(&sample.model);
        assert_eq!(
            check(&sample.model, &graph, None)[0].picture,
            Picture::Spread {
                owner: "p::queue::RunQueue".into(),
                field: "items".into(),
                modules: vec![(1, "p::sched".into())],
                others: vec!["kept: head, limit".into()],
            }
        );
    }

    #[test]
    fn a_getter_is_seen_through_and_free_code_in_the_same_file_is_an_outsider() {
        let (mut sample, admit, head) = queue();
        let queue = sample.model.items[head.index()].owner;
        let run_queue = sample.model.items[head.index()].parent.unwrap();
        let getter = sample.member(run_queue, queue, "RunQueue::head", ItemKind::Function);
        sample.refer(getter, head);
        let hands_out = |copies: bool| crate::model::Wrapper::Accessor {
            lent: if copies { vec![] } else { vec![head] },
            copied: if copies { vec![head] } else { vec![] },
            to_read: true,
            whole: false,
            bare: true,
        };
        sample.model.wrappers.insert(getter, hands_out(false));
        let drained = sample.item(queue, "drained", ItemKind::Function);
        sample.refer(drained, getter);
        sample.refer(admit, getter);
        assert_eq!(
            keys(&sample),
            ["p::queue::RunQueue::head", "p::queue::RunQueue::items"]
        );
        // `fn head(&self) -> usize { self.head }` hands out a copy.
        sample.model.wrappers.insert(getter, hands_out(true));
        assert_eq!(keys(&sample), ["p::queue::RunQueue::items"]);
    }

    #[test]
    fn a_borrow_a_getter_lends_is_no_leak_where_its_caller_only_reads_it() {
        let (mut sample, admit, head) = queue();
        let queue = sample.model.items[head.index()].owner;
        let run_queue = sample.model.items[head.index()].parent.unwrap();
        // `fn head(&self) -> &Head { &self.head }`
        let getter = sample.member(run_queue, queue, "RunQueue::head", ItemKind::Function);
        sample.refer(getter, head);
        let lends = crate::model::Wrapper::Accessor {
            lent: vec![head],
            copied: vec![],
            to_read: true,
            whole: false,
            bare: true,
        };
        sample.model.wrappers.insert(getter, lends);
        // `*queue.head()`
        sample.read(admit, getter);
        assert_eq!(keys(&sample), ["p::queue::RunQueue::items"]);
        // `queue.head().advance()`
        sample.refer(admit, getter);
        assert_eq!(
            keys(&sample),
            ["p::queue::RunQueue::head", "p::queue::RunQueue::items"]
        );
    }

    #[test]
    fn taking_a_copy_of_a_field_is_no_leak_but_the_field_is_not_kept() {
        let (mut sample, admit, head) = queue();
        sample.read(admit, head);
        assert_eq!(keys(&sample), ["p::queue::RunQueue::items"]);
        let limit = ItemId::new(head.index() + 1);
        sample.read(admit, limit);
        assert_eq!(keys(&sample), Vec::<String>::new());
    }

    #[test]
    fn a_marker_field_is_not_one_a_struct_keeps() {
        let (mut sample, admit, head) = queue();
        let limit = ItemId::new(head.index() + 1);
        sample.refer(admit, head);
        let queue = sample.model.items[head.index()].owner;
        let run_queue = sample.model.items[head.index()].parent.unwrap();
        let marker = sample.member(run_queue, queue, "RunQueue::_owner", ItemKind::Field);
        sample.model.items[marker.index()].holds_nothing = true;
        assert_eq!(
            keys(&sample),
            ["p::queue::RunQueue::head", "p::queue::RunQueue::items"]
        );
        sample.refer(admit, limit);
        assert_eq!(keys(&sample), Vec::<String>::new());
    }

    #[test]
    fn a_struct_that_keeps_nothing_or_does_nothing_guards_nothing() {
        let (mut sample, admit, head) = queue();
        let limit = ItemId::new(head.index() + 1);
        sample.refer(admit, head);
        sample.refer(admit, limit);
        assert_eq!(keys(&sample), Vec::<String>::new());
        let (mut sample, _, _) = queue();
        for item in &mut sample.model.items {
            item.takes_self = false;
        }
        assert_eq!(keys(&sample), Vec::<String>::new());
    }
}
