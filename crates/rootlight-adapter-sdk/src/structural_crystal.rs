//! Crystal static names retain their written case and method suffixes.
//! Native role labels distinguish lexical type owners from their direct methods.

use crate::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::EntityKind;

pub(super) fn entity_kind(fact: &SyntaxFact) -> Option<EntityKind> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "crystal.file.module") => Some(EntityKind::Module),
        (SyntaxFactKind::Declaration, "crystal.module.declaration") => Some(EntityKind::Module),
        (SyntaxFactKind::Declaration, "crystal.class.declaration") => Some(EntityKind::Class),
        (SyntaxFactKind::Declaration, "crystal.struct.declaration") => Some(EntityKind::Struct),
        (SyntaxFactKind::Declaration, "crystal.enum.declaration") => Some(EntityKind::Enum),
        (SyntaxFactKind::Declaration, "crystal.method.declaration") => Some(EntityKind::Method),
        _ => None,
    }
}

pub(super) fn captured_name(text: &str, maximum: usize) -> Option<&str> {
    if text.is_empty() || text.len() > maximum {
        return None;
    }
    let name = text.strip_suffix(['?', '!', '=']).unwrap_or(text);
    let mut bytes = name.bytes();
    let first = bytes.next()?;
    ((first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'))
    .then_some(text)
}

pub(super) fn captured_name_for_fact<'a>(
    fact: &SyntaxFact,
    text: &'a str,
    maximum: usize,
) -> Option<&'a str> {
    (fact.kind() == SyntaxFactKind::Occurrence
        && fact.syntax_kind().as_str() == "crystal.name.definition")
        .then(|| captured_name(text, maximum))
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_names_preserve_case_and_single_method_suffix() {
        for name in [
            "Kemal",
            "NullLogHandler",
            "call",
            "ready?",
            "write!",
            "value=",
        ] {
            assert_eq!(captured_name(name, name.len()), Some(name));
            assert_eq!(captured_name(name, name.len() - 1), None);
        }
        for name in [
            "",
            "1name",
            "Kemal::Nested",
            "self.call",
            "value??",
            "foo bar",
            "\"Name\"",
            "λ",
        ] {
            assert_eq!(captured_name(name, 128), None, "{name}");
        }
        assert_ne!(captured_name("Written", 128), captured_name("written", 128));
    }
}
