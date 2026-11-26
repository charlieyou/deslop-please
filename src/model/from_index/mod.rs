//! A SCIP index of a workspace, and its files' text, turned into the plain model.

mod accessors;
mod blocks;
mod forwarders;
mod impls;
mod source;
mod targets;
mod wrappers;

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::Error;
use crate::model::{
    CrateDep, File, FileId, GlobReexport, Item, ItemId, ItemKind, Mention, Model, ModuleReexports,
    NodeId, NodeKind, Reexport, Reference, TraitImpl, Tree, Visibility, Wrapper,
};
use crate::scip::{Document, Impl, Index, Kind, Occurrence, Place, Position, Range, Symbol};
use crate::workspace::{Package, Workspace};
use blocks::Blocks;
use source::{
    Access, ImplBlock, Macros, Mutation, Plain, PubUse, Source, TextPath, owned_type, type_name,
};

/// The model of `workspace` from its `index`, with `consumed`, what its
/// consumers use of it: each symbol, and whether test code uses it.
pub fn build(
    index: &Index,
    workspace: &Workspace,
    consumed: &[(String, bool)],
) -> Result<Model, Error> {
    let mut builder = Builder::new(workspace);
    for document in &index.documents {
        builder.add_document(document)?;
    }
    let mut model = builder.finish(index, consumed);
    model.mentions.sort();
    Ok(model)
}

/// What the code of `workspace`, a consumer, uses of what is outside it,
/// from its `index`: each symbol, and whether test code uses it.
pub fn outside_uses(index: &Index, workspace: &Workspace) -> Result<Vec<(String, bool)>, Error> {
    let mut builder = Builder::new(workspace);
    for document in &index.documents {
        builder.add_document(document)?;
    }
    builder.mark_test_code();
    Ok(builder.outside_uses())
}

/// How many of a file's first lines may mark it `@generated`.
const GENERATED_MARK_WITHIN: usize = 5;

fn is_test_module(name: &str) -> bool {
    name == "tests" || name == "test" || name.ends_with("_tests") || name.starts_with("test_")
}

struct Builder<'a> {
    workspace: &'a Workspace,
    tree: Tree,
    files: Vec<File>,
    items: Vec<Item>,
    references: Vec<Reference>,
    glob_reexports: Vec<GlobReexport>,
    module_reexports: Vec<ModuleReexports>,
    /// By the place of each reference in `references`: where it is written.
    sites: Vec<Site>,
    packages: HashMap<(&'a str, &'a str), NodeId>,
    /// The package of each target's root file.
    target_roots: HashMap<&'a str, NodeId>,
    symbols: HashMap<&'a str, Named>,
    /// Where each item that is not a module is written, as its symbol says.
    places: HashMap<ItemId, Place<'a>>,
    functions: HashSet<ItemId>,
    /// Each item's definition, by its unit and whole range; `None` for an
    /// item defined more than once, which no one text describes.
    definitions: HashMap<ItemId, Option<(usize, Range)>>,
    units: Vec<Unit<'a>>,
}

/// What a symbol of the index stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Named {
    /// A parameter, variable, or type parameter, which only its document names.
    Local,
    Item(ItemId),
    /// Something outside the workspace, by what its last descriptor says it is.
    Outside(Kind),
}

/// Where a reference is written, and what its code does with what it names.
struct Site {
    access: Access,
    range: Range,
    /// The whole definition of its holder.
    holder: Option<Range>,
}

struct Unit<'a> {
    file: FileId,
    document: &'a Document<'a>,
    source: Source,
    impl_blocks: Vec<ImplBlock>,
    inline_modules: Blocks<NodeId>,
    /// The items whose definitions hold code: functions, types, fields, constants.
    holders: Blocks<ItemId>,
    named_modules: Vec<NodeId>,
    /// The modules its `mod` declarations declare, each with whether the
    /// declaration builds it only for tests.
    declared_modules: Vec<(NodeId, bool)>,
    /// External/workspace package names referenced via SCIP symbols in this file.
    scip_packages: HashSet<String>,
    /// Distinct identifier tokens appearing in this file.
    idents: HashSet<String>,
    /// What its plain `pub use` declarations name.
    reexports: Vec<Reexport>,
    /// Paths it writes as text, where.
    mentions: Vec<TextPath>,
    macros: Macros,
}

impl<'a> Builder<'a> {
    fn new(workspace: &'a Workspace) -> Builder<'a> {
        let mut builder = Builder {
            workspace,
            tree: Tree::new(),
            files: Vec::new(),
            items: Vec::new(),
            references: Vec::new(),
            glob_reexports: Vec::new(),
            module_reexports: Vec::new(),
            sites: Vec::new(),
            packages: HashMap::new(),
            target_roots: HashMap::new(),
            symbols: HashMap::new(),
            places: HashMap::new(),
            functions: HashSet::new(),
            definitions: HashMap::new(),
            units: Vec::new(),
        };
        builder.add_packages();
        builder
    }

    /// The model, once every document is in.
    fn finish(mut self, index: &Index, consumed: &[(String, bool)]) -> Model {
        let traits = self.read_symbol_information(index);
        self.mark_owned(index);
        self.mark_signatures();
        self.mark_test_code();
        self.place_in_types();
        self.mark_function_info();
        let derives = self.collect_derives();
        let trait_impls = self.collect_trait_impls(&traits);
        self.mark_copies();
        let field_types = self.mark_markers();
        let wrappers = self.wrappers();
        self.mark_lent_reads(&wrappers);
        let crate_deps = self.collect_crate_deps();
        let mut mentions = self.text_mentions();
        mentions.extend(self.macro_mentions());
        mentions.extend(consumed.iter().filter_map(|(symbol, test)| {
            match self.symbols.get(symbol.as_str()) {
                Some(&Named::Item(item)) => Some(Mention {
                    item,
                    test: *test,
                    from: None,
                }),
                _ => None,
            }
        }));
        Model {
            tree: self.tree,
            files: self.files,
            items: self.items,
            references: self.references,
            mentions,
            reexports: self
                .units
                .iter()
                .flat_map(|unit| unit.reexports.clone())
                .collect(),
            wrappers,
            glob_reexports: self.glob_reexports,
            module_reexports: self.module_reexports,
            crate_deps,
            settings: self.workspace.settings.clone(),
            derives,
            field_types,
            traits,
            trait_impls,
        }
    }

    /// The functions and constants that the workspace's code names as text,
    /// by a path that ends with theirs: `EvDepth::is_off` names a method
    /// `is_off` of a type `EvDepth`, `LIMIT` a constant `LIMIT`. A path names
    /// what its own package has of that name, else what any package has; a
    /// format string's capture names only a constant of its module's or an
    /// enclosing one's.
    fn text_mentions(&self) -> Vec<Mention> {
        let mut by_name: HashMap<&str, Vec<ItemId>> = HashMap::new();
        for (index, item) in self.items.iter().enumerate() {
            if matches!(item.kind, ItemKind::Function | ItemKind::Constant) {
                let last = item.name.rsplit("::").next().unwrap_or(&item.name);
                by_name.entry(last).or_default().push(ItemId::new(index));
            }
        }
        let mut mentions = Vec::new();
        let imported: Vec<Vec<(NodeId, ItemId, bool)>> =
            self.units.iter().map(|unit| self.imported(unit)).collect();
        // What each module's `use` declarations name, or a glob's may: what
        // a glob of the module brings in.
        let used: HashSet<(NodeId, ItemId)> = imported
            .iter()
            .flatten()
            .map(|&(module, item, _)| (module, item))
            .collect();
        let brings = |module: NodeId, item: ItemId| {
            used.contains(&(module, item))
                || self.glob_reexports.iter().any(|glob| glob.from == module)
        };
        for (index, unit) in self.units.iter().enumerate() {
            let file = &self.files[unit.file.index()];
            let package = self.tree.package_of(file.module);
            let imported = &imported[index];
            for text in &unit.mentions {
                let segments: Vec<&str> = text
                    .path
                    .split("::")
                    .skip_while(|s| matches!(*s, "crate" | "self" | "super"))
                    .collect();
                let Some(last) = segments.last() else {
                    continue;
                };
                let from = unit
                    .inline_modules
                    .innermost(text.range)
                    .unwrap_or(file.module);
                // A format string's `{LIMIT}` names a constant in scope: an
                // enclosing module's, or one that a `use` there names, by
                // name or by a glob of a module that holds or re-exports it.
                let in_scope = |id: ItemId| {
                    let item = &self.items[id.index()];
                    let named = |&(module, used, glob): &(NodeId, ItemId, bool)| {
                        let globbed = self.items[used.index()]
                            .module
                            .is_some_and(|holder| holder == item.owner || brings(holder, id));
                        self.tree.is_within(from, module) && (used == id || glob && globbed)
                    };
                    item.kind == ItemKind::Constant
                        && item.parent.is_none()
                        && (self.tree.is_within(from, item.owner) || imported.iter().any(named))
                };
                let named: Vec<ItemId> = by_name
                    .get(last)
                    .into_iter()
                    .flatten()
                    .copied()
                    .filter(|&item| self.ends_with(item, &segments))
                    .filter(|&item| !text.captured || in_scope(item))
                    .collect();
                let own: Vec<ItemId> = named
                    .iter()
                    .copied()
                    .filter(|&item| self.tree.package_of(self.items[item.index()].owner) == package)
                    .collect();
                let test = file.test || self.tree.is_test(from);
                for item in if own.is_empty() { named } else { own } {
                    mentions.push(Mention {
                        item,
                        test,
                        from: Some((unit.file, from)),
                    });
                }
            }
        }
        mentions
    }

    /// What `macro_rules!` bodies use, which the index sees no reference to:
    /// a field or method of a type of its crate that a body writes an `impl`
    /// block of, named in that body, as `Self { numerators, .. }` in
    /// `impl AverageRow<$atomic> { .. }`. Each is used where the body is.
    fn macro_mentions(&self) -> Vec<Mention> {
        // By its package and type's last name, each member of a type.
        let mut members: HashMap<(Option<NodeId>, &str), Vec<ItemId>> = HashMap::new();
        for (index, item) in self.items.iter().enumerate() {
            if let Some(parent) = item.parent
                && matches!(item.kind, ItemKind::Field | ItemKind::Function)
                && self.items[parent.index()].kind == ItemKind::Type
            {
                let parent = &self.items[parent.index()];
                let ty = parent.name.rsplit("::").next().unwrap_or(&parent.name);
                let package = self.tree.package_of(parent.owner);
                members
                    .entry((package, ty))
                    .or_default()
                    .push(ItemId::new(index));
            }
        }
        let mut mentions = Vec::new();
        for unit in &self.units {
            let file = &self.files[unit.file.index()];
            let package = self.tree.package_of(file.module);
            for body in &unit.macros.bodies {
                let from = unit
                    .inline_modules
                    .innermost(body.range)
                    .unwrap_or(file.module);
                let test = file.test || self.tree.is_test(from);
                let named = body
                    .types
                    .iter()
                    .flat_map(|ty| members.get(&(package, ty.as_str())).into_iter().flatten())
                    .filter(|item| {
                        let name = &self.items[item.index()].name;
                        body.names
                            .contains(name.rsplit("::").next().unwrap_or(name))
                    });
                mentions.extend(named.map(|&item| Mention {
                    item,
                    test,
                    from: Some((unit.file, from)),
                }));
            }
        }
        mentions
    }

    /// What the `use` declarations of `unit` name, by the module each is in,
    /// and whether a glob, `name::*`, names it.
    fn imported(&self, unit: &Unit) -> Vec<(NodeId, ItemId, bool)> {
        let module = self.files[unit.file.index()].module;
        let scan = unit.source.scan_file();
        let imports = scan.imports;
        let mut imported = Vec::new();
        for occurrence in unit.document.occurrences.iter().filter(|o| !o.definition) {
            let before = imports.partition_point(|import| import.start <= occurrence.range.start);
            if before > 0
                && imports[before - 1].holds(occurrence.range)
                && let Some(Named::Item(item)) = self.symbols.get(occurrence.symbol)
            {
                let from = unit
                    .inline_modules
                    .innermost(occurrence.range)
                    .unwrap_or(module);
                let glob = scan.globs.iter().any(|g| g.start == occurrence.range.start);
                imported.push((from, *item, glob));
            }
        }
        imported
    }

    /// Whether the path of `item`, its crate's, modules' and own names,
    /// ends with `segments`.
    fn ends_with(&self, item: ItemId, segments: &[&str]) -> bool {
        let item = &self.items[item.index()];
        let path = format!("{}::{}", self.tree.display(item.owner), item.name);
        let path: Vec<&str> = path.split("::").collect();
        path.ends_with(segments)
    }

    /// What the code uses of what is outside the workspace: each symbol, and
    /// whether test code uses it. Test code must be marked.
    fn outside_uses(&self) -> Vec<(String, bool)> {
        let mut uses = Vec::new();
        for unit in &self.units {
            let file = &self.files[unit.file.index()];
            let imports = unit.source.scan_file().imports;
            for occurrence in unit.document.occurrences.iter().filter(|o| !o.definition) {
                let before =
                    imports.partition_point(|import| import.start <= occurrence.range.start);
                let imported = before > 0 && imports[before - 1].holds(occurrence.range);
                let outside =
                    matches!(self.symbols.get(occurrence.symbol), Some(Named::Outside(_)));
                if imported || !outside {
                    continue;
                }
                let from = unit
                    .inline_modules
                    .innermost(occurrence.range)
                    .unwrap_or(file.module);
                uses.push((
                    occurrence.symbol.to_owned(),
                    file.test || self.tree.is_test(from),
                ));
            }
        }
        uses.sort();
        uses.dedup();
        uses
    }

    fn add_packages(&mut self) {
        let mut packages: Vec<&Package> = self.workspace.packages.iter().collect();
        packages.sort_by_key(|package| package.directory.split('/').count());
        let root = self.tree.root();
        let mut placed: Vec<(&str, NodeId)> = Vec::new();
        for package in packages {
            let node = if package.directory.is_empty() {
                let node = self.tree.node_mut(root);
                node.kind = NodeKind::Package;
                node.name = package.crate_name.clone();
                root
            } else {
                let parent = placed
                    .iter()
                    .filter(|(directory, _)| is_under(&package.directory, directory))
                    .max_by_key(|(directory, _)| directory.len())
                    .map_or(root, |&(_, node)| node);
                self.tree
                    .child(parent, NodeKind::Package, &package.crate_name)
            };
            self.tree.node_mut(node).publish = package.publish;
            placed.push((&package.directory, node));
            self.packages
                .insert((&package.name, &package.version), node);
            for target in &package.targets {
                self.target_roots.insert(&target.root, node);
            }
        }
    }

    fn add_document(&mut self, document: &'a Document<'a>) -> Result<(), Error> {
        let not_understood = |why: &str| Error::new(format!("{}: {why}", document.path));
        let first = document
            .occurrences
            .first()
            .ok_or_else(|| not_understood(NOT_THE_FILES_MODULE))?;
        let symbol = parse(first.symbol)?.ok_or_else(|| not_understood(NOT_THE_FILES_MODULE))?;
        let path = symbol
            .module()
            .ok_or_else(|| not_understood(NOT_THE_FILES_MODULE))?;
        let Some(module) = self.module(&symbol, &path) else {
            return Ok(());
        };
        let on_disk = self.workspace.root.join(&*document.path);
        let text = std::fs::read_to_string(&on_disk)
            .map_err(|e| Error::new(format!("{}: {e}", on_disk.display())))?;
        let lines = text.lines().count();
        let generated = text
            .lines()
            .take(GENERATED_MARK_WITHIN)
            .any(|line| line.contains("@generated"));
        let source = Source::new(text);
        let package_root = self.target_roots.get(&*document.path) == Some(&module);
        check_files_module(first, source.whole(), package_root).map_err(not_understood)?;
        let file = FileId::new(self.files.len());
        let test_features = self.workspace.settings.test_features.as_deref();
        let declarations = source.declarations(test_features.unwrap_or_default());
        self.files.push(File {
            path: document.path.to_string(),
            module,
            test: false,
            target: None,
            lines,
            for_tests: declarations.for_tests,
            generated,
        });
        let at = self.units.len();
        let declarations = declarations.modules;
        let mut inline_modules = Vec::new();
        let mut holders = Vec::new();
        let mut named_modules = Vec::new();
        let mut declared_modules = Vec::new();
        for occurrence in &document.occurrences[1..] {
            let Some(item) = self.item(occurrence.symbol)? else {
                continue;
            };
            let item_module = self.items[item.index()].module;
            if let Some(module) = item_module
                && let Some(declaration) = declarations
                    .iter()
                    .find(|declaration| declaration.name.holds(occurrence.range))
            {
                declared_modules.push((module, declaration.for_tests));
            }
            if !occurrence.definition {
                named_modules.extend(item_module);
                continue;
            }
            let Some(block) = occurrence.enclosing else {
                continue;
            };
            if let Some(module) = item_module {
                inline_modules.push((block, module));
            } else {
                holders.push((block, item));
                self.definitions
                    .entry(item)
                    .and_modify(|known| *known = None)
                    .or_insert(Some((at, block)));
            }
        }
        let mut unit = Unit {
            file,
            document,
            impl_blocks: source.impl_blocks(),
            source,
            inline_modules: Blocks::new(inline_modules),
            holders: Blocks::new(holders),
            named_modules,
            declared_modules,
            scip_packages: HashSet::new(),
            idents: HashSet::new(),
            reexports: Vec::new(),
            mentions: Vec::new(),
            macros: Macros::default(),
        };
        let scan = unit.source.scan_file();
        let mut scip_packages = HashSet::new();
        // A reference to the file's module is no name its code writes.
        let skip = usize::from(!first.definition);
        for occurrence in &document.occurrences[skip..] {
            if let Ok(Some(sym)) = parse(occurrence.symbol) {
                scip_packages.insert(sym.package_name().to_owned());
            }
            self.add_definition_or_reference(occurrence, &unit, module, &scan.imports)?;
        }
        unit.reexports = self.add_reexports(&unit, module, &scan.pub_uses);
        unit.scip_packages = scip_packages;
        unit.idents = scan.idents;
        unit.mentions = scan.mentions;
        unit.macros = scan.macros;
        self.units.push(unit);
        Ok(())
    }

    /// Records the unit's re-exports, and returns what its plain `pub use`
    /// declarations name.
    fn add_reexports(&mut self, unit: &Unit, module: NodeId, pub_uses: &[PubUse]) -> Vec<Reexport> {
        let mut reexports = Vec::new();
        let mut per_module: HashMap<NodeId, (u32, usize)> = HashMap::new();
        for pub_use in pub_uses {
            let from = unit
                .inline_modules
                .innermost(pub_use.range)
                .unwrap_or(module);
            let entry = per_module.entry(from).or_insert((pub_use.line, 0));
            entry.0 = entry.0.min(pub_use.line);
            entry.1 += 1;
            if !pub_use.restricted {
                let named = unit
                    .document
                    .occurrences
                    .iter()
                    .filter(|o| !o.definition && pub_use.range.holds(o.range));
                for occurrence in named {
                    if let Some(&Named::Item(item)) = self.symbols.get(occurrence.symbol)
                        && self.items[item.index()].module.is_none()
                    {
                        reexports.push(Reexport {
                            item,
                            file: unit.file,
                            line: occurrence.range.start.line + 1,
                        });
                    }
                }
            }
            if let Some(path) = &pub_use.glob_path {
                let target = unit
                    .document
                    .occurrences
                    .iter()
                    .filter(|o| !o.definition && pub_use.range.holds(o.range))
                    .filter_map(|o| match self.symbols.get(o.symbol) {
                        Some(&Named::Item(item)) => {
                            self.items[item.index()].module.map(|m| (o.range.start, m))
                        }
                        _ => None,
                    })
                    .max_by_key(|&(start, _)| start)
                    .map(|(_, m)| m);
                self.glob_reexports.push(GlobReexport {
                    file: unit.file,
                    line: pub_use.line,
                    from,
                    path: path.clone(),
                    target,
                    restricted: pub_use.restricted,
                    test: false,
                });
            }
        }
        for (reexport_module, (line, count)) in per_module {
            self.module_reexports.push(ModuleReexports {
                file: unit.file,
                line,
                module: reexport_module,
                count,
                test: false,
            });
        }
        reexports
    }

    /// `imports`: where the unit's `use` declarations are, in order.
    fn add_definition_or_reference(
        &mut self,
        occurrence: &Occurrence<'a>,
        unit: &Unit,
        module: NodeId,
        imports: &[Range],
    ) -> Result<(), Error> {
        let Some(item) = self.item(occurrence.symbol)? else {
            return Ok(());
        };
        if occurrence.definition {
            let entry = &mut self.items[item.index()];
            if entry.file.is_none() {
                entry.file = Some(unit.file);
                entry.line = occurrence.range.start.line + 1;
            }
            return Ok(());
        }
        let before = imports.partition_point(|import| import.start <= occurrence.range.start);
        if before > 0 && imports[before - 1].holds(occurrence.range) {
            return Ok(());
        }
        let from = unit
            .inline_modules
            .innermost(occurrence.range)
            .unwrap_or(module);
        let holder = unit.holders.innermost_block(occurrence.range);
        let (access, mutation) = if self.functions.contains(&item) {
            (unit.source.call_access(occurrence.range), Mutation::None)
        } else {
            (
                unit.source.access(occurrence.range),
                unit.source.mutates(occurrence.range),
            )
        };
        self.references.push(Reference {
            file: unit.file,
            line: occurrence.range.start.line + 1,
            from,
            holder: holder.map(|(_, holder)| holder),
            item,
            copies: false,
            mutates: mutation != Mutation::None,
            owned: false,
            test: false,
            in_signature: false,
            struct_update: false,
        });
        self.sites.push(Site {
            access,
            range: occurrence.range,
            holder: holder.map(|(block, _)| block),
        });
        Ok(())
    }

    /// Marks the references that their holders' signatures write, and the
    /// types that struct literals with a base name.
    fn mark_signatures(&mut self) {
        for (at, site) in self.sites.iter().enumerate() {
            let reference = &self.references[at];
            let unit = &self.units[reference.file.index()];
            let holder = reference
                .holder
                .zip(site.holder)
                .map(|(holder, block)| (block, self.items[holder.index()].kind));
            let written = unit.source.written(site.range, holder);
            let names_type = self.items[reference.item.index()].kind == ItemKind::Type;
            let reference = &mut self.references[at];
            reference.in_signature = written.in_signature;
            reference.struct_update = written.struct_update && names_type;
        }
    }

    /// Records lines, branch count, parameter count, and (for free functions)
    /// the workspace type of the first parameter.
    fn mark_function_info(&mut self) {
        let mut metrics_found = Vec::new();
        let mut param_types_found = Vec::new();
        for (&item, &definition) in &self.definitions {
            let entry = &self.items[item.index()];
            if entry.kind != ItemKind::Function {
                continue;
            }
            let Some((at, range)) = definition else {
                continue;
            };
            let unit = &self.units[at];
            let Some((metrics, type_range)) = unit.source.function_info(range) else {
                continue;
            };
            metrics_found.push((item, metrics));
            if entry.parent.is_none()
                && let Some(type_range) = type_range
                && let Some(ty) = unit
                    .document
                    .occurrences
                    .iter()
                    .find(|o| o.range == type_range && !o.definition)
                    .and_then(|o| match self.symbols.get(o.symbol) {
                        Some(&Named::Item(ty)) if self.items[ty.index()].kind == ItemKind::Type => {
                            Some(ty)
                        }
                        _ => None,
                    })
            {
                param_types_found.push((item, ty));
            }
        }
        for (item, metrics) in metrics_found {
            self.items[item.index()].metrics = Some(metrics);
        }
        for (item, ty) in param_types_found {
            self.items[item.index()].first_param_type = Some(ty);
        }
    }

    /// What each workspace type derives, by each macro's last name.
    fn collect_derives(&self) -> HashMap<ItemId, Vec<String>> {
        let mut derives = HashMap::new();
        for (&item, &definition) in &self.definitions {
            if self.items[item.index()].kind != ItemKind::Type {
                continue;
            }
            if let Some((at, range)) = definition {
                let derived = self.units[at].source.derives(range);
                if !derived.is_empty() {
                    derives.insert(item, derived);
                }
            }
        }
        derives
    }

    /// Marks the references to fields that only read the field's value: a
    /// copy of any field, a shared borrow of plain data, a method call on a
    /// number.
    fn mark_copies(&mut self) {
        let mut plain: HashMap<ItemId, Plain> = HashMap::new();
        for at in 0..self.references.len() {
            let field = self.references[at].item;
            if self.items[field.index()].kind != ItemKind::Field {
                continue;
            }
            let reads = self.only_reads(self.sites[at].access, &[field], &mut plain);
            self.references[at].copies = reads;
        }
    }

    /// Marks the fields of marker types, which hold nothing.
    /// And returns each field's declared type.
    fn mark_markers(&mut self) -> HashMap<ItemId, String> {
        let mut types = HashMap::new();
        for (&item, &definition) in &self.definitions {
            if self.items[item.index()].kind == ItemKind::Field
                && let Some((unit, range)) = definition
            {
                let field = self.units[unit].source.field(range);
                self.items[item.index()].holds_nothing |= field.holds == Plain::Nothing;
                types.insert(item, field.written);
            }
        }
        types
    }

    /// Marks the calls of accessors that lend to read, directly or through
    /// forwarders, that only read what they lend, as [`Self::mark_copies`] marks
    /// code that names the fields: `*queue.head()` reads `head` as
    /// `queue.head` does.
    fn mark_lent_reads(&mut self, wrappers: &HashMap<ItemId, Wrapper>) {
        let mut plain: HashMap<ItemId, Plain> = HashMap::new();
        for at in 0..self.references.len() {
            let mut called = self.references[at].item;
            while let Some(Wrapper::Forwarder { target, .. }) = wrappers.get(&called) {
                called = *target;
            }
            if let Some(Wrapper::Accessor {
                lent,
                to_read,
                whole,
                ..
            }) = wrappers.get(&called)
                && !lent.is_empty()
            {
                if *to_read {
                    let reads = self.only_reads(self.sites[at].access, lent, &mut plain);
                    self.references[at].copies = reads;
                } else if !*whole {
                    self.references[at].mutates = true;
                }
            }
        }
    }

    /// Whether code that does `access` with `fields` only reads their
    /// values: takes a copy of any field, borrows plain data, or calls a
    /// method on a number. `plain` keeps what each field holds.
    fn only_reads(
        &self,
        access: Access,
        fields: &[ItemId],
        plain: &mut HashMap<ItemId, Plain>,
    ) -> bool {
        let mut holds = fields.iter().map(|&field| {
            *plain.entry(field).or_insert_with(|| {
                match self.definitions.get(&field).copied().flatten() {
                    Some((unit, range)) => self.units[unit].source.field(range).holds,
                    None => Plain::No,
                }
            })
        });
        match access {
            Access::Copy => true,
            Access::Borrow => holds.all(|holds| holds != Plain::No),
            Access::Call => holds.all(|holds| holds == Plain::Scalar),
            Access::Reach => false,
        }
    }

    fn item(&mut self, text: &'a str) -> Result<Option<ItemId>, Error> {
        match self.named(text)? {
            Named::Item(item) => Ok(Some(item)),
            Named::Local | Named::Outside(_) => Ok(None),
        }
    }

    fn named(&mut self, text: &'a str) -> Result<Named, Error> {
        if let Some(&known) = self.symbols.get(text) {
            return Ok(known);
        }
        let named = match parse(text)? {
            Some(symbol) => self
                .new_item(&symbol)
                .map_or(Named::Outside(symbol.kind()), Named::Item),
            None => Named::Local,
        };
        self.symbols.insert(text, named);
        Ok(named)
    }

    fn new_item(&mut self, symbol: &Symbol<'a>) -> Option<ItemId> {
        let id = ItemId::new(self.items.len());
        if let Some(path) = symbol.module() {
            let module = self.module(symbol, &path)?;
            let owner = self.tree.parent(module).unwrap_or(module);
            self.items.push(Item {
                name: self.tree.node(module).name.clone(),
                owner,
                module: Some(module),
                file: None,
                line: 0,
                kind: ItemKind::Module,
                parent: None,
                first_param_type: None,
                visibility: Visibility::Private,
                trait_impl: false,
                takes_self: false,
                is_unsafe: false,
                extern_abi: false,
                holds_nothing: false,
                metrics: None,
            });
            return Some(id);
        }
        let owner = self.module(symbol, &symbol.namespaces())?;
        let place = symbol.place();
        let (kind, trait_impl) = match &place {
            Place::Impl(Impl { is_block: true, .. }) => (ItemKind::Impl, false),
            Place::Impl(block) => (ItemKind::Other, block.of_trait),
            Place::Module | Place::In(_) => (ItemKind::Other, false),
        };
        self.items.push(Item {
            name: symbol.item_name(),
            owner,
            module: None,
            file: None,
            line: 0,
            kind,
            parent: None,
            first_param_type: None,
            visibility: Visibility::Private,
            trait_impl,
            takes_self: false,
            is_unsafe: false,
            extern_abi: false,
            holds_nothing: false,
            metrics: None,
        });
        if symbol.is_function() {
            self.functions.insert(id);
        }
        self.places.insert(id, place);
        Some(id)
    }

    /// Marks each mutation whose path, the variable and the fields that lead
    /// to the field it mutates, holds values of the function's own: each
    /// type, as the index declares it, borrows nothing, and the variable's
    /// is a type of the workspace's, no type parameter that may be `&mut`.
    fn mark_owned(&mut self, index: &Index) {
        // A `local N` symbol is only its document's: those are by document.
        let declared: HashMap<&str, &str> = index
            .documents
            .iter()
            .flat_map(|document| &document.symbols)
            .filter(|symbol| !symbol.signature.is_empty() && !symbol.symbol.starts_with("local "))
            .map(|symbol| (symbol.symbol, symbol.signature))
            .collect();
        let types: HashSet<&str> = self
            .items
            .iter()
            .filter(|item| item.kind == ItemKind::Type)
            .map(|item| item.name.rsplit("::").next().unwrap_or(&item.name))
            .collect();
        let units: HashMap<FileId, &Unit> =
            self.units.iter().map(|unit| (unit.file, unit)).collect();
        // By file, how the index declares what each place names.
        let mut by_file: HashMap<FileId, BTreeMap<Position, &str>> = HashMap::new();
        for (at, reference) in self.references.iter_mut().enumerate() {
            let Some(unit) = units.get(&reference.file).filter(|_| reference.mutates) else {
                continue;
            };
            let Mutation::Of(path) = unit.source.mutates(self.sites[at].range) else {
                continue;
            };
            let signatures = by_file.entry(unit.file).or_insert_with(|| {
                let locals: HashMap<&str, &str> = unit
                    .document
                    .symbols
                    .iter()
                    .filter(|symbol| symbol.symbol.starts_with("local "))
                    .map(|symbol| (symbol.symbol, symbol.signature))
                    .collect();
                unit.document
                    .occurrences
                    .iter()
                    .filter_map(|o| {
                        let signature = locals.get(o.symbol).or_else(|| declared.get(o.symbol));
                        Some((o.range.start, *signature?))
                    })
                    .collect()
            });
            let root = signatures.get(&path.start).copied();
            reference.owned = root.is_some_and(|root| types.contains(type_name(root)))
                && signatures
                    .range(path.start..path.end)
                    .all(|(_, signature)| owned_type(signature));
        }
    }

    fn read_symbol_information(&mut self, index: &Index) -> HashSet<ItemId> {
        const TRAIT: u64 = 53;
        let mut traits = HashSet::new();
        for information in index.documents.iter().flat_map(|d| &d.symbols) {
            let Some(&Named::Item(id)) = self.symbols.get(information.symbol) else {
                continue;
            };
            if information.kind == TRAIT {
                traits.insert(id);
            }
            let item = &mut self.items[id.index()];
            if item.module.is_none() && item.kind != ItemKind::Impl {
                item.kind = item_kind(information.kind);
            }
            if !information.signature.is_empty() {
                let (visibility, is_unsafe, extern_abi) = parse_signature(information.signature);
                if visibility > item.visibility {
                    item.visibility = visibility;
                }
                item.is_unsafe |= is_unsafe;
                item.extern_abi |= extern_abi;
            }
        }
        traits
    }

    /// Collects every `impl Trait for Type` block in the workspace, combining
    /// explicit `impl` headers with SCIP `impl` symbols (for macro-generated impls).
    fn collect_trait_impls(&self, traits: &HashSet<ItemId>) -> HashMap<ItemId, Vec<TraitImpl>> {
        let mut by_trait: HashMap<ItemId, Vec<TraitImpl>> = HashMap::new();
        let mut explicit_spans: HashSet<(FileId, Range)> = HashSet::new();

        for unit in &self.units {
            let file = &self.files[unit.file.index()];
            for block in &unit.impl_blocks {
                let Some(trait_range) = block.trait_type else {
                    continue;
                };
                let Some(trait_id) = unit
                    .document
                    .occurrences
                    .iter()
                    .find(|o| !o.definition && o.range == trait_range)
                    .and_then(|o| match self.symbols.get(o.symbol) {
                        Some(&Named::Item(id)) if traits.contains(&id) => Some(id),
                        _ => None,
                    })
                else {
                    continue;
                };
                let owner = unit
                    .inline_modules
                    .innermost(block.block)
                    .unwrap_or(file.module);
                let test = file.test || self.tree.is_test(owner);
                let self_package = block.self_type.and_then(|self_range| {
                    let occurrence = unit
                        .document
                        .occurrences
                        .iter()
                        .find(|o| !o.definition && o.range == self_range)?;
                    match self.symbols.get(occurrence.symbol) {
                        Some(&Named::Item(ty)) => {
                            self.tree.package_of(self.items[ty.index()].owner)
                        }
                        _ => None,
                    }
                });
                explicit_spans.insert((unit.file, block.block));
                by_trait.entry(trait_id).or_default().push(TraitImpl {
                    owner,
                    test,
                    self_package,
                });
            }
        }

        // Catch macro-generated trait impls whose `impl` keyword is not written in source.
        let mut trait_by_name: HashMap<&str, Vec<ItemId>> = HashMap::new();
        for &id in traits {
            trait_by_name
                .entry(self.items[id.index()].name.as_str())
                .or_default()
                .push(id);
        }
        for (&id, place) in &self.places {
            let Place::Impl(Impl {
                of_trait: true,
                is_block: true,
                block,
                ..
            }) = place
            else {
                continue;
            };
            // Skip impl blocks already counted from explicit source syntax.
            if let Some(Some((at, range))) = self.definitions.get(&id) {
                let unit = &self.units[*at];
                if explicit_spans
                    .iter()
                    .any(|&(f, r)| f == unit.file && r.holds(*range))
                {
                    continue;
                }
            }
            let Some(trait_name) = block
                .strip_suffix(']')
                .and_then(|s| s.rsplit_once('['))
                .map(|(_, name)| name)
            else {
                continue;
            };
            let item = &self.items[id.index()];
            let test = item.file.is_some_and(|f| self.files[f.index()].test)
                || self.tree.is_test(item.owner);
            let self_package = item
                .parent
                .and_then(|ty| self.tree.package_of(self.items[ty.index()].owner));
            if let Some(candidates) = trait_by_name.get(trait_name) {
                let chosen = match candidates.as_slice() {
                    [only] => Some(*only),
                    many => {
                        let pkg = self.tree.package_of(item.owner);
                        many.iter()
                            .copied()
                            .find(|&t| self.tree.package_of(self.items[t.index()].owner) == pkg)
                    }
                };
                if let Some(trait_id) = chosen {
                    by_trait.entry(trait_id).or_default().push(TraitImpl {
                        owner: item.owner,
                        test,
                        self_package,
                    });
                }
            }
        }

        self.add_macro_trait_impls(&trait_by_name, &mut by_trait);
        by_trait
    }

    /// Adds an `impl` for each invocation of a `macro_rules!` whose body
    /// writes `impl Trait for`, in its package, of a trait of that package
    /// by that name. The index shows none of them.
    fn add_macro_trait_impls(
        &self,
        trait_by_name: &HashMap<&str, Vec<ItemId>>,
        by_trait: &mut HashMap<ItemId, Vec<TraitImpl>>,
    ) {
        for unit in &self.units {
            let file = &self.files[unit.file.index()];
            let package = self.tree.package_of(file.module);
            for (name, trait_name) in &unit.macros.impls {
                let Some(&trait_id) = trait_by_name.get(trait_name.as_str()).and_then(|ids| {
                    ids.iter()
                        .find(|&&t| self.tree.package_of(self.items[t.index()].owner) == package)
                }) else {
                    continue;
                };
                for other in &self.units {
                    let other_file = &self.files[other.file.index()];
                    if self.tree.package_of(other_file.module) != package {
                        continue;
                    }
                    let invocations = other.macros.invoked.iter().filter(|&n| n == name);
                    let test = other_file.test || self.tree.is_test(other_file.module);
                    by_trait
                        .entry(trait_id)
                        .or_default()
                        .extend(invocations.map(|_| TraitImpl {
                            owner: other_file.module,
                            test,
                            self_package: package,
                        }));
                }
            }
        }
    }

    fn module(&mut self, symbol: &Symbol, path: &[&str]) -> Option<NodeId> {
        let mut node = symbol.package_in(&self.packages)?;
        for name in path {
            node = self.tree.child(node, NodeKind::Module, name);
        }
        Some(node)
    }

    fn mark_test_code(&mut self) {
        let target_units: Vec<targets::Unit> = self
            .units
            .iter()
            .map(|unit| {
                let module = self.files[unit.file.index()].module;
                let mut defines = vec![module];
                defines.extend(unit.inline_modules.iter().map(|(_, module)| module));
                targets::Unit {
                    path: &unit.document.path,
                    module,
                    defines,
                    names: unit.named_modules.clone(),
                }
            })
            .collect();
        let roots: Vec<_> = self
            .workspace
            .packages
            .iter()
            .flat_map(|package| &package.targets)
            .map(|target| (target.kind, target.root.as_str()))
            .collect();
        let kinds = targets::assign(&target_units, &roots, &self.tree);
        for (unit, kind) in self.units.iter().zip(kinds) {
            let file = &mut self.files[unit.file.index()];
            file.target = kind;
            file.test = kind.is_some_and(|kind| kind.is_test());
        }
        // A module that is defined only in test or bench targets is test code
        // even when its name is not `tests`, `*_tests` or `test_*`. Otherwise
        // an integration test tree such as `tests/queue/part.rs` shares the
        // library module's path and makes `src/queue.rs` look like a parent
        // module to `shallow-module`.
        let mut non_test_modules = HashSet::new();
        let mut test_modules = HashSet::new();
        for unit in &self.units {
            let file = &self.files[unit.file.index()];
            let bucket = if file.test {
                &mut test_modules
            } else {
                &mut non_test_modules
            };
            bucket.insert(file.module);
            for (_, inline) in unit.inline_modules.iter() {
                bucket.insert(inline);
            }
        }
        for item in &self.items {
            if let Some(file) = item.file {
                let module = item.module.unwrap_or(item.owner);
                if self.files[file.index()].test {
                    test_modules.insert(module);
                } else {
                    non_test_modules.insert(module);
                }
            }
        }
        let ancestors: Vec<NodeId> = non_test_modules
            .iter()
            .flat_map(|&id| self.tree.ancestors(id))
            .collect();
        non_test_modules.extend(ancestors);
        let built_for_tests = self.built_for_tests();
        for id in self.tree.ids() {
            let node = self.tree.node_mut(id);
            node.test = node.kind == NodeKind::Module
                && (is_test_module(&node.name)
                    || built_for_tests.contains(&id)
                    || (test_modules.contains(&id) && !non_test_modules.contains(&id)));
        }
        let gated = self.workspace.settings.test_features.is_some();
        for reference in &mut self.references {
            let file = &self.files[reference.file.index()];
            reference.test = file.test
                || self.tree.is_test(reference.from)
                || (gated && file.tests_only_at(reference.line));
        }
        for glob in &mut self.glob_reexports {
            glob.test = self.files[glob.file.index()].test || self.tree.is_test(glob.from);
        }
        for entry in &mut self.module_reexports {
            entry.test = self.files[entry.file.index()].test || self.tree.is_test(entry.module);
        }
    }

    /// Collects normal `[dependencies]` per workspace package and marks each as
    /// used when referenced by SCIP symbols, non-test source tokens, or `[features]`.
    fn collect_crate_deps(&self) -> Vec<CrateDep> {
        let mut pkg_scip: HashMap<NodeId, HashSet<&str>> = HashMap::new();
        let mut pkg_idents: HashMap<NodeId, HashSet<&str>> = HashMap::new();
        for unit in &self.units {
            let file = &self.files[unit.file.index()];
            if file.test || self.tree.is_test(file.module) {
                continue;
            }
            let Some(pkg) = self.tree.package_of(file.module) else {
                continue;
            };
            pkg_scip
                .entry(pkg)
                .or_default()
                .extend(unit.scip_packages.iter().map(String::as_str));
            pkg_idents
                .entry(pkg)
                .or_default()
                .extend(unit.idents.iter().map(String::as_str));
        }
        let mut deps = Vec::new();
        for package in &self.workspace.packages {
            let Some(&pkg_node) = self
                .packages
                .get(&(package.name.as_str(), package.version.as_str()))
            else {
                continue;
            };
            let scip = pkg_scip.get(&pkg_node);
            let idents = pkg_idents.get(&pkg_node);
            for dep in &package.dependencies {
                if !dep.normal {
                    continue;
                }
                let in_scip = scip.is_some_and(|set| set.contains(dep.name.as_str()));
                let in_idents = idents.is_some_and(|set| set.contains(dep.crate_name.as_str()));
                let in_features = package.feature_deps.contains(&dep.name)
                    || package.feature_deps.contains(&dep.crate_name);
                deps.push(CrateDep {
                    package: pkg_node,
                    package_name: package.name.clone(),
                    dep_name: dep.name.clone(),
                    dep_crate: dep.crate_name.clone(),
                    optional: dep.optional,
                    used: in_scip || in_idents || in_features,
                });
            }
        }
        deps
    }

    /// The modules declared `#[cfg(test)] mod name;` or the like, whatever
    /// their names, and declared no other way outside test targets. SCIP's
    /// symbols name a package, not a target, so a module one target builds
    /// only for tests and another builds for itself is not test code.
    fn built_for_tests(&self) -> HashSet<NodeId> {
        let mut for_tests = HashSet::new();
        let mut built = HashSet::new();
        for unit in &self.units {
            let in_tests = self.files[unit.file.index()].test;
            for &(module, only_for_tests) in &unit.declared_modules {
                if only_for_tests {
                    for_tests.insert(module);
                } else if !in_tests {
                    built.insert(module);
                }
            }
        }
        for_tests.retain(|module| !built.contains(module));
        for_tests
    }
}

fn parse_signature(signature: &str) -> (Visibility, bool, bool) {
    let mut header = signature.trim_start();
    while header.starts_with("#[") {
        let Some(end) = header.find(']') else {
            break;
        };
        header = header[end + 1..].trim_start();
    }
    let (visibility, rest) = if let Some(after) = header.strip_prefix("pub(") {
        let after = after.find(')').map_or(after, |close| &after[close + 1..]);
        (Visibility::Restricted, after)
    } else if let Some(after) = header
        .strip_prefix("pub")
        .filter(|rest| rest.starts_with(char::is_whitespace))
    {
        (Visibility::Pub, after)
    } else {
        (Visibility::Private, header)
    };
    let before_args = rest.split(['(', '{', ';', '=']).next().unwrap_or(rest);
    let mut words = before_args.split_whitespace();
    let mut is_unsafe = false;
    let mut extern_abi = false;
    while let Some(word) = words.next() {
        match word {
            "unsafe" => is_unsafe = true,
            "extern"
                if words
                    .clone()
                    .next()
                    .is_some_and(|next| next.starts_with('"')) =>
            {
                extern_abi = true;
            }
            _ => {}
        }
    }
    (visibility, is_unsafe, extern_abi)
}

fn item_kind(kind: u64) -> ItemKind {
    const ASSOCIATED_TYPE: u64 = 3;
    const CONSTANT: u64 = 8;
    const ENUM: u64 = 11;
    const ENUM_MEMBER: u64 = 12;
    const FIELD: u64 = 15;
    const FUNCTION: u64 = 17;
    const METHOD: u64 = 26;
    const STRUCT: u64 = 49;
    const TRAIT: u64 = 53;
    const TYPE_ALIAS: u64 = 55;
    const UNION: u64 = 59;
    const TRAIT_METHOD: u64 = 70;
    const STATIC_METHOD: u64 = 80;
    const STATIC_VARIABLE: u64 = 82;
    match kind {
        ASSOCIATED_TYPE | ENUM | STRUCT | TRAIT | TYPE_ALIAS | UNION => ItemKind::Type,
        FUNCTION | METHOD | TRAIT_METHOD | STATIC_METHOD => ItemKind::Function,
        FIELD => ItemKind::Field,
        ENUM_MEMBER => ItemKind::Variant,
        CONSTANT | STATIC_VARIABLE => ItemKind::Constant,
        _ => ItemKind::Other,
    }
}

const NOT_THE_FILES_MODULE: &str = "the index does not start with the file's module";
const ANOTHER_TARGETS_MODULE: &str =
    "the index gives the file the module of another target that `include!`s it";

/// Checks that `first`, the first occurrence in the document of a file whose
/// text spans `whole`, is the file's module. rust-analyzer writes the module
/// over the whole text, which no name in the code spans, and takes it from
/// the first target that holds the file. A definition is taken as the file's
/// own module wherever the index puts it. Only a reference has to show by its
/// range that it is no name the code writes. It means that target holds the
/// file only through an `include!`, and the module is the including file's.
/// That is the file's own only when both files are roots of the package's
/// targets: `package_root` says the file is one, and `first` names the
/// package's root module.
fn check_files_module(
    first: &Occurrence,
    whole: Range,
    package_root: bool,
) -> Result<(), &'static str> {
    if first.definition {
        Ok(())
    } else if first.range != whole {
        Err(NOT_THE_FILES_MODULE)
    } else if !package_root {
        Err(ANOTHER_TARGETS_MODULE)
    } else {
        Ok(())
    }
}

fn parse(text: &str) -> Result<Option<Symbol<'_>>, Error> {
    Symbol::parse(text).map_err(|e| Error::new(e.to_string()))
}

fn is_under(path: &str, directory: &str) -> bool {
    directory.is_empty()
        || path
            .strip_prefix(directory)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn lines(range: Range) -> u32 {
    let last = if range.end.column == 0 {
        range.end.line
    } else {
        range.end.line + 1
    };
    last.saturating_sub(range.start.line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scip::Position;
    use crate::workspace::Package;

    #[test]
    fn a_package_is_inside_the_package_whose_directory_holds_it_and_a_plain_directory_is_no_level()
    {
        let package = |name: &str, directory: &str| Package {
            name: name.into(),
            version: "0.1.0".into(),
            crate_name: name.into(),
            directory: directory.into(),
            publish: true,
            targets: Vec::new(),
            dependencies: Vec::new(),
            feature_deps: HashSet::new(),
        };
        let workspace = Workspace {
            root: "/w".into(),
            target_directory: "/w/target".into(),
            packages: vec![
                package("helper", "app/tests/programs/helper"),
                package("lib", "crates/lib"),
                package("app", "app"),
            ],
            settings: Default::default(),
        };
        let builder = Builder::new(&workspace);
        let tree = &builder.tree;
        let node = |name| builder.packages[&(name, "0.1.0")];
        assert_eq!(tree.parent(node("lib")), Some(tree.root()));
        assert_eq!(tree.parent(node("app")), Some(tree.root()));
        assert_eq!(tree.parent(node("helper")), Some(node("app")));
    }

    #[test]
    fn a_definition_is_the_files_module_and_a_reference_only_over_its_text_at_a_root() {
        let range = |start: (u32, u32), end: (u32, u32)| Range {
            start: Position {
                line: start.0,
                column: start.1,
            },
            end: Position {
                line: end.0,
                column: end.1,
            },
        };
        let module = |range, definition| Occurrence {
            range,
            symbol: "rust-analyzer cargo p 0.1.0 crate/",
            definition,
            enclosing: definition.then_some(range),
        };
        let whole = range((0, 0), (5, 0));
        // What rust-analyzer writes for a file of its own module, and for
        // one that a target sorting first `include!`s.
        assert_eq!(
            check_files_module(&module(whole, true), whole, false),
            Ok(())
        );
        assert_eq!(
            check_files_module(&module(whole, false), whole, true),
            Ok(())
        );
        assert_eq!(
            check_files_module(&module(whole, false), whole, false),
            Err(ANOTHER_TARGETS_MODULE)
        );
        // A reference that the code writes first, `crate` in
        // `use crate::other;`, is no file's module. A definition is taken
        // wherever the index puts it, `inner` in `mod inner {` too.
        let name = range((0, 4), (0, 9));
        assert_eq!(
            check_files_module(&module(name, false), whole, true),
            Err(NOT_THE_FILES_MODULE)
        );
        assert_eq!(check_files_module(&module(name, true), whole, true), Ok(()));
    }

    #[test]
    fn test_modules_by_name() {
        assert!(is_test_module("tests"));
        assert!(is_test_module("test"));
        assert!(is_test_module("shuttle_tests"));
        assert!(is_test_module("test_machine"));
        assert!(!is_test_module("contests"));
        assert!(!is_test_module("testing"));
    }

    #[test]
    fn signature_visibility_unsafe_and_extern_abi_are_parsed() {
        assert_eq!(
            parse_signature("pub fn run(&self)"),
            (Visibility::Pub, false, false)
        );
        assert_eq!(
            parse_signature("pub(crate) unsafe fn poke()"),
            (Visibility::Restricted, true, false)
        );
        assert_eq!(
            parse_signature("pub extern \"C\" fn hook()"),
            (Visibility::Pub, false, true)
        );
        assert_eq!(
            parse_signature("#[inline]\npub struct Foo"),
            (Visibility::Pub, false, false)
        );
        assert_eq!(
            parse_signature("fn helper()"),
            (Visibility::Private, false, false)
        );
    }
}
