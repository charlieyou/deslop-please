//! Plain-text allowlist for existing architectural debt. One entry per
//! line, sorted: `<lint> <key>`, then optionally `<= N`, a ceiling on what
//! the entry allows, then optionally `# <reason>`. Comment lines just above
//! an entry belong to it; those above the first entry are the file's header.
//! Fixing an allowed entry never fails the check.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::path::Path;

use crate::lint::{Diagnostic, GraphLint, HygieneLint, Lint, SurfaceLint};
use crate::{Error, TOOL};

#[derive(Debug, Default, PartialEq, Eq)]
pub struct AllowList {
    /// The lines above the entries, as written.
    header: Vec<String>,
    entries: Vec<Entry>,
    /// The comment lines below the last entry.
    trailer: Vec<String>,
    /// Each line whose key an entry above it has, which a merge by union
    /// can leave: it is merged into that entry.
    duplicates: Vec<Entry>,
}

/// One line of the allowlist, with the comment lines above it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    lint: Lint,
    /// A diagnostic's key, or for a group `prefix::*`.
    key: String,
    /// The most it allows of what the diagnostic measures, or of a group's
    /// diagnostics.
    ceiling: Option<usize>,
    reason: Option<String>,
    comments: Vec<String>,
}

/// How an allowlist and the diagnostics of a run meet. Entries are by
/// their place in the list, diagnostics by theirs in the run's.
#[derive(Debug, Default)]
pub struct Audit {
    /// By diagnostic, the entry that allows it.
    pub allowed: Vec<Option<usize>>,
    /// By diagnostic, an entry that would allow it but for its ceiling, and
    /// what the diagnostic measures against it.
    pub exceeded: Vec<Option<(usize, usize)>>,
    /// Entries that allow nothing.
    pub stale: Vec<usize>,
    /// Entries whose ceiling is above all they allow, with what it can be.
    pub lowerable: Vec<(usize, usize)>,
    /// Entries that allow a diagnostic with another key: a smaller circle,
    /// or one that moved. By entry, the key it can take.
    pub renamed: Vec<(usize, String)>,
    /// Entries that allow a diagnostic they moved to.
    pub moved: Vec<usize>,
}

/// What to change in an allowlist.
#[derive(Debug, Default)]
pub struct Edit<'a> {
    /// Entries to take out.
    pub removed: Vec<usize>,
    /// Entries to give another key.
    pub renamed: Vec<(usize, String)>,
    /// Entries to give a lower ceiling.
    pub lowered: Vec<(usize, usize)>,
    /// Diagnostics to add an entry for.
    pub added: Vec<&'a Diagnostic>,
    /// Each added entry's ceiling is what its diagnostic measures.
    pub ceilings: bool,
}

/// The default header of a new allowlist.
fn default_header() -> Vec<String> {
    vec![
        format!("# Existing architectural debt suppressed by `{TOOL} --allow`."),
        "# Fixing an entry here never fails CI; new issues not listed here fail.".to_owned(),
    ]
}

impl AllowList {
    pub fn read(path: &Path) -> Result<AllowList, Error> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| Error::new(format!("{}: {e}", path.display())))?;
        AllowList::parse(&text)
            .map_err(|(line, why)| Error::new(format!("{}:{line}: {why}", path.display())))
    }

    /// The allowlist at `path`, or a new, empty one if there is none.
    pub fn read_or_new(path: &Path) -> Result<AllowList, Error> {
        if path.exists() {
            AllowList::read(path)
        } else {
            Ok(AllowList {
                header: default_header(),
                ..AllowList::default()
            })
        }
    }

    pub fn write(&self, path: &Path) -> Result<(), Error> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::new(format!("{}: {e}", parent.display())))?;
        }
        std::fs::write(path, self.to_text())
            .map_err(|e| Error::new(format!("{}: {e}", path.display())))
    }

    /// The allowlist `text` writes, or the 1-based line it cannot read and
    /// why.
    pub fn parse(text: &str) -> Result<AllowList, (usize, Error)> {
        let mut list = AllowList::default();
        // Comment lines since the last entry or blank line.
        let mut comments: Vec<String> = Vec::new();
        let mut before_entries: Vec<String> = Vec::new();
        for (index, line) in text.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with('#') {
                comments.push(trimmed.to_owned());
                continue;
            }
            if trimmed.is_empty() {
                if list.entries.is_empty() {
                    before_entries.append(&mut comments);
                    before_entries.push(String::new());
                } else {
                    // A blank line keeps a comment block with the next entry.
                    list.trailer.append(&mut comments);
                }
                continue;
            }
            if list.entries.is_empty() {
                // Above the first entry, a header without a blank line
                // after it is all comments: today's files have no blank.
                if before_entries.is_empty() {
                    before_entries.append(&mut comments);
                }
                list.header = std::mem::take(&mut before_entries);
            }
            let mut above = std::mem::take(&mut list.trailer);
            above.append(&mut comments);
            list.entries
                .push(Entry::parse(trimmed, above).map_err(|why| (index + 1, why))?);
        }
        if list.entries.is_empty() {
            before_entries.append(&mut comments);
            list.header = before_entries;
        } else {
            list.trailer.append(&mut comments);
        }
        list.sort();
        Ok(list)
    }

    pub fn to_text(&self) -> String {
        let mut lines = self.header.clone();
        for entry in &self.entries {
            lines.extend(entry.lines());
        }
        lines.extend(self.trailer.iter().cloned());
        lines.iter().map(|line| format!("{line}\n")).collect()
    }

    /// How many entries it has.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// The entry at `index`, as its line writes it.
    pub fn line(&self, index: usize) -> String {
        self.entries[index].to_string()
    }

    /// Whether the key of the entry at `index` names something under
    /// `prefix`, `crate::module::`: it or one of its members.
    pub fn names(&self, index: usize, prefix: &str) -> bool {
        self.entries[index].names(prefix)
    }

    /// The lines whose key an entry above them has, as written, of those
    /// whose keys name something under `prefix` if given.
    pub fn duplicates(&self, prefix: Option<&str>) -> Vec<String> {
        self.duplicates
            .iter()
            .filter(|entry| prefix.is_none_or(|prefix| entry.names(prefix)))
            .map(Entry::to_string)
            .collect()
    }

    /// Sorts the entries, and merges those with one key, which a merge by
    /// union can leave: the lower ceiling holds.
    fn sort(&mut self) {
        self.entries.sort_by(Entry::order);
        let duplicates = &mut self.duplicates;
        self.entries.dedup_by(|later, kept| {
            let same = later.order(kept) == Ordering::Equal;
            if same {
                duplicates.push(later.clone());
                kept.merge(later);
            }
            same
        });
    }

    /// How this allowlist meets `diagnostics`, all of a run's. With
    /// `follow_moves`, a stale entry allows a diagnostic that nothing else
    /// does and that it [moved to](Entry::moved_to).
    pub fn audit(&self, diagnostics: &[Diagnostic], follow_moves: bool) -> Audit {
        let mut audit = Audit {
            allowed: vec![None; diagnostics.len()],
            exceeded: vec![None; diagnostics.len()],
            ..Audit::default()
        };
        // By entry, what each diagnostic it allows measures.
        let mut met: BTreeMap<usize, Vec<Option<usize>>> = BTreeMap::new();
        let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for (at, diagnostic) in diagnostics.iter().enumerate() {
            let found = self
                .position(|entry| entry.is(diagnostic))
                .or_else(|| self.position(|entry| entry.holds_circle(diagnostic)));
            // Every other entry that holds the circle allows it too.
            for (index, entry) in self.entries.iter().enumerate() {
                if Some(index) != found && entry.holds_circle(diagnostic) {
                    met.entry(index).or_default().push(None);
                }
            }
            if let Some(index) = found {
                let entry = &self.entries[index];
                let measure = diagnostic.measure();
                met.entry(index).or_default().push(measure);
                if entry.allows(measure) {
                    audit.allowed[at] = Some(index);
                } else {
                    audit.exceeded[at] = measure.map(|measure| (index, measure));
                }
                if !entry.is(diagnostic) {
                    audit.renamed.push((index, diagnostic.key.clone()));
                }
            } else if let Some(index) = self.position(|entry| entry.holds_in_group(diagnostic)) {
                groups.entry(index).or_default().push(at);
            }
        }
        // A report of many, as of a struct's fields, counts each.
        for (index, members) in groups {
            let count = members
                .iter()
                .map(|&at| diagnostics[at].measure().unwrap_or(1))
                .sum();
            let allows = self.entries[index].allows(Some(count));
            for at in members {
                if allows {
                    audit.allowed[at] = Some(index);
                } else {
                    audit.exceeded[at] = Some((index, count));
                }
            }
            met.insert(index, vec![Some(count)]);
        }
        if follow_moves {
            self.follow_moves(diagnostics, &mut audit, &mut met);
        }
        // A circle that split into several keeps its key.
        let mut renames: BTreeMap<usize, usize> = BTreeMap::new();
        for (index, _) in &audit.renamed {
            *renames.entry(*index).or_default() += 1;
        }
        audit.renamed.retain(|(index, _)| renames[index] == 1);
        for (index, entry) in self.entries.iter().enumerate() {
            match met.get(&index) {
                None => audit.stale.push(index),
                Some(measures) => audit.lowerable.extend(
                    measures
                        .iter()
                        .copied()
                        .collect::<Option<Vec<usize>>>()
                        .and_then(|measures| entry.lowered_to(measures.into_iter().max()?))
                        .map(|most| (index, most)),
                ),
            }
        }
        audit
    }

    /// Lets each entry that allows nothing allow the first diagnostic that
    /// nothing allows and that it moved to.
    fn follow_moves(
        &self,
        diagnostics: &[Diagnostic],
        audit: &mut Audit,
        met: &mut BTreeMap<usize, Vec<Option<usize>>>,
    ) {
        for (index, entry) in self.entries.iter().enumerate() {
            if met.contains_key(&index) {
                continue;
            }
            let Some(at) = (0..diagnostics.len()).find(|&at| {
                audit.allowed[at].is_none()
                    && audit.exceeded[at].is_none()
                    && entry.moved_to(&diagnostics[at])
            }) else {
                continue;
            };
            let measure = diagnostics[at].measure();
            met.insert(index, vec![measure]);
            if entry.allows(measure) {
                audit.allowed[at] = Some(index);
            } else {
                audit.exceeded[at] = measure.map(|measure| (index, measure));
            }
            audit.renamed.push((index, diagnostics[at].key.clone()));
            audit.moved.push(index);
        }
    }

    fn position(&self, wanted: impl Fn(&Entry) -> bool) -> Option<usize> {
        self.entries.iter().position(wanted)
    }

    /// The allowlist with `edit` made, and a line for each change it makes:
    /// `- entry`, `~ entry -> entry`, `+ entry`.
    pub fn edited(&self, edit: &Edit) -> (AllowList, Vec<String>) {
        let mut changes: Vec<String> = self
            .duplicates
            .iter()
            .map(|entry| format!("- {entry} (listed twice)"))
            .collect();
        let mut entries = Vec::new();
        for (index, entry) in self.entries.iter().enumerate() {
            if edit.removed.contains(&index) {
                changes.push(format!("- {entry}"));
                continue;
            }
            let changed = entry.changed(
                edit.renamed
                    .iter()
                    .find(|(at, _)| *at == index)
                    .map(|(_, key)| key.as_str()),
                edit.lowered
                    .iter()
                    .find(|(at, _)| *at == index)
                    .map(|&(_, most)| most),
            );
            if changed != *entry {
                changes.push(format!("~ {entry} -> {changed}"));
            }
            entries.push(changed);
        }
        for diagnostic in &edit.added {
            let entry = Entry::of(diagnostic, edit.ceilings);
            changes.push(format!("+ {entry}"));
            entries.push(entry);
        }
        let mut list = AllowList {
            header: self.header.clone(),
            entries,
            trailer: self.trailer.clone(),
            duplicates: Vec::new(),
        };
        list.sort();
        (list, changes)
    }
}

impl Entry {
    /// A new entry for `diagnostic`, with its measure as its ceiling when
    /// `ceilings`.
    fn of(diagnostic: &Diagnostic, ceilings: bool) -> Entry {
        Entry {
            lint: diagnostic.lint,
            key: diagnostic.key.clone(),
            ceiling: diagnostic.measure().filter(|_| ceilings),
            reason: None,
            comments: Vec::new(),
        }
    }

    /// The entry `line` writes, with the `comments` above it.
    fn parse(line: &str, comments: Vec<String>) -> Result<Entry, Error> {
        let invalid = || Error::new("invalid allowlist entry");
        let (rest, reason) = match line.split_once(" # ") {
            Some((rest, reason)) => (rest.trim(), Some(reason.trim().to_owned())),
            None => (line, None),
        };
        let (rest, ceiling) = match rest.rsplit_once(" <= ") {
            Some((rest, number)) if number.trim().parse::<usize>().is_ok() => {
                (rest.trim(), number.trim().parse::<usize>().ok())
            }
            _ => (rest, None),
        };
        let (lint_name, key) = rest.split_once(' ').ok_or_else(invalid)?;
        let lint = Lint::parse(lint_name).ok_or_else(invalid)?;
        let key = key.trim();
        // What reads as a ceiling or a reason, written wrong, is no key.
        // A raw name, `r#type`, keeps its `#`.
        if key.is_empty() || key.contains("<=") || key.contains(" #") {
            return Err(invalid());
        }
        let entry = Entry {
            lint,
            key: key.to_owned(),
            ceiling,
            reason,
            comments,
        };
        if entry.group().is_some() && ceiling.is_none() {
            return Err(Error::new(
                "a group key, `prefix::*`, needs a ceiling: `<= N`",
            ));
        }
        Ok(entry)
    }

    /// Takes in `other`, an entry with its key: the lower ceiling holds, and
    /// its reason if it has none, and its comments go below its own.
    fn merge(&mut self, other: &mut Entry) {
        self.ceiling = match (self.ceiling, other.ceiling) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        self.reason = self.reason.take().or(other.reason.take());
        // A merge by union writes the comments above a line twice too.
        for comment in std::mem::take(&mut other.comments) {
            if !self.comments.contains(&comment) {
                self.comments.push(comment);
            }
        }
    }

    /// It with another `key` or a lower `ceiling`, each if given.
    fn changed(&self, key: Option<&str>, ceiling: Option<usize>) -> Entry {
        Entry {
            key: key.map_or_else(|| self.key.clone(), str::to_owned),
            ceiling: ceiling.or(self.ceiling),
            ..self.clone()
        }
    }

    /// Whether its key names something under `prefix`, `crate::module::`:
    /// it or one of its members.
    fn names(&self, prefix: &str) -> bool {
        self.key
            .split(',')
            .any(|member| member.starts_with(prefix) || prefix == format!("{member}::"))
    }

    /// Its comment lines, then its own.
    fn lines(&self) -> impl Iterator<Item = String> + '_ {
        self.comments
            .iter()
            .cloned()
            .chain(std::iter::once(self.to_string()))
    }

    /// The allowlist's order: by lint, then key.
    fn order(&self, other: &Entry) -> Ordering {
        (self.lint, &self.key).cmp(&(other.lint, &other.key))
    }

    /// Whether its key is `diagnostic`'s.
    fn is(&self, diagnostic: &Diagnostic) -> bool {
        self.lint == diagnostic.lint && self.key == diagnostic.key
    }

    /// Whether it allows a diagnostic that measures `measure`.
    fn allows(&self, measure: Option<usize>) -> bool {
        match (self.ceiling, measure) {
            (Some(ceiling), Some(measure)) => measure <= ceiling,
            _ => true,
        }
    }

    /// The ceiling it can take when it allows no more than `most`, if that
    /// is lower than its own.
    fn lowered_to(&self, most: usize) -> Option<usize> {
        self.ceiling.filter(|&ceiling| most < ceiling).map(|_| most)
    }

    /// For a group entry, `over-public crate::module::* <= N`, the prefix
    /// of the keys it holds: `crate::module::`. Only `over-public` and
    /// `test-only-pub` have groups; another lint's `::*` key is a glob's.
    fn group(&self) -> Option<&str> {
        matches!(
            self.lint,
            Lint::Surface(SurfaceLint::OverPublic | SurfaceLint::TestOnlyPub)
        )
        .then(|| self.key.strip_suffix('*'))
        .flatten()
        .filter(|prefix| prefix.ends_with("::"))
    }

    /// Whether this entry allows `diagnostic`, a circle or duplicate group
    /// whose members are among its own: shrinking an allowed circle is an
    /// improvement, never a new error.
    fn holds_circle(&self, diagnostic: &Diagnostic) -> bool {
        let allowed = members(&self.key);
        self.lint == diagnostic.lint
            && is_circle(self.lint)
            && members(&diagnostic.key)
                .iter()
                .all(|member| allowed.contains(member))
    }

    /// Whether this is a group entry, `prefix::*`, that `diagnostic` is in.
    fn holds_in_group(&self, diagnostic: &Diagnostic) -> bool {
        self.lint == diagnostic.lint
            && self
                .group()
                .is_some_and(|prefix| diagnostic.key.starts_with(prefix))
    }

    /// Whether `diagnostic` is this stale entry's after its code moved:
    /// the same lint, crate, last name and, a member, type; or for a circle
    /// one no bigger that shares more than half its members with it.
    fn moved_to(&self, diagnostic: &Diagnostic) -> bool {
        if self.lint != diagnostic.lint || self.group().is_some() {
            return false;
        }
        if is_circle(self.lint) {
            let old = members(&self.key);
            let new = members(&diagnostic.key);
            let shared = new.iter().filter(|member| old.contains(member)).count();
            return new.len() <= old.len() && shared * 2 > new.len();
        }
        !self.key.contains(" -> ") && ends(&self.key) == ends(&diagnostic.key)
    }
}

impl std::fmt::Display for Entry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.lint.name(), self.key)?;
        if let Some(ceiling) = self.ceiling {
            write!(f, " <= {ceiling}")?;
        }
        if let Some(reason) = &self.reason {
            write!(f, "  # {reason}")?;
        }
        Ok(())
    }
}

/// What a symbol's key keeps when its code moves: its crate, last name
/// and, for a member, its type's name. A file's or directory's key, with no
/// `::`, keeps all of itself: which file took a split file's debt is no
/// one's guess.
fn ends(key: &str) -> Vec<&str> {
    let parts: Vec<&str> = key.split("::").collect();
    let mut ends = vec![parts[0]];
    if let [.., parent, last] = &parts[1..] {
        if parent.starts_with(|c: char| c.is_ascii_uppercase()) {
            ends.push(*parent);
        }
        ends.push(*last);
    } else {
        ends.extend(parts.last());
    }
    ends
}

/// Whether `lint`'s keys list a circle's or group's members.
fn is_circle(lint: Lint) -> bool {
    matches!(
        lint,
        Lint::Graph(GraphLint::Tangle | GraphLint::ModuleCycle)
            | Lint::Hygiene(HygieneLint::DuplicateCode | HygieneLint::DuplicateStruct)
    )
}

/// The members a circle's key lists. A member's own commas are inside
/// brackets, as in `<(A, B) as Trait>::run`, and split nothing.
fn members(key: &str) -> Vec<&str> {
    let mut members = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    for (at, c) in key.char_indices() {
        match c {
            '<' | '(' | '[' => depth += 1,
            '>' | ')' | ']' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                members.push(&key[start..at]);
                start = at + 1;
            }
            _ => {}
        }
    }
    members.push(&key[start..]);
    members
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lint::{Picture, SizeLint};

    fn tangle(key: &str) -> Diagnostic {
        Diagnostic {
            lint: Lint::Graph(GraphLint::Tangle),
            key: key.into(),
            message: String::new(),
            location: None,
            notes: vec![],
            picture: Picture::Circle {
                steps: vec![],
                rest: None,
            },
        }
    }

    fn long(key: &str, lines: usize) -> Diagnostic {
        Diagnostic {
            lint: Lint::Size(SizeLint::LongFunction),
            picture: Picture::Bar {
                label: "lines".into(),
                length: lines,
                most: 150,
            },
            ..tangle(key)
        }
    }

    fn allowed(list: &AllowList, diagnostic: Diagnostic) -> bool {
        list.audit(&[diagnostic], false).allowed[0].is_some()
    }

    #[test]
    fn shrinking_an_allowed_tangle_stays_allowed() {
        let allow = AllowList::parse("tangle a::X,a::Y,b::Z\n").unwrap();
        assert!(allowed(&allow, tangle("a::X,b::Z")));
        assert!(!allowed(&allow, tangle("a::W,a::X,a::Y,b::Z")));
        assert_eq!(allow.audit(&[tangle("a::X,a::Y")], false).stale.len(), 0);
        assert_eq!(allow.audit(&[], false).stale.len(), 1);
        // Two entries that hold one circle both allow it: neither is stale.
        let both = AllowList::parse(
            "tangle a::X,a::Y
tangle a::X,a::Y,b::Z
",
        )
        .unwrap();
        assert!(both.audit(&[tangle("a::X,a::Y")], false).stale.is_empty());
    }

    #[test]
    fn shrinking_an_allowed_module_cycle_stays_allowed() {
        let cycle = |key: &str| Diagnostic {
            lint: Lint::Graph(GraphLint::ModuleCycle),
            ..tangle(key)
        };
        let allow = AllowList::parse("module-cycle a::x,a::y,a::z\n").unwrap();
        assert!(allowed(&allow, cycle("a::x,a::z")));
        assert!(!allowed(&allow, cycle("a::w,a::x")));
        assert!(!allowed(&allow, tangle("a::x,a::z")));
    }

    #[test]
    fn a_comma_inside_a_members_name_splits_nothing() {
        let allow = AllowList::parse("tangle a::<(A, B) as T>::run,a::X,a::Y\n").unwrap();
        assert!(allowed(&allow, tangle("a::<(A, B) as T>::run,a::Y")));
        assert!(!allowed(&allow, tangle("a::<(A, C) as T>::run,a::Y")));
        assert!(!allowed(&allow, tangle("a::Y,a::Z")));
    }

    #[test]
    fn a_removed_lint_is_an_invalid_entry() {
        assert_eq!(
            AllowList::parse("field-leak a::T::x\nchatty-seam a -> b\n")
                .map(|_| ())
                .map_err(|(line, _)| line),
            Err(2)
        );
        // A ceiling or reason written wrong is no part of a key.
        for line in [
            "long-function a::f <=3",
            "long-function a::f <= three",
            "long-function a::f #reason",
        ] {
            assert!(AllowList::parse(line).is_err(), "{line}");
        }
    }

    #[test]
    fn a_key_listed_twice_keeps_its_lower_ceiling() {
        // As a merge by union writes it: one branch lowered the ceiling.
        let text = "long-function a::f <= 300  # why
long-function a::f <= 250
                    long-function a::g <= 40
long-function a::g
";
        let allow = AllowList::parse(text).unwrap();
        assert_eq!(
            allow.to_text(),
            "long-function a::f <= 250  # why
long-function a::g <= 40
"
        );
        assert!(!allowed(&allow, long("a::f", 260)));
        // Ceilings and reasons aside, the key is what makes a line twice.
        assert_eq!(
            allow.duplicates(None),
            ["long-function a::f <= 250", "long-function a::g"]
        );
        let (edited, changes) = allow.edited(&Edit::default());
        assert!(edited.duplicates(None).is_empty());
        assert_eq!(allow.duplicates(Some("b::")), Vec::<String>::new());
        // The comments above a line are written twice too.
        let twice = "long-function a::a\n# hot\nlong-function a::h\n# hot\nlong-function a::h\n";
        assert_eq!(
            AllowList::parse(twice).unwrap().to_text(),
            "long-function a::a\n# hot\nlong-function a::h\n"
        );
        assert_eq!(
            changes,
            [
                "- long-function a::f <= 250 (listed twice)",
                "- long-function a::g (listed twice)"
            ]
        );
    }

    #[test]
    fn a_file_without_ceilings_or_reasons_reads_and_writes_as_before() {
        let text = format!(
            "{}\nlong-function a::f\nlayer-violation low -> high\n",
            default_header().join("\n")
        );
        let allow = AllowList::parse(&text).unwrap();
        assert!(allowed(&allow, long("a::f", 9_999)));
        assert_eq!(
            allow.to_text(),
            format!(
                "{}\nlayer-violation low -> high\nlong-function a::f\n",
                default_header().join("\n")
            )
        );
    }

    #[test]
    fn a_ceiling_allows_up_to_it_and_a_reason_and_comments_stay_with_their_entry() {
        let text = "# Debt.\n\nlong-function a::b\n# Settled: the planner is one walk.\n\
                    long-function a::plan <= 300  # a reason\n# End.\n";
        let allow = AllowList::parse(text).unwrap();
        assert!(allowed(&allow, long("a::plan", 300)));
        let audit = allow.audit(&[long("a::plan", 301)], false);
        assert_eq!(
            (audit.allowed[0], audit.exceeded[0]),
            (None, Some((1, 301)))
        );
        assert_eq!(
            allow.audit(&[long("a::plan", 250)], false).lowerable,
            [(1, 250)]
        );
        assert_eq!(allow.to_text(), text);
        let edit = Edit {
            removed: vec![0],
            ..Edit::default()
        };
        let (removed, changes) = allow.edited(&edit);
        assert_eq!(changes, ["- long-function a::b"]);
        assert_eq!(
            removed.to_text(),
            "# Debt.\n\n# Settled: the planner is one walk.\n\
             long-function a::plan <= 300  # a reason\n# End.\n"
        );
    }

    #[test]
    fn a_group_key_allows_up_to_its_ceiling_of_its_lints_and_needs_one() {
        let over = |key: &str| Diagnostic {
            lint: Lint::Surface(SurfaceLint::OverPublic),
            ..tangle(key)
        };
        let allow = AllowList::parse("over-public a::m::* <= 2\n").unwrap();
        let two = allow.audit(
            &[over("a::m::x"), over("a::m::y::z"), over("a::n::x")],
            false,
        );
        assert_eq!(two.allowed, [Some(0), Some(0), None]);
        let three = allow.audit(&[over("a::m::x"), over("a::m::y"), over("a::m::z")], false);
        assert_eq!(three.allowed, [None, None, None]);
        assert_eq!(three.exceeded[0], Some((0, 3)));
        assert_eq!(allow.audit(&[over("a::m::x")], false).lowerable, [(0, 1)]);
        // One report of a struct's three fields counts three.
        let fields = Diagnostic {
            picture: crate::lint::Picture::Bar {
                label: "fields".into(),
                length: 3,
                most: 0,
            },
            ..over("a::m::S.fields")
        };
        assert_eq!(allow.audit(&[fields], false).exceeded[0], Some((0, 3)));
        assert!(AllowList::parse("over-public a::m::*\n").is_err());
        // Another lint's `::*` is a glob re-export's key.
        let glob = Diagnostic {
            lint: Lint::Encapsulation(crate::lint::EncapsulationLint::PassThrough),
            ..tangle("a::m::*")
        };
        let globs = AllowList::parse("pass-through a::m::*\n").unwrap();
        assert!(allowed(&globs, glob.clone()));
        assert!(!allowed(
            &globs,
            Diagnostic {
                key: "a::m::x".into(),
                ..glob
            }
        ));
    }

    #[test]
    fn a_stale_entry_moved_to_a_diagnostic_with_its_crate_and_last_name() {
        let entry = Entry::parse("long-function a::old::plan <= 300", Vec::new()).unwrap();
        assert!(entry.moved_to(&long("a::new::plan", 10)));
        assert!(!entry.moved_to(&long("b::old::plan", 10)));
        assert!(!entry.moved_to(&long("a::old::plans", 10)));
        let method = Entry::parse("long-function a::old::Plan::run", Vec::new()).unwrap();
        assert!(method.moved_to(&long("a::new::Plan::run", 10)));
        assert!(!method.moved_to(&long("a::old::Walk::run", 10)));
        let file = Entry::parse("long-file crates/a/src/plan.rs", Vec::new()).unwrap();
        let long_file = |key: &str| Diagnostic {
            lint: Lint::Size(SizeLint::LongFile),
            ..long(key, 3_000)
        };
        assert!(!file.moved_to(&long_file("crates/a/src/planner.rs")));
        assert!(!file.moved_to(&long_file("crates/b/src/plan.rs")));
        let circle = Entry::parse("tangle a::W,a::X,a::Y,a::Z", Vec::new()).unwrap();
        assert!(circle.moved_to(&tangle("a::X,a::Y,a::V")));
        assert!(!circle.moved_to(&tangle("a::X,a::V,a::U")));
        assert!(!circle.moved_to(&tangle("a::W,a::X,a::Y,a::Z,a::V")));
    }
}
