//! Token-level extractors for function return types, normalized clone
//! fingerprints, `pub use` re-exports, and referenced identifiers.

use std::collections::{HashMap, HashSet};

use crate::model::BodyFingerprint;
use crate::scip::Range;

use super::tokens::{Kind, Token};

/// Minimum body tokens required before a function is considered for clone detection.
pub const MIN_CLONE_TOKENS: usize = 45;

/// A `pub use` / `pub(...) use` declaration scanned from a file.
#[derive(Debug, PartialEq, Eq)]
pub struct PubUse {
    pub range: Range,
    pub line: u32,
    pub restricted: bool,
    /// Non-empty when the declaration contains `*`.
    pub glob_path: Option<String>,
}

/// Whether the return type slice (tokens after `->` up to `{`, `where`, or `;`)
/// is `Result<..., String>` (or `...::Result<..., ...::String>`).
pub fn returns_string_error(text: &str, ret_tokens: &[Token]) -> bool {
    let mut i = 0;
    while i < ret_tokens.len() {
        let tok = &text[ret_tokens[i].start..ret_tokens[i].end];
        if tok == "Result"
            && ret_tokens
                .get(i + 1)
                .is_some_and(|t| &text[t.start..t.end] == "<")
            && let Some((second_arg, _end)) = second_generic_arg(text, ret_tokens, i + 1)
            && is_plain_string_type(text, second_arg)
        {
            return true;
        }
        i += 1;
    }
    false
}

/// Returns the second top-level generic argument inside `<...>` starting at `open`.
fn second_generic_arg<'a>(
    text: &str,
    tokens: &'a [Token],
    open: usize,
) -> Option<(&'a [Token], usize)> {
    let mut depth = 0i32;
    let mut comma = None;
    for (idx, token) in tokens.iter().enumerate().skip(open) {
        match &text[token.start..token.end] {
            "<" | "(" | "[" | "{" => depth += 1,
            ">" | ")" | "]" | "}" => {
                depth -= 1;
                if depth == 0 {
                    let start = comma? + 1;
                    return Some((&tokens[start..idx], idx));
                }
            }
            "," if depth == 1 && comma.is_none() => comma = Some(idx),
            "," if depth == 1 => return None,
            _ => {}
        }
    }
    None
}

/// Whether `tokens` is `String`, `std::string::String`, `alloc::string::String`, etc.
fn is_plain_string_type(text: &str, tokens: &[Token]) -> bool {
    if tokens.is_empty() {
        return false;
    }
    let last = &text[tokens[tokens.len() - 1].start..tokens[tokens.len() - 1].end];
    if last != "String" {
        return false;
    }
    // Every preceding token must be path segments (`ident` or `::`).
    tokens[..tokens.len() - 1]
        .iter()
        .all(|t| t.kind == Kind::Ident || &text[t.start..t.end] == "::")
}

/// Computes a 128-bit normalized fingerprint of `body_tokens` if it has at least
/// [`MIN_CLONE_TOKENS`] tokens. Parameter names and `let [mut] ident` bindings
/// are normalized to positional slots while field accesses (`.x`) and paths
/// (`::x`) keep their exact names.
pub fn body_fingerprint(
    text: &str,
    param_tokens: &[Token],
    body_tokens: &[Token],
) -> Option<BodyFingerprint> {
    if body_tokens.len() < MIN_CLONE_TOKENS {
        return None;
    }
    let mut slots: HashMap<&str, String> = HashMap::new();
    collect_param_names(text, param_tokens, &mut slots);
    let mut local_count = 0usize;

    let mut hash_a: u64 = 0xcbf2_9ce4_8422_2325;
    let mut hash_b: u64 = 0x9e37_79b9_7f4a_7c15;

    let mut i = 0;
    while i < body_tokens.len() {
        let token = body_tokens[i];
        let raw = &text[token.start..token.end];

        // Record simple `let [mut] ident` bindings so renamed locals match.
        if raw == "let" {
            let mut next = i + 1;
            if body_tokens
                .get(next)
                .is_some_and(|t| &text[t.start..t.end] == "mut")
            {
                next += 1;
            }
            if let Some(binding) = body_tokens.get(next)
                && binding.kind == Kind::Ident
            {
                let name = &text[binding.start..binding.end];
                if name != "_" && !is_keyword(name) && !slots.contains_key(name) {
                    slots.insert(name, format!("$v{local_count}"));
                    local_count += 1;
                }
            }
        }

        let preceded_by_dot_or_path = i > 0
            && matches!(
                &text[body_tokens[i - 1].start..body_tokens[i - 1].end],
                "." | "::"
            );
        let followed_by_path_or_bang = body_tokens
            .get(i + 1)
            .is_some_and(|t| matches!(&text[t.start..t.end], "::" | "!"));

        let normalized = if token.kind == Kind::Ident
            && !preceded_by_dot_or_path
            && !followed_by_path_or_bang
            && let Some(slot) = slots.get(raw)
        {
            slot.as_str()
        } else {
            raw
        };

        mix(&mut hash_a, &mut hash_b, normalized.as_bytes());
        i += 1;
    }

    Some(BodyFingerprint {
        hash_a,
        hash_b,
        tokens: u32::try_from(body_tokens.len()).unwrap_or(u32::MAX),
    })
}

fn collect_param_names<'a>(text: &'a str, params: &[Token], slots: &mut HashMap<&'a str, String>) {
    let mut start = 0usize;
    let mut depth = 0i32;
    let mut param_index = 0usize;
    for (i, token) in params.iter().enumerate() {
        match &text[token.start..token.end] {
            "<" | "(" | "[" | "{" => depth += 1,
            ">" | ")" | "]" | "}" => depth -= 1,
            "," if depth == 0 => {
                register_param(text, &params[start..i], slots, &mut param_index);
                start = i + 1;
            }
            _ => {}
        }
    }
    register_param(text, &params[start..], slots, &mut param_index);
}

fn register_param<'a>(
    text: &'a str,
    param: &[Token],
    slots: &mut HashMap<&'a str, String>,
    param_index: &mut usize,
) {
    let Some(colon) = param.iter().position(|t| &text[t.start..t.end] == ":") else {
        return;
    };
    let head = &param[..colon];
    if let Some(last) = head.last()
        && last.kind == Kind::Ident
    {
        let name = &text[last.start..last.end];
        if name != "self" && name != "_" && !is_keyword(name) && !slots.contains_key(name) {
            slots.insert(name, format!("$p{}", *param_index));
            *param_index += 1;
        }
    }
}

fn is_keyword(word: &str) -> bool {
    matches!(
        word,
        "as" | "async"
            | "await"
            | "break"
            | "const"
            | "continue"
            | "crate"
            | "else"
            | "enum"
            | "extern"
            | "false"
            | "fn"
            | "for"
            | "if"
            | "impl"
            | "in"
            | "let"
            | "loop"
            | "match"
            | "mod"
            | "move"
            | "mut"
            | "pub"
            | "ref"
            | "return"
            | "self"
            | "Self"
            | "static"
            | "struct"
            | "super"
            | "trait"
            | "true"
            | "type"
            | "unsafe"
            | "use"
            | "where"
            | "while"
            | "Some"
            | "None"
            | "Ok"
            | "Err"
    )
}

fn mix(hash_a: &mut u64, hash_b: &mut u64, bytes: &[u8]) {
    for &b in bytes {
        *hash_a ^= u64::from(b);
        *hash_a = hash_a.wrapping_mul(0x0100_0000_01b3);
        *hash_b = hash_b.rotate_left(7) ^ u64::from(b).wrapping_mul(0x517c_c1b7_2722_0a95);
    }
    // Token boundary separator.
    *hash_a ^= 0xff;
    *hash_a = hash_a.wrapping_mul(0x0100_0000_01b3);
    *hash_b = hash_b.rotate_left(13) ^ 0xa076_1d64_78bd_642f;
}

/// Scans every `pub use` and `pub(...) use` declaration in `tokens`.
pub fn pub_uses(
    text: &str,
    tokens: &[Token],
    range_of: impl Fn(usize, usize) -> Range,
) -> Vec<PubUse> {
    let mut found = Vec::new();
    for (at, token) in tokens.iter().enumerate() {
        let precise_capture = tokens
            .get(at + 1)
            .is_some_and(|next| &text[next.start..next.end] == "<");
        if &text[token.start..token.end] != "use" || precise_capture {
            continue;
        }
        let Some((pub_start, restricted)) = pub_before(text, tokens, at) else {
            continue;
        };
        let Some(semi) = tokens[at..]
            .iter()
            .position(|t| &text[t.start..t.end] == ";")
        else {
            continue;
        };
        let body = &tokens[at + 1..at + semi];
        let range = range_of(tokens[pub_start].start, tokens[at + semi].end);
        let glob_path = if body.iter().any(|t| &text[t.start..t.end] == "*") {
            let mut path = String::new();
            for &tok in body {
                let piece = &text[tok.start..tok.end];
                if piece == "*" || piece == "{" {
                    break;
                }
                path.push_str(piece);
            }
            let trimmed = path.trim_end_matches("::").to_owned();
            (!trimmed.is_empty()).then_some(trimmed)
        } else {
            None
        };
        found.push(PubUse {
            range,
            line: range.start.line + 1,
            restricted,
            glob_path,
        });
    }
    found
}

/// If `tokens[at]` (`use`) is preceded by `pub` or `pub(...)`, returns
/// `(index of pub token, is_restricted)`.
fn pub_before(text: &str, tokens: &[Token], at: usize) -> Option<(usize, bool)> {
    if at == 0 {
        return None;
    }
    if &text[tokens[at - 1].start..tokens[at - 1].end] == "pub" {
        return Some((at - 1, false));
    }
    if &text[tokens[at - 1].start..tokens[at - 1].end] == ")" {
        let mut depth = 0i32;
        for idx in (0..at).rev() {
            match &text[tokens[idx].start..tokens[idx].end] {
                ")" => depth += 1,
                "(" => {
                    depth -= 1;
                    if depth == 0 {
                        if idx > 0 && &text[tokens[idx - 1].start..tokens[idx - 1].end] == "pub" {
                            return Some((idx - 1, true));
                        }
                        return None;
                    }
                }
                _ => {}
            }
        }
    }
    None
}

/// Distinct identifier tokens appearing in the file (used for dependency usage checks).
pub fn collect_idents(text: &str, tokens: &[Token]) -> HashSet<String> {
    tokens
        .iter()
        .filter(|t| t.kind == Kind::Ident)
        .map(|t| text[t.start..t.end].to_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::from_index::source::tokens;

    #[test]
    fn detects_result_with_string_error() {
        let check = |ret: &str| {
            let toks = tokens::read(ret);
            returns_string_error(ret, &toks)
        };
        assert!(check("Result<u32, String>"));
        assert!(check(
            "std::result::Result<Vec<(u8, u8)>, std::string::String>"
        ));
        assert!(!check("Result<String, Error>"));
        assert!(!check("Result<u32, MyError>"));
        assert!(!check("Option<String>"));
        assert!(!check("Result<u32, Option<String>>"));
    }

    #[test]
    fn fingerprint_matches_renamed_params_and_locals_but_not_different_fields() {
        let fp = |params: &str, body: &str| {
            let full = format!("fn f({params}) {{ {body} }}");
            let all = tokens::read(&full);
            let p_open = all
                .iter()
                .position(|t| &full[t.start..t.end] == "(")
                .unwrap();
            let p_close = all
                .iter()
                .position(|t| &full[t.start..t.end] == ")")
                .unwrap();
            let b_open = all
                .iter()
                .position(|t| &full[t.start..t.end] == "{")
                .unwrap();
            let b_close = all.len() - 1;
            body_fingerprint(&full, &all[p_open + 1..p_close], &all[b_open + 1..b_close])
        };
        let body_a = "let mut total = 0u64; for item in items.iter() { if item.weight > 0 && item.active { total += item.score * limit + 1; } else { total = total.saturating_sub(item.penalty); } } if total > limit { total - limit } else { total }";
        let body_b = "let mut sum = 0u64; for item in values.iter() { if item.weight > 0 && item.active { sum += item.score * cap + 1; } else { sum = sum.saturating_sub(item.penalty); } } if sum > cap { sum - cap } else { sum }";
        let body_c = "let mut sum = 0u64; for item in values.iter() { if item.other > 0 && item.active { sum += item.score * cap + 1; } else { sum = sum.saturating_sub(item.penalty); } } if sum > cap { sum - cap } else { sum }";
        assert_eq!(
            fp("items: &[Row], limit: u64", body_a),
            fp("values: &[Row], cap: u64", body_b)
        );
        assert!(fp("items: &[Row], limit: u64", body_a).is_some());
        assert_ne!(
            fp("items: &[Row], limit: u64", body_a),
            fp("values: &[Row], cap: u64", body_c)
        );
    }
}
