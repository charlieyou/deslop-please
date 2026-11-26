//! Which `mod` declarations build their module only for tests, read from
//! the `#[cfg(..)]` attributes on them, which the index does not show.

use super::Source;
use super::tokens::{Kind, Token};
use crate::model::ForTests;
use crate::scip::Range;

/// A `mod name;` or `mod name { .. }` as its text writes it.
#[derive(Debug)]
pub struct ModuleDeclaration {
    /// Where it writes the module's name.
    pub name: Range,
    /// One of its attributes is a `cfg` that holds only when `test` does:
    /// `#[cfg(test)]`, `#[cfg(all(test, ..))]`, but not `#[cfg(any(test, ..))]`.
    pub for_tests: bool,
}

/// The items a file's `cfg` attributes build only for tests, and its `mod`
/// declarations.
#[derive(Debug, Default)]
pub struct Declarations {
    /// Every `mod` declaration, in order.
    pub modules: Vec<ModuleDeclaration>,
    /// Each item built only for tests, from its first attribute to its end,
    /// in order of where it starts; one may hold others.
    pub for_tests: Vec<ForTests>,
}

impl Source {
    /// Every `mod` declaration, and every item built only for tests, where a
    /// `cfg` predicate `feature = "<one of test_features>"` holds only for
    /// tests too.
    pub fn declarations(&self, test_features: &[String]) -> Declarations {
        let tokens = &self.tokens;
        let mut declarations = Declarations::default();
        // Whether an attribute read since the last item asks for tests.
        let mut for_tests = false;
        let mut at = 0;
        while at < tokens.len() {
            let token = tokens[at];
            let next = tokens.get(at + 1).copied();
            if self.is(token, "#") && next.is_some_and(|t| self.is(t, "[")) {
                let close = self.matching(at + 1).unwrap_or(tokens.len());
                if self.is_cfg_for_tests(&tokens[at + 2..close], test_features) {
                    for_tests = true;
                    declarations
                        .for_tests
                        .extend(self.item_for_tests(at, close + 1));
                }
                at = close + 1;
                continue;
            }
            if self.is(token, "pub") {
                at = match next {
                    Some(open) if self.is(open, "(") => {
                        self.matching(at + 1).map_or(at + 1, |c| c + 1)
                    }
                    _ => at + 1,
                };
                continue;
            }
            if self.is(token, "mod")
                && let Some(name) = next.filter(|t| t.kind == Kind::Ident)
                && tokens
                    .get(at + 2)
                    .is_some_and(|t| self.is(*t, ";") || self.is(*t, "{"))
            {
                declarations.modules.push(ModuleDeclaration {
                    name: self.range(name.start, name.end),
                    for_tests,
                });
            }
            for_tests = false;
            at += 1;
        }
        declarations
    }

    /// The item whose attributes start at token `first` and whose tokens
    /// follow from `from` on, with any more attributes first.
    fn item_for_tests(&self, first: usize, mut from: usize) -> Option<ForTests> {
        let tokens = &self.tokens;
        while self.is(*tokens.get(from)?, "#") && self.is(*tokens.get(from + 1)?, "[") {
            from = self.matching(from + 1)? + 1;
        }
        let start = from;
        // After an `=`, a `{` opens an expression, and a `;` ends the item.
        let mut assigned = false;
        // How deep in generics' `<..>` the header is.
        let mut angles = 0i32;
        let mut at = from;
        let last = loop {
            let token = *tokens.get(at)?;
            match (token.kind, self.text(token)) {
                (Kind::Open, "{") if !assigned => break self.matching(at)?,
                (Kind::Open, _) => at = self.matching(at)?,
                // The end of what holds it: a field's, an argument's.
                (Kind::Close, _) => break at.checked_sub(1)?,
                (_, ";") => break at,
                (_, ",") if angles <= 0 => break at.checked_sub(1)?,
                (_, "=") if angles <= 0 => assigned = true,
                (_, "<") if !assigned => angles += 1,
                (_, ">") if !assigned => angles -= 1,
                _ => {}
            }
            at += 1;
        };
        if last < start {
            return None;
        }
        let line = |token: Token| self.position(token.start).line + 1;
        let words: Vec<&str> = tokens[start..last]
            .iter()
            .map(|t| self.text(*t))
            .filter(|word| !matches!(*word, "pub" | "(" | ")" | "crate" | "super" | "in"))
            .take(1)
            .collect();
        Some(ForTests {
            lines: (line(tokens[first]), line(tokens[last])),
            inline_module: words == ["mod"] && self.is(tokens[last], "}"),
        })
    }

    /// Whether the attribute `tokens` write, inside `#[..]`, is a `cfg` that
    /// holds only when `test` does.
    fn is_cfg_for_tests(&self, tokens: &[Token], test_features: &[String]) -> bool {
        match tokens {
            [cfg, open, predicate @ .., close] => {
                self.is(*cfg, "cfg")
                    && self.is(*open, "(")
                    && self.is(*close, ")")
                    && self.implies_test(predicate, test_features)
            }
            _ => false,
        }
    }

    /// Whether the `cfg` predicate `tokens` write holds only when `test`
    /// does: `test`, a feature of `test_features`, an `all` with such a
    /// predicate in it, an `any` of only such predicates. A `not` is taken
    /// to hold without `test`.
    fn implies_test(&self, tokens: &[Token], test_features: &[String]) -> bool {
        match tokens {
            [name] => self.is(*name, "test"),
            [name, equals, value] if self.is(*name, "feature") && self.is(*equals, "=") => {
                let value = self.text(*value);
                test_features.iter().any(|feature| {
                    value.strip_prefix('"').and_then(|v| v.strip_suffix('"')) == Some(feature)
                })
            }
            [operator, open, inner @ .., close] if self.is(*open, "(") && self.is(*close, ")") => {
                let predicates = self.predicates(inner);
                match self.text(*operator) {
                    "all" => predicates
                        .iter()
                        .any(|p| self.implies_test(p, test_features)),
                    "any" => {
                        !predicates.is_empty()
                            && predicates
                                .iter()
                                .all(|p| self.implies_test(p, test_features))
                    }
                    _ => false,
                }
            }
            _ => false,
        }
    }

    /// The predicates a comma-separated list writes, by its commas outside brackets.
    fn predicates<'t>(&self, tokens: &'t [Token]) -> Vec<&'t [Token]> {
        let mut depth = 0i32;
        tokens
            .split(|token| {
                match token.kind {
                    Kind::Open => depth += 1,
                    Kind::Close => depth -= 1,
                    _ => {}
                }
                depth == 0 && self.is(*token, ",")
            })
            .filter(|predicate| !predicate.is_empty())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each `mod` declaration's name, and whether it builds only for tests.
    fn declared(text: &str) -> Vec<(String, bool)> {
        let source = Source::new(text.into());
        source
            .declarations(&[])
            .modules
            .into_iter()
            .map(|declaration| {
                let start = source.offset(declaration.name.start).unwrap();
                let end = source.offset(declaration.name.end).unwrap();
                (text[start..end].to_owned(), declaration.for_tests)
            })
            .collect()
    }

    #[test]
    fn a_cfg_that_implies_test_builds_a_module_only_for_tests() {
        let text = "#[cfg(test)]\nmod support;\n\
                    #[cfg(all(test, feature = \"slow\"))]\npub(crate) mod soak {\n    mod deep;\n}\n\
                    #[cfg(any(test))]\npub(in crate::a) mod only;\n\
                    #[cfg(test)]\n// a note\n/// docs\n#[path = \"x.rs\"]\nmod moved;\n\
                    #[cfg(all(feature = \"a\", any(test, all(test, unix))))]\nmod nested;\n";
        assert_eq!(
            declared(text),
            [
                ("support".to_owned(), true),
                ("soak".to_owned(), true),
                ("deep".to_owned(), false),
                ("only".to_owned(), true),
                ("moved".to_owned(), true),
                ("nested".to_owned(), true),
            ]
        );
    }

    #[test]
    fn any_not_and_attributes_of_other_items_do_not() {
        let text = "#![cfg(test)]\nmod plain;\n\
                    #[cfg(any(test, feature = \"fuzz\"))]\nmod fuzzing;\n\
                    #[cfg(not(test))]\nmod real;\n\
                    #[cfg(all(not(test), unix))]\nmod unix_only;\n\
                    #[cfg(any())]\nmod never;\n\
                    #[cfg_attr(test, path = \"t.rs\")]\nmod switched;\n\
                    #[cfg(test)]\nfn helper() {}\nmod after;\n\
                    #[cfg(test)]\nuse a::b;\nmod later;\n\
                    macro_rules! m { ($name:ident) => { #[cfg(test)] mod $name; } }\n";
        assert_eq!(
            declared(text),
            [
                ("plain".to_owned(), false),
                ("fuzzing".to_owned(), false),
                ("real".to_owned(), false),
                ("unix_only".to_owned(), false),
                ("never".to_owned(), false),
                ("switched".to_owned(), false),
                ("after".to_owned(), false),
                ("later".to_owned(), false),
            ]
        );
        // Text that stops inside an attribute.
        assert_eq!(declared("mod a;\n#[cfg(test"), [("a".to_owned(), false)]);
    }
}
