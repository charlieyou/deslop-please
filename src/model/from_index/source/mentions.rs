//! Paths that code writes as text, which the index does not resolve: the
//! functions a serde attribute names in a string, and the names a format
//! string captures.

use super::Source;
use super::tokens::{Kind, Token};
use crate::scip::Range;

/// The keys of a `#[serde(..)]` attribute whose string names a function.
const SERDE_FUNCTIONS: [&str; 5] = [
    "default",
    "deserialize_with",
    "getter",
    "serialize_with",
    "skip_serializing_if",
];

/// A path written as text, where.
#[derive(Debug, PartialEq, Eq)]
pub struct TextPath {
    pub range: Range,
    pub path: String,
    /// A format string captures it, which only a value can be.
    pub captured: bool,
}

impl Source {
    /// Each path written as text, where: `EvDepth::is_off` in
    /// `#[serde(skip_serializing_if = "EvDepth::is_off")]`, `m::serialize`
    /// and `m::deserialize` in `#[serde(with = "m")]`, `LIMIT` in
    /// `format!("{LIMIT}")`.
    pub(super) fn mentions(&self) -> Vec<TextPath> {
        let tokens = &self.tokens;
        let mut mentions = Vec::new();
        // The brackets open around `at`, each with whether a macro's
        // invocation opens it.
        let mut open: Vec<bool> = Vec::new();
        for (at, token) in tokens.iter().enumerate() {
            match token.kind {
                Kind::Open => {
                    let invoked = at > 0 && self.is(tokens[at - 1], "!");
                    open.push(invoked);
                }
                Kind::Close => {
                    open.pop();
                }
                Kind::Literal if open.last() == Some(&true) => {
                    let range = self.range(token.start, token.end);
                    let captured = captures(self.string(*token).unwrap_or_default());
                    mentions.extend(captured.into_iter().map(|path| TextPath {
                        range,
                        path,
                        captured: true,
                    }));
                }
                _ => {}
            }
            if self.is(*token, "serde")
                && at >= 2
                && self.is(tokens[at - 1], "[")
                && self.is(tokens[at - 2], "#")
            {
                mentions.extend(self.serde_paths(&tokens[at + 1..]));
            }
        }
        mentions
    }

    /// The paths a serde attribute's arguments, from `tokens`, name.
    fn serde_paths(&self, tokens: &[Token]) -> Vec<TextPath> {
        let Some(close) = tokens
            .first()
            .filter(|t| self.is(**t, "("))
            .and_then(|_| self.matching_in(tokens, 0))
        else {
            return Vec::new();
        };
        let mut paths = Vec::new();
        for window in tokens[..close].windows(3) {
            let [key, equals, value] = window else {
                continue;
            };
            let Some(path) = self.string(*value).filter(|_| self.is(*equals, "=")) else {
                continue;
            };
            let range = self.range(value.start, value.end);
            let named = match self.text(*key) {
                "with" => vec![format!("{path}::serialize"), format!("{path}::deserialize")],
                key if SERDE_FUNCTIONS.contains(&key) => vec![path.to_owned()],
                _ => continue,
            };
            paths.extend(named.into_iter().map(|path| TextPath {
                range,
                path,
                captured: false,
            }));
        }
        paths
    }

    /// What a plain string literal holds, without its quotes.
    fn string(&self, token: Token) -> Option<&str> {
        let text = self.text(token);
        text.strip_prefix('"')?.strip_suffix('"')
    }
}

/// The names a format string captures: `a` and `B` in `"{a} {B:>8} {{c}} {0}"`.
fn captures(format: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = format;
    while let Some(start) = rest.find('{') {
        let after = &rest[start + 1..];
        if let Some(escaped) = after.strip_prefix('{') {
            rest = escaped;
            continue;
        }
        let name: String = after
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        let starts_a_name = name
            .chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '_');
        if starts_a_name && after[name.len()..].starts_with(['}', ':']) {
            names.push(name.clone());
        }
        rest = &after[name.len()..];
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mentioned(text: &str) -> Vec<String> {
        let source = Source::new(text.to_owned());
        source
            .mentions()
            .into_iter()
            .map(|text| format!("{}{}", if text.captured { "{}" } else { "" }, text.path))
            .collect()
    }

    #[test]
    fn serde_attributes_name_functions_and_format_strings_capture_names() {
        let text = "#[derive(Serialize)]\nstruct S {\n    \
                    #[serde(default = \"crate::defaults::depth\", skip_serializing_if = \"EvDepth::is_off\")]\n    \
                    depth: EvDepth,\n    #[serde(with = \"hex\", rename = \"not_a_path\")]\n    bytes: Vec<u8>,\n}\n\
                    fn f() -> String { let s = \"{NOT_A_MACRO}\"; format!(\"{LIMIT} {{x}} {0} {width:>8}\", 1) }\n";
        assert_eq!(
            mentioned(text),
            [
                "crate::defaults::depth",
                "EvDepth::is_off",
                "hex::serialize",
                "hex::deserialize",
                "{}LIMIT",
                "{}width",
            ]
        );
    }
}
