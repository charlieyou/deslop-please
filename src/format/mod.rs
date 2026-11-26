//! `rustc`/`clippy`-style diagnostic formatting, in color when stdout is a
//! terminal, with a picture under each report.

mod picture;

use std::fmt::Write;
use std::io::IsTerminal;

use crate::TOOL;
use crate::lint::Diagnostic;

/// What `--prune-allow` would change in an allowlist, a line for each
/// entry: what `--strict-allow` fails on.
#[derive(Debug, Default)]
pub struct Tighter {
    /// Each entry that allows nothing, as written.
    pub stale: Vec<String>,
    /// Each entry whose ceiling is above what it allows, or that moved, and
    /// how.
    pub tightened: Vec<String>,
}

impl Tighter {
    pub fn is_empty(&self) -> bool {
        self.stale.is_empty() && self.tightened.is_empty()
    }
}

pub fn render(
    diagnostics: &[Diagnostic],
    tighter: &Tighter,
    allow_path: Option<&str>,
    strict_allow: bool,
) -> String {
    let paint = Paint(stdout_has_color());
    let mut out = String::new();
    for diag in diagnostics {
        let _ = writeln!(
            out,
            "{}{}",
            paint.error(&format!("error[{}]", diag.lint.name())),
            paint.bold(&format!(": {}", diag.message))
        );
        if let Some(loc) = &diag.location {
            // rustc shape (`file:line:column`) so IDE terminals link-navigate.
            let _ = writeln!(out, "  {} {}:{}:1", paint.gutter("-->"), loc.file, loc.line);
        }
        for note in &diag.notes {
            let _ = writeln!(
                out,
                "  {} {} {note}",
                paint.gutter("="),
                paint.bold("note:")
            );
        }
        for line in picture::draw(&diag.picture, &paint) {
            let _ = writeln!(out, "{line}");
        }
        out.push('\n');
    }

    let lines: Vec<&String> = tighter.stale.iter().chain(&tighter.tightened).collect();
    let fixed_allowed = lines.len();
    if fixed_allowed > 0
        && let Some(path) = allow_path
    {
        let noun = if fixed_allowed == 1 {
            "entry"
        } else {
            "entries"
        };
        // Only stale entries: what `--update-allow` takes out, too.
        let (fixed, command) = if tighter.tightened.is_empty() {
            ("are now fixed", "--update-allow")
        } else {
            ("are now fixed or allow less", "--prune-allow")
        };
        if strict_allow {
            let _ = writeln!(
                out,
                "{}: {fixed_allowed} stale allowed {noun} in {path} (run `{TOOL} {command} {path}` to prune)",
                paint.error("error[stale-allow]")
            );
            for line in lines.iter().take(20) {
                let _ = writeln!(out, "  {} {line}", paint.gutter("-"));
            }
            if fixed_allowed > 20 {
                let _ = writeln!(
                    out,
                    "  {} ... and {} more",
                    paint.gutter("-"),
                    fixed_allowed - 20
                );
            }
        } else {
            let _ = writeln!(
                out,
                "{} {fixed_allowed} allowed {noun} in {path} {fixed} (run `{TOOL} {command} {path}` to tighten it)",
                paint.bold("note:")
            );
        }
    }

    let failed = !diagnostics.is_empty() || (strict_allow && fixed_allowed > 0);
    if !failed {
        let _ = writeln!(out, "{TOOL}: {}", paint.ok("ok"));
    } else {
        let total = diagnostics.len() + if strict_allow { fixed_allowed } else { 0 };
        let noun = if total == 1 { "error" } else { "errors" };
        let count = format!("{total} {noun}");
        let _ = writeln!(out, "{TOOL}: {}, well done!", paint.error(&count));
    }
    out
}

/// One `lint<TAB>key<TAB>file:line` line for each of `diagnostics`, with
/// an empty place for one that has none.
pub fn tsv(diagnostics: &[Diagnostic]) -> String {
    diagnostics
        .iter()
        .map(|diag| {
            let at = diag
                .location
                .as_ref()
                .map_or(String::new(), |loc| format!("{}:{}", loc.file, loc.line));
            format!("{}\t{}\t{at}\n", diag.lint.name(), diag.key)
        })
        .collect()
}

/// Like cargo: `CARGO_TERM_COLOR=always|never` decides, then `NO_COLOR`, then
/// whether stdout is a terminal.
fn stdout_has_color() -> bool {
    match std::env::var("CARGO_TERM_COLOR").as_deref() {
        Ok("always") => true,
        Ok("never") => false,
        _ => {
            std::env::var_os("NO_COLOR").is_none_or(|value| value.is_empty())
                && std::io::stdout().is_terminal()
        }
    }
}

/// rustc's colors: bold red errors, bold blue gutter, bold green success.
struct Paint(bool);

impl Paint {
    fn error(&self, text: &str) -> String {
        self.sgr("1;31", text)
    }

    fn gutter(&self, text: &str) -> String {
        self.sgr("1;34", text)
    }

    fn ok(&self, text: &str) -> String {
        self.sgr("1;32", text)
    }

    fn bold(&self, text: &str) -> String {
        self.sgr("1", text)
    }

    fn sgr(&self, code: &str, text: &str) -> String {
        if self.0 {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_owned()
        }
    }
}
