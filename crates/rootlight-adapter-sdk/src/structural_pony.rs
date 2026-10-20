//! Written Pony identifiers preserve case, underscores and prime suffixes.
//! Native role labels distinguish type owners from directly declared members.

use rootlight_ir::EntityKind;

use crate::{SyntaxFact, SyntaxFactKind};

pub(super) fn entity_kind(fact: &SyntaxFact) -> Option<EntityKind> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "pony.file.module") => Some(EntityKind::Module),
        (
            SyntaxFactKind::Declaration,
            "pony.class.declaration" | "pony.actor.declaration" | "pony.primitive.declaration",
        ) => Some(EntityKind::Class),
        (SyntaxFactKind::Declaration, "pony.struct.declaration") => Some(EntityKind::Struct),
        (SyntaxFactKind::Declaration, "pony.trait.declaration") => Some(EntityKind::Trait),
        (SyntaxFactKind::Declaration, "pony.interface.declaration") => Some(EntityKind::Interface),
        (SyntaxFactKind::Declaration, "pony.method.declaration" | "pony.behavior.declaration") => {
            Some(EntityKind::Method)
        }
        (SyntaxFactKind::Declaration, "pony.constructor.declaration") => {
            Some(EntityKind::Constructor)
        }
        _ => None,
    }
}

pub(super) fn captured_name(text: &str, maximum: usize) -> Option<&str> {
    if text.is_empty() || text.len() > maximum {
        return None;
    }
    let mut bytes = text.bytes();
    let first = bytes.next()?;
    ((first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'\'')))
    .then_some(text)
}

pub(super) fn captured_name_for_fact<'a>(
    fact: &SyntaxFact,
    text: &'a str,
    maximum: usize,
) -> Option<&'a str> {
    (fact.kind() == SyntaxFactKind::Occurrence
        && fact.syntax_kind().as_str() == "pony.name.definition")
        .then(|| captured_name(text, maximum))
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_preserve_case_and_do_not_infer_computed_or_documentary_text() {
        for name in ["SimpleServer", "_ConnectionNotify", "connected", "value'"] {
            assert_eq!(captured_name(name, name.len()), Some(name));
            assert_eq!(captured_name(name, name.len() - 1), None);
        }
        for name in [
            "",
            "1name",
            "ref connected",
            "a.b",
            "\"Name\"",
            "λ",
            "name-1",
        ] {
            assert_eq!(captured_name(name, 128), None, "{name}");
        }
        assert_ne!(captured_name("Written", 128), captured_name("written", 128));
    }
}
