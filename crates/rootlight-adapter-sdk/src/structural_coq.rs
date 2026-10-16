//! Coq labels classify written declarations, fields and explicit constructors.
//! They do not establish elaborated types, generated symbols or module membership.

use rootlight_ir::EntityKind;

use crate::{SyntaxFact, SyntaxFactKind};

pub(super) fn entity_kind(fact: &SyntaxFact) -> Option<EntityKind> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "coq.file.module") => Some(EntityKind::Module),
        (
            SyntaxFactKind::Declaration,
            "coq.module_header.declaration" | "coq.module_type_header.declaration",
        ) => Some(EntityKind::Module),
        (
            SyntaxFactKind::Declaration,
            "coq.definition.declaration"
            | "coq.theorem.declaration"
            | "coq.fixpoint.declaration"
            | "coq.instance.declaration"
            | "coq.declared_instance.declaration",
        ) => Some(EntityKind::Constant),
        (SyntaxFactKind::Declaration, "coq.inductive.declaration") => Some(EntityKind::Enum),
        (SyntaxFactKind::Declaration, "coq.record.declaration") => Some(EntityKind::Struct),
        (SyntaxFactKind::Declaration, "coq.class.declaration") => Some(EntityKind::Class),
        (
            SyntaxFactKind::Declaration,
            "coq.record_field.declaration" | "coq.class_field.declaration",
        ) => Some(EntityKind::Field),
        (SyntaxFactKind::Declaration, "coq.record_constructor.declaration") => {
            Some(EntityKind::Constructor)
        }
        (SyntaxFactKind::Declaration, "coq.constructor.declaration") => {
            Some(EntityKind::Constructor)
        }
        _ => None,
    }
}

pub(super) fn captured_name(text: &str, maximum_bytes: usize) -> Option<&str> {
    // Native captures still require the fixed lexical keywords from Rocq 9.2:
    // https://rocq-prover.org/doc/V9.2.0/refman/language/core/basic.html
    if matches!(
        text,
        "_" | "Axiom"
            | "CoFixpoint"
            | "Definition"
            | "Fixpoint"
            | "Hypothesis"
            | "Parameter"
            | "Prop"
            | "SProp"
            | "Set"
            | "Theorem"
            | "Type"
            | "Variable"
            | "as"
            | "at"
            | "cofix"
            | "else"
            | "end"
            | "fix"
            | "for"
            | "forall"
            | "fun"
            | "if"
            | "in"
            | "let"
            | "match"
            | "return"
            | "then"
            | "where"
            | "with"
    ) {
        return None;
    }
    let mut characters = text.chars();
    (text.len() <= maximum_bytes
        && characters
            .next()
            .is_some_and(|character| character.is_alphabetic() || character == '_')
        && characters
            .all(|character| character.is_alphanumeric() || matches!(character, '_' | '\'')))
    .then_some(text)
}

pub(super) fn captured_name_for_fact<'a>(
    fact: &SyntaxFact,
    text: &'a str,
    maximum_bytes: usize,
) -> Option<&'a str> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (
            SyntaxFactKind::Occurrence,
            "coq.name.definition"
            | "coq.declared_instance_name.definition"
            | "coq.record_field_name.definition"
            | "coq.class_field_name.definition",
        ) => captured_name(text, maximum_bytes),
        (SyntaxFactKind::Occurrence, "coq.written_reference.reference") => {
            crate::structural_captured_name(text, maximum_bytes)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SyntaxKindLabel;
    use rootlight_ids::FileId;
    use rootlight_ir::SourceSpan;

    fn fact(kind: SyntaxFactKind, label: &str) -> SyntaxFact {
        SyntaxFact::new(
            1,
            None,
            kind,
            SourceSpan::new(FileId::from_bytes([9; 20]), 0, 1).unwrap(),
            0,
            SyntaxKindLabel::new(label).unwrap(),
        )
    }

    #[test]
    fn coq_categories_reject_unknown_labels_and_roles() {
        for (label, kind) in [
            ("coq.definition.declaration", EntityKind::Constant),
            ("coq.theorem.declaration", EntityKind::Constant),
            ("coq.fixpoint.declaration", EntityKind::Constant),
            ("coq.instance.declaration", EntityKind::Constant),
            ("coq.declared_instance.declaration", EntityKind::Constant),
            ("coq.inductive.declaration", EntityKind::Enum),
            ("coq.record.declaration", EntityKind::Struct),
            ("coq.class.declaration", EntityKind::Class),
            ("coq.record_field.declaration", EntityKind::Field),
            ("coq.class_field.declaration", EntityKind::Field),
            (
                "coq.record_constructor.declaration",
                EntityKind::Constructor,
            ),
            ("coq.constructor.declaration", EntityKind::Constructor),
            ("coq.module_header.declaration", EntityKind::Module),
            ("coq.module_type_header.declaration", EntityKind::Module),
        ] {
            assert_eq!(
                crate::structural_entity_kind(&fact(SyntaxFactKind::Declaration, label)),
                Some(kind)
            );
            for other in [
                SyntaxFactKind::Signature,
                SyntaxFactKind::Occurrence,
                SyntaxFactKind::Module,
            ] {
                assert_eq!(crate::structural_entity_kind(&fact(other, label)), None);
            }
            assert_eq!(
                crate::structural_entity_kind(&fact(
                    SyntaxFactKind::Declaration,
                    &format!("{label}.extra")
                )),
                None
            );
        }
        for label in [
            "coq.unreviewed_function.declaration",
            "coq.context_parameter.declaration",
            "coq.generated_projection.declaration",
        ] {
            assert_eq!(
                crate::structural_entity_kind(&fact(SyntaxFactKind::Declaration, label)),
                None
            );
        }
        assert_eq!(
            crate::structural_entity_kind(&fact(SyntaxFactKind::Module, "coq.file.module")),
            Some(EntityKind::Module)
        );
    }

    #[test]
    fn coq_names_preserve_authored_unicode_and_reject_cross_language_labels() {
        for name in ["value", "résumé", "Σ", "x'", "_value2"] {
            for label in [
                "coq.name.definition",
                "coq.declared_instance_name.definition",
                "coq.record_field_name.definition",
                "coq.class_field_name.definition",
            ] {
                let definition = fact(SyntaxFactKind::Occurrence, label);
                assert_eq!(
                    crate::structural_captured_name_for_fact("coq", &definition, name, name.len())
                        .as_deref(),
                    Some(name)
                );
                assert!(
                    crate::structural_captured_name_for_fact("rust", &definition, name, name.len())
                        .is_none()
                );
            }
            assert!(captured_name(name, name.len() - 1).is_none());
        }
        for name in [
            "",
            "bad name",
            "3name",
            "A.B",
            "x\n",
            "_",
            "Type",
            "Definition",
            "match",
        ] {
            assert!(captured_name(name, 100).is_none());
        }
    }
}
