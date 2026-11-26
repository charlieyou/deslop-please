//! What a run does with an allowlist: `--allow` reports what it does not
//! allow, `--update-allow` writes what a run reports into it, and
//! `--prune-allow` takes out of it what a run no longer reports. A run
//! with a crate or module reads and changes only the entries in its scope.

use std::collections::BTreeSet;
use std::path::Path;

use crate::allow::{AllowList, Audit, Edit};
use crate::format::Tighter;
use crate::lint::{Diagnostic, Lint};
use crate::settings::Allow;
use crate::{Error, TOOL};

/// A run's diagnostics, and which of them its scope shows.
pub struct Scoped<'a> {
    /// The whole workspace's.
    pub all: &'a [Diagnostic],
    /// Those in the run's scope, by lint and key.
    pub shown: &'a BTreeSet<(Lint, String)>,
    /// `crate::module::` for a run with a scope.
    pub prefix: Option<String>,
}

impl Scoped<'_> {
    pub fn shows(&self, diagnostic: &Diagnostic) -> bool {
        self.shown
            .contains(&(diagnostic.lint, diagnostic.key.clone()))
    }

    /// Whether the run's scope holds the entry at `index`: what it allows,
    /// or, allowing nothing, what its key names.
    fn holds(&self, allow: &AllowList, audit: &Audit, index: usize) -> bool {
        let Some(prefix) = &self.prefix else {
            return true;
        };
        let mut matched = self
            .all
            .iter()
            .enumerate()
            .filter(|&(at, _)| {
                audit.allowed[at] == Some(index)
                    || audit.exceeded[at].is_some_and(|(entry, _)| entry == index)
            })
            .peekable();
        if matched.peek().is_none() {
            return allow.names(index, prefix);
        }
        matched.any(|(_, diagnostic)| self.shows(diagnostic))
    }

    /// The diagnostics in scope that `allow` does not allow, each above its
    /// entry's ceiling with a note that says so.
    pub fn reported(&self, allow: &AllowList, audit: &Audit, path: &Path) -> Vec<Diagnostic> {
        self.all
            .iter()
            .enumerate()
            .filter(|&(at, diagnostic)| audit.allowed[at].is_none() && self.shows(diagnostic))
            .map(|(at, diagnostic)| {
                let mut diagnostic = diagnostic.clone();
                if let Some((index, measure)) = audit.exceeded[at] {
                    diagnostic.notes.push(format!(
                        "{}: it is {measure}, above `{}`",
                        path.display(),
                        allow.line(index)
                    ));
                }
                diagnostic
            })
            .collect()
    }

    /// What `--prune-allow` changes in `allow`: in scope, each entry that
    /// allows nothing, each ceiling above what it allows, and each entry
    /// that moved or, a circle, shrank.
    fn prune<'a>(&self, allow: &AllowList, audit: &Audit) -> Edit<'a> {
        let holds = |index: usize| self.holds(allow, audit, index);
        Edit {
            removed: audit.stale.iter().copied().filter(|&i| holds(i)).collect(),
            renamed: audit
                .renamed
                .iter()
                .filter(|(index, _)| holds(*index))
                .cloned()
                .collect(),
            lowered: audit
                .lowerable
                .iter()
                .copied()
                .filter(|&(index, _)| holds(index))
                .collect(),
            ..Edit::default()
        }
    }
}

/// What `--prune-allow` would change in `allow`: what `--strict-allow`
/// fails on. A circle that shrank is no failure.
pub fn tighter(allow: &AllowList, audit: &Audit, scoped: &Scoped) -> Tighter {
    let edit = scoped.prune(allow, audit);
    let lowered = edit
        .lowered
        .iter()
        .map(|&(index, most)| format!("{}: allows no more than {most}", allow.line(index)));
    let moved = edit
        .renamed
        .iter()
        .filter(|(index, _)| audit.moved.contains(index))
        .map(|(index, key)| format!("{}: moved to `{key}`", allow.line(*index)));
    Tighter {
        stale: edit
            .removed
            .iter()
            .map(|&index| allow.line(index))
            .collect(),
        tightened: lowered.chain(moved).collect(),
    }
}

/// `--update-allow`: the allowlist at `path` with the entries in scope that
/// allow nothing taken out, those that moved or shrank rewritten, and an
/// entry added for each diagnostic in scope that none allows. No ceiling
/// rises: a diagnostic above one fails the run.
pub fn update(path: &Path, scoped: &Scoped, settings: &Allow) -> Result<bool, Error> {
    let allow = AllowList::read_or_new(path)?;
    let audit = allow.audit(scoped.all, settings.follow_moves);
    let mut edit = Edit {
        lowered: Vec::new(),
        ceilings: settings.ceilings,
        ..scoped.prune(&allow, &audit)
    };
    let mut above = Vec::new();
    for (at, diagnostic) in scoped.all.iter().enumerate() {
        if !scoped.shows(diagnostic) {
            continue;
        }
        match (audit.allowed[at], audit.exceeded[at]) {
            (None, None) => edit.added.push(diagnostic),
            (_, Some((index, measure))) => above.push(format!(
                "{TOOL}: `{}` is {measure}, above `{}`; no update raises a ceiling",
                diagnostic.key,
                allow.line(index)
            )),
            _ => {}
        }
    }
    write(&allow, &edit, path)?;
    for line in &above {
        eprintln!("{line}");
    }
    Ok(above.is_empty())
}

/// `--prune-allow`: the allowlist at `path` with the entries in scope that
/// allow nothing taken out, each ceiling lowered to what it allows, and
/// those that moved or shrank rewritten. It adds nothing.
pub fn prune(path: &Path, scoped: &Scoped, settings: &Allow) -> Result<bool, Error> {
    let allow = AllowList::read(path)?;
    let audit = allow.audit(scoped.all, settings.follow_moves);
    write(&allow, &scoped.prune(&allow, &audit), path)?;
    Ok(true)
}

/// Writes `allow` with `edit` made to `path`, and prints each change.
fn write(allow: &AllowList, edit: &Edit, path: &Path) -> Result<(), Error> {
    let (edited, changes) = allow.edited(edit);
    edited.write(path)?;
    for change in changes {
        eprintln!("{change}");
    }
    eprintln!(
        "{TOOL}: wrote {} entries to {}",
        edited.len(),
        path.display()
    );
    Ok(())
}
