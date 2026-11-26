//! What the index leaves out, read from a file's text. The index says which
//! names a file writes and what each resolves to, but not the syntax around
//! them: which names are in `use` declarations, which type an `impl` block
//! is for, which modules are built only for tests, and whether a function's
//! body computes anything.

mod cfg;
mod mentions;
mod metrics;
mod operators;
mod tokens;
mod uses;

pub use mentions::TextPath;
pub use metrics::PubUse;
pub use uses::{Access, Plain};

use std::collections::HashSet;

use crate::model::{Codegen, FunctionMetrics, ItemKind};
use crate::scip::{Position, Range};
use tokens::{Kind, Token};

/// What holds a value and is peeled off a type to reach it: `Rc<RefCell<T>>` holds a `T`.
const HOLDERS: [&str; 8] = [
    "Option", "Cell", "RefCell", "Rc", "Arc", "Box", "Mutex", "RwLock",
];

/// Types and what a getter of one may hand out instead: what it dereferences
/// to, or the value an atomic holds. `Vec<T>` hands out `[T]` too.
const HANDED_OUT: [(&str, &str); 15] = [
    ("String", "str"),
    ("PathBuf", "Path"),
    ("OsString", "OsStr"),
    ("CString", "CStr"),
    ("AtomicBool", "bool"),
    ("AtomicI8", "i8"),
    ("AtomicI16", "i16"),
    ("AtomicI32", "i32"),
    ("AtomicI64", "i64"),
    ("AtomicIsize", "isize"),
    ("AtomicU8", "u8"),
    ("AtomicU16", "u16"),
    ("AtomicU32", "u32"),
    ("AtomicU64", "u64"),
    ("AtomicUsize", "usize"),
];

/// A file's text, read as tokens.
pub struct Source {
    text: String,
    line_starts: Vec<usize>,
    tokens: Vec<Token>,
}

/// File-wide syntax facts scanned once per file: `use` declaration ranges,
/// `pub use` / `pub(...) use` re-exports, and distinct identifier tokens.
pub struct FileScan {
    pub imports: Vec<Range>,
    /// In `use` declarations, each name before `::*`: a module whose items
    /// they all import.
    pub globs: Vec<Range>,
    pub pub_uses: Vec<PubUse>,
    pub idents: HashSet<String>,
    /// Paths written as text, where: in serde attributes and format strings.
    pub mentions: Vec<TextPath>,
    /// The traits that each `macro_rules!` writes an `impl` of, by the
    /// macro's name, and every macro invocation's name.
    pub macros: Macros,
}

/// What a file's `macro_rules!` definitions implement, and what it invokes.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Macros {
    /// By macro: each trait, by its last name, that its body writes
    /// `impl Trait for` of.
    pub impls: Vec<(String, String)>,
    /// The name of each macro invoked, once per invocation.
    pub invoked: Vec<String>,
}

/// Parsed pieces of an `impl` block header before its `{`.
struct ImplHeader<'a> {
    trait_type: Option<&'a [Token]>,
    self_type: &'a [Token],
    open: usize,
}

/// An `impl` block as its text writes it.
#[derive(Debug, PartialEq, Eq)]
pub struct ImplBlock {
    /// From `impl` to the closing brace.
    pub block: Range,
    /// The name of the type it is for, past `&`, `Box<..>` and the like:
    /// `Ready` in `impl Display for &Ready`. `None` for a tuple, slice or array.
    pub self_type: Option<Range>,
    /// For a trait `impl`, the name of the trait before `for`: `Display` in
    /// `impl fmt::Display for &Ready`. `None` for an inherent `impl`.
    pub trait_type: Option<Range>,
}

/// How a reference is written, as [`Source::written`] reads it.
pub struct Written {
    /// In its holder's signature: before a function's body or a
    /// constant's value, or anywhere in another item's definition.
    pub in_signature: bool,
    /// It names the type of a struct literal with a base: `T { a, ..base }`.
    pub struct_update: bool,
}

/// What a function's text says about it.
pub struct FunctionText {
    /// Its body has an arithmetic, comparison or logic operator, invokes a
    /// macro, or calls something other than one of its own parameters that
    /// does more than pass a value on.
    pub computes: bool,
    /// It takes `self` by value, so it gives or takes fields whole.
    pub whole: bool,
    /// It takes no `&mut self`, what it returns points nowhere, and its body
    /// overwrites nothing and calls no parameter: it can only return copies.
    pub returns_copies: bool,
    /// It takes `&self`, its body overwrites nothing and calls no
    /// parameter, and what it returns lends nothing to write: what it lends
    /// can only be read.
    pub lends_to_read: bool,
    /// What comes before the body, as [`normalized`] tokens.
    signature: Vec<String>,
}

impl FunctionText {
    /// Whether the signature writes one of `types`, each as [`Source::field_types`] gives it.
    pub fn writes_any(&self, types: &[Vec<String>]) -> bool {
        types.iter().any(|written| {
            !written.is_empty()
                && self
                    .signature
                    .windows(written.len())
                    .any(|window| window == written.as_slice())
        })
    }
}

impl Source {
    pub fn new(text: String) -> Source {
        let line_starts = std::iter::once(0)
            .chain(text.match_indices('\n').map(|(at, _)| at + 1))
            .collect();
        let tokens = tokens::read(&text);
        Source {
            text,
            line_starts,
            tokens,
        }
    }

    /// The name of the type `written` writes, past what holds it: `Ready` for
    /// `&'a Box<crate::Ready<T>>`.
    pub fn type_name(written: &str) -> Option<String> {
        let source = Source::new(written.to_owned());
        let name = source.name_of(&source.tokens)?;
        Some(source.text(name).to_owned())
    }

    /// The range of the whole text, which the index gives the file's module.
    pub fn whole(&self) -> Range {
        self.range(0, self.text.len())
    }

    /// File-wide `use` declarations, `pub use` / `pub(...) use` re-exports, and
    /// identifier tokens.
    pub fn scan_file(&self) -> FileScan {
        let tokens = &self.tokens;
        let mut imports = Vec::new();
        let mut globs = Vec::new();
        for (at, token) in tokens.iter().enumerate() {
            let precise_capture = tokens.get(at + 1).is_some_and(|next| self.is(*next, "<"));
            if !self.is(*token, "use") || precise_capture {
                continue;
            }
            let last = tokens[at..]
                .iter()
                .position(|t| self.is(*t, ";"))
                .map_or(tokens.len(), |end| at + end);
            // Each `*`, after `name::` or in a group, `name::{self, *}`.
            for star in at + 3..last {
                if !self.is(tokens[star], "*") {
                    continue;
                }
                // The `::` before the `*`, or before the group's `{`.
                let mut depth = 0usize;
                let colons = (at + 1..star).rev().find_map(|before| {
                    match self.text(tokens[before]) {
                        "::" if depth == 0 && before + 1 == star => return Some(before),
                        "}" => depth += 1,
                        "{" if depth == 0 => return Some(before - 1),
                        "{" => depth -= 1,
                        _ => {}
                    }
                    None
                });
                if let Some(colons) = colons.filter(|&colons| colons > at + 1)
                    && self.is(tokens[colons], "::")
                    && tokens[colons - 1].kind == Kind::Ident
                {
                    let name = tokens[colons - 1];
                    globs.push(self.range(name.start, name.end));
                }
            }
            let end = tokens.get(last).map_or(self.text.len(), |t| t.end);
            imports.push(self.range(token.start, end));
        }
        let pub_uses = metrics::pub_uses(&self.text, tokens, |start, end| self.range(start, end));
        let idents = metrics::collect_idents(&self.text, tokens);
        FileScan {
            imports,
            globs,
            pub_uses,
            idents,
            mentions: self.mentions(),
            macros: self.macros(),
        }
    }

    fn macros(&self) -> Macros {
        let tokens = &self.tokens;
        let mut macros = Macros::default();
        for (at, token) in tokens.iter().enumerate() {
            let bang = tokens.get(at + 1).is_some_and(|t| self.is(*t, "!"));
            if token.kind != Kind::Ident || !bang {
                continue;
            }
            let defines = self.is(*token, "macro_rules");
            match tokens.get(at + 2) {
                Some(name) if defines && name.kind == Kind::Ident => {
                    let Some(close) = tokens
                        .get(at + 3)
                        .filter(|t| t.kind == Kind::Open)
                        .and_then(|_| self.matching(at + 3))
                    else {
                        continue;
                    };
                    let body = &tokens[at + 3..close];
                    for (index, word) in body.iter().enumerate() {
                        if !self.is(*word, "impl") {
                            continue;
                        }
                        let header = body[index + 1..]
                            .iter()
                            .take_while(|t| !self.is(**t, "{") && !self.is(**t, ";"));
                        let before_for: Vec<&Token> =
                            header.take_while(|t| !self.is(**t, "for")).collect();
                        let has_for = body
                            .get(index + 1 + before_for.len())
                            .is_some_and(|t| self.is(*t, "for"));
                        // The trait's name: its last outside generic arguments.
                        let mut depth = 0;
                        let mut named = None;
                        for &part in &before_for {
                            match self.text(*part) {
                                "<" => depth += 1,
                                ">" => depth -= 1,
                                _ if depth == 0 && part.kind == Kind::Ident => named = Some(part),
                                _ => {}
                            }
                        }
                        if let Some(name_of_trait) = named.filter(|_| has_for) {
                            macros.impls.push((
                                self.text(*name).to_owned(),
                                self.text(*name_of_trait).to_owned(),
                            ));
                        }
                    }
                }
                Some(open) if !defines && open.kind == Kind::Open => {
                    macros.invoked.push(self.text(*token).to_owned());
                }
                _ => {}
            }
        }
        macros
    }

    /// How the name at `name` is written, inside the definition `holder` of
    /// an item of its kind, if it is.
    pub fn written(&self, name: Range, holder: Option<(Range, ItemKind)>) -> Written {
        Written {
            in_signature: holder
                .is_some_and(|(holder, kind)| self.in_signature(holder, kind, name)),
            struct_update: self.struct_update(name),
        }
    }

    /// Whether the name at `name` writes a struct literal with a base:
    /// `T { a: 1, ..base }`, `m::T::<u8> { ..base }`, but not the pattern
    /// `T { a, .. }`.
    fn struct_update(&self, name: Range) -> bool {
        let tokens = &self.tokens;
        let Some(offset) = self.offset(name.start) else {
            return false;
        };
        let mut at = tokens.partition_point(|token| token.start < offset) + 1;
        if tokens.get(at).is_some_and(|t| self.is(*t, "::"))
            && tokens.get(at + 1).is_some_and(|t| self.is(*t, "<"))
        {
            at = self.past_angles(at + 1).unwrap_or(tokens.len());
        }
        if !tokens.get(at).is_some_and(|t| self.is(*t, "{")) {
            return false;
        }
        let Some(close) = self.matching(at) else {
            return false;
        };
        let mut depth = 0;
        (at + 1..close).any(|index| {
            match tokens[index].kind {
                Kind::Open => depth += 1,
                Kind::Close => depth -= 1,
                _ => {}
            }
            depth == 0
                && self.is(tokens[index], "..")
                && [",", "{"].contains(&self.text(tokens[index - 1]))
                && index + 1 < close
        })
    }

    /// Whether `at`, inside the definition `holder` of an item of `kind`, is
    /// in its signature: before a function's body or a constant's value.
    fn in_signature(&self, holder: Range, kind: ItemKind, at: Range) -> bool {
        let Some(tokens) = self.tokens_in(holder) else {
            return false;
        };
        let end = match kind {
            ItemKind::Function => {
                if !tokens.last().is_some_and(|t| self.is(*t, "}")) {
                    return true;
                }
                let close = tokens.len() - 1;
                let mut depth = 0;
                let open = (0..=close).rev().find(|&at| {
                    match tokens[at].kind {
                        Kind::Close => depth += 1,
                        Kind::Open => depth -= 1,
                        _ => {}
                    }
                    depth == 0
                });
                match open {
                    Some(open) => tokens[open].start,
                    None => return true,
                }
            }
            ItemKind::Constant => {
                let mut depth = 0;
                let equals = tokens.iter().find(|token| {
                    match token.kind {
                        Kind::Open => depth += 1,
                        Kind::Close => depth -= 1,
                        _ => {}
                    }
                    depth == 0 && self.is(**token, "=")
                });
                match equals {
                    Some(equals) => equals.start,
                    None => return true,
                }
            }
            _ => return true,
        };
        at.start < self.position(end)
    }

    /// Every `impl` block, in order.
    pub fn impl_blocks(&self) -> Vec<ImplBlock> {
        let tokens = &self.tokens;
        let mut blocks = Vec::new();
        for (at, token) in tokens.iter().enumerate() {
            if !self.is(*token, "impl") || !self.starts_item(at) {
                continue;
            }
            let Some(header) = self.impl_header(at + 1) else {
                continue;
            };
            let close = self.matching(header.open).unwrap_or(tokens.len() - 1);
            blocks.push(ImplBlock {
                block: self.range(token.start, tokens[close].end),
                self_type: self
                    .name_of(header.self_type)
                    .map(|name| self.range(name.start, name.end)),
                trait_type: header
                    .trait_type
                    .and_then(|slice| self.name_of(slice))
                    .map(|name| self.range(name.start, name.end)),
            });
        }
        blocks
    }

    /// Structural info for a defined function: metrics plus the range of its
    /// first non-`self` parameter's type name, if any.
    pub fn function_info(&self, item: Range) -> Option<(FunctionMetrics, Option<Range>)> {
        let tokens = self.tokens_in(item)?;
        let fn_pos = tokens.iter().position(|t| self.is(*t, "fn"))?;
        let mut open = fn_pos + 2;
        if tokens.get(open).is_some_and(|t| self.is(*t, "<")) {
            open = self.past_angles_in(tokens, open);
        }
        if !tokens.get(open).is_some_and(|t| self.is(*t, "(")) {
            return None;
        }
        let close = self.matching_in(tokens, open)?;
        let param_tokens = &tokens[open + 1..close];
        let params = self.count_non_self_params(param_tokens);
        let first_type = self.first_param_type_in(param_tokens);
        let positional_trap = metrics::positional_trap(&self.text, param_tokens);

        let last = tokens.len().checked_sub(1)?;
        let body_open = if self.is(tokens[last], "}") {
            let mut depth = 0i32;
            (0..=last)
                .rev()
                .find(|&at| {
                    match tokens[at].kind {
                        Kind::Close => depth += 1,
                        Kind::Open => depth -= 1,
                        _ => {}
                    }
                    depth == 0
                })
                .filter(|&bo| bo > close)
        } else {
            None
        };
        let header_end = body_open.unwrap_or(tokens.len());
        let returns_string_error = close + 1 < header_end
            && tokens[close + 1..header_end]
                .iter()
                .position(|t| self.is(*t, "->"))
                .is_some_and(|arrow| {
                    let ret_start = close + 1 + arrow + 1;
                    let ret_end = tokens[ret_start..header_end]
                        .iter()
                        .position(|t| self.is(*t, "where"))
                        .map_or(header_end, |w| ret_start + w);
                    metrics::returns_string_error(&self.text, &tokens[ret_start..ret_end])
                });
        let body_tokens = body_open.map_or(&[][..], |bo| &tokens[bo + 1..last]);
        let (branches, body_fingerprint, decides) = match body_open {
            Some(_) => {
                // A number after `.` is a tuple field, as in `self.0`.
                let decides = body_tokens.iter().enumerate().any(|(at, t)| {
                    let field = at > 0 && self.is(body_tokens[at - 1], ".");
                    (t.kind == Kind::Literal && !field)
                        || matches!(self.text(*t), "if" | "match" | "true" | "false")
                });
                let branches = body_tokens
                    .iter()
                    .filter(|t| {
                        matches!(
                            self.text(**t),
                            "if" | "match" | "for" | "while" | "loop" | "&&" | "||"
                        )
                    })
                    .count() as u32;
                let fp = metrics::body_fingerprint(&self.text, param_tokens, body_tokens);
                (branches, fp, decides)
            }
            None => (0, None, false),
        };

        // From the first token past the attributes: doc comments are
        // comments, which are no tokens.
        let mut start = 0;
        while self.is(tokens[start], "#") {
            let Some(close) = tokens
                .get(start + 1)
                .filter(|t| self.is(**t, "["))
                .and_then(|_| self.matching_in(tokens, start + 1))
            else {
                break;
            };
            start = close + 1;
        }
        Some((
            FunctionMetrics {
                lines: metrics::code_lines(&self.text, &tokens[start..=last]),
                branches,
                params,
                returns_string_error,
                body_fingerprint,
                positional_trap,
                codegen: self.codegen(&tokens[..fn_pos]),
                decides,
            },
            first_type,
        ))
    }

    /// What the attributes among `tokens`, those before a function's `fn`,
    /// ask of its code generation.
    fn codegen(&self, tokens: &[Token]) -> Codegen {
        let mut codegen = Codegen::default();
        for (at, token) in tokens.iter().enumerate() {
            if !self.is(*token, "#") || !tokens.get(at + 1).is_some_and(|t| self.is(*t, "[")) {
                continue;
            }
            let words: Vec<&str> = tokens[at + 2..]
                .iter()
                .take(4)
                .map(|t| self.text(*t))
                .collect();
            match words.as_slice() {
                ["target_feature", ..] => codegen.target_feature = true,
                ["cold", ..] => codegen.cold = true,
                ["inline", "(", "always", ..] => codegen.inline_always = true,
                _ => {}
            }
        }
        codegen
    }

    fn first_param_type_in(&self, params: &[Token]) -> Option<Range> {
        let mut depth = 0i32;
        let end = params
            .iter()
            .position(|t| {
                match self.text(*t) {
                    "<" | "(" | "[" | "{" => depth += 1,
                    ">" | ")" | "]" | "}" => depth -= 1,
                    "," if depth == 0 => return true,
                    _ => {}
                }
                false
            })
            .unwrap_or(params.len());
        let first = &params[..end];
        let colon = first.iter().position(|t| self.is(*t, ":"))?;
        let name = self.name_of(&first[colon + 1..])?;
        Some(self.range(name.start, name.end))
    }

    fn count_non_self_params(&self, params: &[Token]) -> u16 {
        let mut count = 0u16;
        let mut start = 0usize;
        let mut depth = 0i32;
        for (i, token) in params.iter().enumerate() {
            match self.text(*token) {
                "<" | "(" | "[" | "{" => depth += 1,
                ">" | ")" | "]" | "}" => depth -= 1,
                "," if depth == 0 => {
                    if self.is_non_self_param(&params[start..i]) {
                        count = count.saturating_add(1);
                    }
                    start = i + 1;
                }
                _ => {}
            }
        }
        if self.is_non_self_param(&params[start..]) {
            count = count.saturating_add(1);
        }
        count
    }

    fn is_non_self_param(&self, param: &[Token]) -> bool {
        if param.is_empty() {
            return false;
        }
        // Skip leading attributes on the parameter.
        let mut slice = param;
        while slice.first().is_some_and(|t| self.is(*t, "#")) {
            let Some(open) = slice.iter().position(|t| self.is(*t, "[")) else {
                break;
            };
            let Some(close) = self.matching_in(slice, open) else {
                break;
            };
            slice = &slice[close + 1..];
        }
        if slice.is_empty() {
            return false;
        }
        let colon = slice.iter().position(|t| self.is(*t, ":"));
        let head = colon.map_or(slice, |c| &slice[..c]);
        !head.iter().any(|t| self.is(*t, "self"))
    }

    /// Whether the type written at `item` has `#[derive(... Serialize ...)]`
    /// or `#[derive(... Deserialize ...)]`.
    pub fn has_serde_derive(&self, item: Range) -> bool {
        let Some(tokens) = self.tokens_in(item) else {
            return false;
        };
        let mut at = 0;
        while at < tokens.len() {
            if self.is(tokens[at], "#") && tokens.get(at + 1).is_some_and(|t| self.is(*t, "[")) {
                let Some(close) = self.matching_in(tokens, at + 1) else {
                    break;
                };
                let inner = &tokens[at + 2..close];
                let is_derive = inner.first().is_some_and(|t| self.is(*t, "derive"));
                if is_derive
                    && inner
                        .iter()
                        .any(|t| matches!(self.text(*t), "Serialize" | "Deserialize"))
                {
                    return true;
                }
                at = close + 1;
                continue;
            }
            if matches!(self.text(tokens[at]), "struct" | "enum" | "union") {
                break;
            }
            at += 1;
        }
        false
    }

    /// What the text of the function written at `item` says about it, if it has a body.
    pub fn function(&self, item: Range) -> Option<FunctionText> {
        let tokens = self.tokens_in(item)?;
        let last = tokens.len().checked_sub(1)?;
        if !self.is(tokens[last], "}") {
            return None;
        }
        let mut depth = 0;
        let open = (0..=last).rev().find(|&at| {
            match tokens[at].kind {
                Kind::Close => depth += 1,
                Kind::Open => depth -= 1,
                _ => {}
            }
            depth == 0
        })?;
        let signature: Vec<String> = self
            .normalized(&tokens[..open])
            .into_iter()
            .map(|(_, text)| text.to_owned())
            .collect();
        let is_parameter = |name: &str| signature.windows(2).any(|w| w[0] == name && w[1] == ":");
        let body = &tokens[open + 1..last];
        let hands_out = self.hands_out(&tokens[..open], body, is_parameter);
        Some(FunctionText {
            computes: self.computes(body, is_parameter),
            whole: hands_out.whole,
            returns_copies: hands_out.copies,
            lends_to_read: hands_out.to_read,
            signature,
        })
    }

    /// The field written at `field`'s declared type, with `&`, lifetimes, paths
    /// and [`HOLDERS`] peeled off, then what a getter may hand out instead
    /// ([`HANDED_OUT`]), as [`normalized`] tokens:
    /// `[["Vec", "<", "u8", ">"], ["[", "u8", "]"]]` for `pub(crate) bytes: Rc<Vec<u8>>`.
    pub fn field_types(&self, field: Range) -> Vec<Vec<String>> {
        let Some(tokens) = self.tokens_in(field) else {
            return Vec::new();
        };
        let tokens = self.declared_type(tokens);
        let normalized = self.normalized(tokens);
        let peeled: Vec<String> = peel(&normalized)
            .iter()
            .map(|&(_, text)| text.to_owned())
            .collect();
        let instead = match peeled.as_slice() {
            [vec, open, inner @ .., close] if vec == "Vec" && open == "<" && close == ">" => {
                Some([&["[".to_owned()], inner, &["]".to_owned()]].concat())
            }
            [owned] => HANDED_OUT
                .iter()
                .find(|(from, _)| from == owned)
                .map(|(_, to)| vec![(*to).to_owned()]),
            _ => None,
        };
        std::iter::once(peeled).chain(instead).collect()
    }

    /// The tokens of the type a field's definition declares: past its
    /// attributes, visibility and name.
    fn declared_type<'t>(&self, mut tokens: &'t [Token]) -> &'t [Token] {
        while tokens.first().is_some_and(|t| self.is(*t, "#")) {
            let open = tokens.iter().position(|t| self.is(*t, "["));
            let close = open.and_then(|open| self.matching_in(tokens, open));
            tokens = close.map_or(&[], |close| &tokens[close + 1..]);
        }
        if tokens.first().is_some_and(|t| self.is(*t, "pub")) {
            tokens = &tokens[1..];
            if tokens.first().is_some_and(|t| self.is(*t, "(")) {
                let close = self.matching_in(tokens, 0).unwrap_or(tokens.len() - 1);
                tokens = &tokens[close + 1..];
            }
        }
        if let [name, colon, ..] = tokens
            && name.kind == Kind::Ident
            && self.is(*colon, ":")
        {
            tokens = &tokens[2..];
        }
        tokens
    }

    fn text(&self, token: Token) -> &str {
        &self.text[token.start..token.end]
    }

    fn is(&self, token: Token, text: &str) -> bool {
        self.text(token) == text
    }

    fn position(&self, offset: usize) -> Position {
        let line = self.line_starts.partition_point(|&start| start <= offset) - 1;
        Position {
            line: u32::try_from(line).unwrap_or(u32::MAX),
            column: u32::try_from(offset - self.line_starts[line]).unwrap_or(u32::MAX),
        }
    }

    fn offset(&self, position: Position) -> Option<usize> {
        let start = *self.line_starts.get(position.line as usize)?;
        Some(start + position.column as usize).filter(|&at| at <= self.text.len())
    }

    fn range(&self, start: usize, end: usize) -> Range {
        Range {
            start: self.position(start),
            end: self.position(end),
        }
    }

    fn tokens_in(&self, range: Range) -> Option<&[Token]> {
        let (start, end) = (self.offset(range.start)?, self.offset(range.end)?);
        let first = self.tokens.partition_point(|t| t.start < start);
        let last = self.tokens.partition_point(|t| t.end <= end);
        self.tokens.get(first..last.max(first))
    }

    /// Whether an item can start at token `at`: after a brace, a `;`, an
    /// attribute, or nothing. This tells an `impl` block from `impl Trait`.
    fn starts_item(&self, at: usize) -> bool {
        let before = self.tokens[..at]
            .iter()
            .rev()
            .find(|t| !self.is(**t, "unsafe") && !self.is(**t, "default"));
        before.is_none_or(|t| ["{", "}", ";", "]"].contains(&self.text(*t)))
    }

    /// The tokens of an `impl` header's optional trait, its self type, and where its block opens.
    fn impl_header(&self, mut at: usize) -> Option<ImplHeader<'_>> {
        let tokens = &self.tokens;
        if tokens.get(at).is_some_and(|t| self.is(*t, "<")) {
            at = self.past_angles(at)?;
        }
        let start = at;
        let mut trait_end = None;
        let mut self_type = start;
        let mut end = None;
        let mut depth = 0i32;
        for (index, token) in tokens.iter().enumerate().skip(start) {
            match self.text(*token) {
                "<" | "(" | "[" => depth += 1,
                ">" | ")" | "]" => depth -= 1,
                ";" => return None,
                "{" if depth == 0 => {
                    let type_end = end.unwrap_or(index);
                    let trait_type = trait_end.map(|te| &tokens[start..te]);
                    return Some(ImplHeader {
                        trait_type,
                        self_type: &tokens[self_type..type_end],
                        open: index,
                    });
                }
                "for"
                    if depth == 0
                        && end.is_none()
                        && !tokens.get(index + 1).is_some_and(|t| self.is(*t, "<")) =>
                {
                    trait_end = Some(index);
                    self_type = index + 1;
                }
                "where" if depth == 0 => end = Some(index),
                _ => {}
            }
        }
        None
    }

    /// The token naming the type `tokens` write, past what [`peel`] takes off.
    fn name_of(&self, tokens: &[Token]) -> Option<Token> {
        let start = tokens
            .iter()
            .position(|t| !self.is(*t, "const") && !self.is(*t, "!"))?;
        let normalized = self.normalized(&tokens[start..]);
        let &(index, _) = peel(&normalized).first()?;
        let name = tokens[start + index];
        (name.kind == Kind::Ident).then_some(name)
    }

    /// `tokens`' texts with lifetimes and paths left out, each with its index:
    /// `&'a crate::kinds::Ready<'a, T>` is `& Ready < T >`.
    fn normalized<'s>(&'s self, tokens: &[Token]) -> Vec<(usize, &'s str)> {
        let mut out: Vec<(usize, &str)> = Vec::new();
        let mut at = 0;
        while at < tokens.len() {
            let token = tokens[at];
            let next = tokens.get(at + 1);
            if token.kind == Kind::Lifetime {
                at += if next.is_some_and(|t| self.is(*t, ",")) {
                    2
                } else {
                    1
                };
                continue;
            }
            if self.is(token, "::")
                || (token.kind == Kind::Ident && next.is_some_and(|t| self.is(*t, "::")))
            {
                at += 1;
                continue;
            }
            let text = self.text(token);
            match (out.last().map(|&(_, last)| last), text) {
                (Some("<"), ">") => {
                    out.pop();
                }
                (Some(","), ">") => {
                    out.pop();
                    out.push((at, text));
                }
                _ => out.push((at, text)),
            }
            at += 1;
        }
        out
    }

    /// The index past the `>` that closes the `<` at `at`.
    fn past_angles(&self, at: usize) -> Option<usize> {
        let end = self.past_angles_in(&self.tokens, at);
        (end <= self.tokens.len()).then_some(end)
    }

    fn past_angles_in(&self, tokens: &[Token], at: usize) -> usize {
        let mut depth = 0;
        for (index, token) in tokens.iter().enumerate().skip(at) {
            match self.text(*token) {
                "<" => depth += 1,
                ">" => depth -= 1,
                _ => {}
            }
            if depth == 0 {
                return index + 1;
            }
        }
        tokens.len()
    }

    /// The bracket that closes the one at `open`.
    fn matching(&self, open: usize) -> Option<usize> {
        self.matching_in(&self.tokens, open)
    }

    fn matching_in(&self, tokens: &[Token], open: usize) -> Option<usize> {
        let mut depth = 0;
        for (index, token) in tokens.iter().enumerate().skip(open) {
            match token.kind {
                Kind::Open => depth += 1,
                Kind::Close => depth -= 1,
                _ => {}
            }
            if depth == 0 {
                return Some(index);
            }
        }
        None
    }
}

/// `type` past what holds a value: `&`, `mut`, `dyn`, and [`HOLDERS`] with one
/// argument. `& mut Rc < RefCell < T > >` is `T`.
fn peel<'n, 's>(mut written: &'n [(usize, &'s str)]) -> &'n [(usize, &'s str)] {
    loop {
        match written {
            [(_, "&" | "&&" | "*" | "mut" | "const" | "dyn"), rest @ ..] => written = rest,
            [(_, holder), (_, "<"), inner @ .., (_, ">")]
                if HOLDERS.contains(holder) && is_one_argument(inner) =>
            {
                written = inner
            }
            _ => return written,
        }
    }
}

/// Whether `inner` is one generic argument whose own brackets balance.
fn is_one_argument(inner: &[(usize, &str)]) -> bool {
    let mut depth = 0i32;
    for &(_, text) in inner {
        match text {
            "<" | "(" | "[" => depth += 1,
            ">" | ")" | "]" => depth -= 1,
            "," if depth == 0 => return false,
            _ => {}
        }
        if depth < 0 {
            return false;
        }
    }
    depth == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(line: u32, column: u32) -> Position {
        Position { line, column }
    }

    #[test]
    fn the_whole_text_ends_where_rust_analyzer_ends_a_files_module() {
        // What rust-analyzer 1.99.0 writes for these files: it reads `\r\n`
        // as `\n`, and counts a byte order mark's 3 bytes.
        for (text, end) in [
            ("", at(0, 0)),
            ("pub fn d() {}", at(0, 13)),
            ("pub fn a() {}\r\npub fn b() {}\r\n", at(2, 0)),
            ("pub fn e() {}\r\npub fn ff() {}", at(1, 14)),
            ("\u{feff}pub fn c() {}", at(0, 16)),
        ] {
            let whole = Source::new(text.into()).whole();
            assert_eq!((whole.start, whole.end), (at(0, 0), end), "{text:?}");
        }
    }

    #[test]
    fn use_declarations_but_not_precise_captures() {
        let source = Source::new(
            "pub use a::{b,\n c};\nfn f() -> impl Sized + use<> { use d::e; }\n".into(),
        );
        assert_eq!(
            source.scan_file().imports,
            [
                Range {
                    start: at(0, 4),
                    end: at(1, 4)
                },
                Range {
                    start: at(2, 31),
                    end: at(2, 40)
                },
            ]
        );
    }

    #[test]
    fn an_impl_blocks_self_type_is_past_references_and_holders() {
        let text = "fn f() -> impl Display { 1 }\n\
                    #[cfg(x)]\n\
                    impl<'a, T: Fn() -> u8> fmt::Display for &'a Box<crate::Ready<T>> where T: for<'b> X<'b> {\n}\n\
                    unsafe impl Send for (u8, u8) {}\n";
        let blocks = Source::new(text.into()).impl_blocks();
        assert_eq!(
            blocks,
            [
                ImplBlock {
                    block: Range {
                        start: at(2, 0),
                        end: at(3, 1)
                    },
                    self_type: Some(Range {
                        start: at(2, 56),
                        end: at(2, 61)
                    }),
                    trait_type: Some(Range {
                        start: at(2, 29),
                        end: at(2, 36)
                    }),
                },
                ImplBlock {
                    block: Range {
                        start: at(4, 7),
                        end: at(4, 32)
                    },
                    self_type: None,
                    trait_type: Some(Range {
                        start: at(4, 12),
                        end: at(4, 16)
                    }),
                },
            ]
        );
    }

    #[test]
    fn a_getter_computes_nothing_and_writes_its_fields_type() {
        let getter = Source::new(
            "pub fn with<R>(&self, f: impl FnOnce(&Credentials) -> R) -> R {\n    f(&self.credentials.borrow())\n}".into(),
        );
        let text = getter.function(getter.whole()).unwrap();
        assert!(!text.computes);
        let field =
            Source::new("#[allow(x)]\npub(crate) credentials: RefCell<crate::Credentials>".into());
        let types = field.field_types(field.whole());
        assert_eq!(types, [["Credentials"]]);
        assert!(text.writes_any(&types));
    }

    #[test]
    fn operators_macros_and_calls_that_work_compute_but_passing_a_value_on_does_not() {
        let computes = |body: &str| {
            let source = Source::new(format!("fn f(&self, set: u64, each: F) {{ {body} }}"));
            source.function(source.whole()).unwrap().computes
        };
        for quiet in [
            "&mut *self.head",
            "self.x as u64",
            "self.set = set;",
            "&self.items[..]",
            "let items = &self.items;\n items",
            "Some(self.inner.lock().unwrap().clone())",
            "self.held.load(Ordering::Relaxed)",
            "each(&self.items)",
            "if (self.a, self.b).0 { return (self.c); }",
        ] {
            assert!(!computes(quiet), "{quiet}");
        }
        for busy in [
            "self.end - self.start",
            "self.a && self.b",
            "!self.ready",
            "self.x < self.y",
            "self.mask & 1",
            "self.x as u64 * 2",
            "-self.x",
            "format!(\"{}\", self.x)",
            "self.limit.saturating_sub(self.held.load(Relaxed))",
            "self.a.max(self.b)",
            "self.items.iter().map(|x| x.id).collect::<Vec<_>>()",
            "self.parse::<u64>()",
            "(self.hook)()",
        ] {
            assert!(computes(busy), "{busy}");
        }
    }

    #[test]
    fn a_vec_field_may_be_handed_out_as_a_slice_and_an_atomic_as_its_value() {
        let field = Source::new("items: Option<Vec<u32>>".into());
        assert_eq!(
            field.field_types(field.whole()),
            [vec!["Vec", "<", "u32", ">"], vec!["[", "u32", "]"]]
        );
        let field = Source::new("doomed: core::sync::atomic::AtomicBool".into());
        assert_eq!(field.field_types(field.whole()), [["AtomicBool"], ["bool"]]);
    }

    #[test]
    fn glob_reexports_capture_plain_and_restricted_pub_use() {
        let text = "\
use a::*;\n\
pub(crate) use b::*;\n\
pub use c::*;\n\
pub use d::e::*;\n\
pub use f::{x, *};\n\
pub use g::Item;\n";
        let scan = Source::new(text.into()).scan_file();
        let summary: Vec<(u32, bool, Option<&str>)> = scan
            .pub_uses
            .iter()
            .map(|p| (p.line, p.restricted, p.glob_path.as_deref()))
            .collect();
        assert_eq!(
            summary,
            [
                (2, true, Some("b")),
                (3, false, Some("c")),
                (4, false, Some("d::e")),
                (5, false, Some("f")),
                (6, false, None),
            ]
        );
    }

    #[test]
    fn first_param_type_peels_references_and_skips_self() {
        let pick = |code: &str| {
            let source = Source::new(code.into());
            let (_, range) = source.function_info(source.whole())?;
            let range = range?;
            let start = source.offset(range.start)?;
            let end = source.offset(range.end)?;
            Some(source.text[start..end].to_owned())
        };
        assert_eq!(
            pick("pub fn insert(store: &mut crate::Store, k: u32) -> Result<(), Err> {}"),
            Some("Store".into())
        );
        assert_eq!(
            pick("pub fn inspect(s: &Box<Store>) {}"),
            Some("Store".into())
        );
        assert_eq!(pick("pub fn method(&self, k: u32) {}"), None);
        assert_eq!(pick("pub fn none() -> Result<(), Err> {}"), None);
    }

    #[test]
    fn function_metrics_and_serde_derive_are_extracted() {
        let code = "pub fn f(&self, a: u32, b: (u32, u32), c: u32) {\n\
                    \x20   if a > 0 && b.0 > 0 {\n\
                    \x20       for _ in 0..c {}\n\
                    \x20   }\n\
                    }\n";
        let source = Source::new(code.into());
        let (m, _) = source.function_info(source.whole()).unwrap();
        assert_eq!((m.lines, m.branches, m.params), (5, 3, 3));

        let dto = Source::new(
            "#[derive(Debug, Serialize, Deserialize)]\npub struct Dto { a: u32 }".into(),
        );
        assert!(dto.has_serde_derive(dto.whole()));
        let plain = Source::new("#[derive(Debug, Clone)]\npub struct Plain { a: u32 }".into());
        assert!(!plain.has_serde_derive(plain.whole()));
    }
}
