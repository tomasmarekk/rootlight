//! HDL declarations describe authored objects before preprocessing or elaboration.
//! Escaped identifiers retain physical spelling in occurrences and decoded identity here.

use rootlight_ir::EntityKind;

use crate::{SyntaxFact, SyntaxFactKind};

fn syntax(label: &str) -> Option<&str> {
    label
        .strip_prefix("verilog.")
        .or_else(|| label.strip_prefix("systemverilog."))
}

pub(super) fn entity_kind(fact: &SyntaxFact) -> Option<EntityKind> {
    match (fact.kind(), syntax(fact.syntax_kind().as_str())?) {
        (SyntaxFactKind::Module, "file.module") => Some(EntityKind::Module),
        (
            SyntaxFactKind::Declaration,
            "module.declaration" | "primitive.declaration" | "program.declaration",
        ) => Some(EntityKind::Module),
        (SyntaxFactKind::Declaration, "interface.declaration") => Some(EntityKind::Interface),
        (SyntaxFactKind::Declaration, "class.declaration") => Some(EntityKind::Class),
        (
            SyntaxFactKind::Declaration,
            "package.declaration"
            | "generate.declaration"
            | "modport.declaration"
            | "clocking.declaration",
        ) => Some(EntityKind::Namespace),
        (
            SyntaxFactKind::Declaration,
            "function.declaration"
            | "task.declaration"
            | "checker.declaration"
            | "property.declaration"
            | "sequence.declaration"
            | "let.declaration",
        ) => Some(EntityKind::Function),
        (SyntaxFactKind::Declaration, "port.declaration") => Some(EntityKind::Field),
        (SyntaxFactKind::Declaration, "argument.declaration") => Some(EntityKind::Parameter),
        (
            SyntaxFactKind::Declaration,
            "parameter.declaration" | "enumerator.declaration" | "macro.declaration",
        ) => Some(EntityKind::Constant),
        (
            SyntaxFactKind::Declaration,
            "variable.declaration"
            | "net.declaration"
            | "instance.declaration"
            | "genvar.declaration",
        ) => Some(EntityKind::Variable),
        (SyntaxFactKind::Declaration, "type.declaration") => Some(EntityKind::TypeAlias),
        (SyntaxFactKind::Declaration, "type_parameter.declaration") => {
            Some(EntityKind::TypeParameter)
        }
        _ => None,
    }
}

pub(super) fn captured_name(text: &str, maximum_bytes: usize) -> Option<&str> {
    if text.len() > maximum_bytes {
        return None;
    }
    if let Some(name) = text.strip_prefix('\\') {
        return (!name.is_empty() && name.bytes().all(|byte| byte.is_ascii_graphic()))
            .then_some(name);
    }
    let mut bytes = text.bytes();
    (bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$')))
    .then_some(text)
}

pub(super) fn captured_name_for_fact<'a>(
    fact: &SyntaxFact,
    text: &'a str,
    maximum_bytes: usize,
) -> Option<&'a str> {
    match (fact.kind(), syntax(fact.syntax_kind().as_str())?) {
        (SyntaxFactKind::Occurrence, "system_call.call") => (text.len() <= maximum_bytes
            && text.starts_with('$'))
        .then(|| text.strip_prefix('$'))
        .flatten()
        .filter(|name| captured_name(name, maximum_bytes).is_some())
        .map(|_| text),
        (
            SyntaxFactKind::Occurrence,
            "name.definition"
            | "identifier.reference"
            | "type.reference"
            | "written_call.call"
            | "macro.reference",
        ) => captured_name(text, maximum_bytes),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SyntaxKindLabel;
    use rootlight_ids::FileId;
    use rootlight_ir::SourceSpan;

    #[test]
    fn hdl_roles_require_exact_namespace_and_fact_kind() {
        let fact = |kind, label: &str| {
            SyntaxFact::new(
                1,
                None,
                kind,
                SourceSpan::new(FileId::from_bytes([3; 20]), 0, 1).unwrap(),
                0,
                SyntaxKindLabel::new(label).unwrap(),
            )
        };
        for (label, kind) in [
            ("systemverilog.module.declaration", EntityKind::Module),
            ("systemverilog.port.declaration", EntityKind::Field),
            ("systemverilog.argument.declaration", EntityKind::Parameter),
            ("systemverilog.parameter.declaration", EntityKind::Constant),
            ("systemverilog.instance.declaration", EntityKind::Variable),
            ("systemverilog.generate.declaration", EntityKind::Namespace),
            ("systemverilog.type.declaration", EntityKind::TypeAlias),
        ] {
            assert_eq!(
                entity_kind(&fact(SyntaxFactKind::Declaration, label)),
                Some(kind)
            );
            assert_eq!(entity_kind(&fact(SyntaxFactKind::Occurrence, label)), None);
            assert_eq!(
                entity_kind(&fact(
                    SyntaxFactKind::Declaration,
                    &format!("{label}.extra")
                )),
                None
            );
        }
        assert_eq!(
            entity_kind(&fact(
                SyntaxFactKind::Declaration,
                "systemverilog.unknown_function.declaration"
            )),
            None
        );
    }

    #[test]
    fn hdl_escaped_names_separate_written_and_canonical_spelling() {
        for (written, canonical) in [
            ("name$2", "name$2"),
            ("_name", "_name"),
            ("\\data[0]", "data[0]"),
            ("\\a.b", "a.b"),
            ("\\module", "module"),
        ] {
            assert_eq!(captured_name(written, written.len()), Some(canonical));
            assert_eq!(captured_name(written, written.len() - 1), None);
        }
        for invalid in ["", "\\", "$system", "4value", "a.b", "\\a b", "\\a\n", "λ"] {
            assert_eq!(captured_name(invalid, usize::MAX), None);
        }
    }
}
