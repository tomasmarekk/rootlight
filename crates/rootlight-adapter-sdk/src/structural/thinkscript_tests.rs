//! ThinkScript labels admit only written ordinary declaration and value names.
//! Foreign roles and over-limit text cannot enter the generic identifier fallback.

use super::*;
use crate::SyntaxKindLabel;
use rootlight_ids::FileId;
use rootlight_ir::SourceSpan;

#[test]
fn written_script_input_and_read_names_require_closed_language_roles_and_complete_text() {
    let fact = |kind, label| {
        SyntaxFact::new(
            1,
            None,
            kind,
            SourceSpan::new(FileId::from_bytes([1; 20]), 0, 5).unwrap(),
            0,
            SyntaxKindLabel::new(label).unwrap(),
        )
    };
    assert_eq!(
        structural_entity_kind(&fact(
            SyntaxFactKind::Declaration,
            "thinkscript.script.declaration"
        )),
        Some(EntityKind::Function)
    );
    assert_eq!(
        structural_entity_kind(&fact(
            SyntaxFactKind::Declaration,
            "thinkscript.input.declaration"
        )),
        Some(EntityKind::Parameter)
    );
    for label in [
        "thinkscript.script.definition",
        "thinkscript.input.definition",
        "thinkscript.input_value.read",
    ] {
        let name = fact(SyntaxFactKind::Occurrence, label);
        for text in ["value", "width2", "_input"] {
            assert_eq!(
                structural_captured_name_for_fact("thinkscript", &name, text, text.len())
                    .as_deref(),
                Some(text)
            );
            assert!(
                structural_captured_name_for_fact("thinkscript", &name, text, text.len() - 1)
                    .is_none()
            );
            assert!(structural_captured_name_for_fact("rust", &name, text, text.len()).is_none());
        }
        for text in [
            "",
            "obj.value",
            "value arg",
            "value\0",
            "λ",
            "(value)",
            "1value",
        ] {
            assert!(structural_captured_name_for_fact("thinkscript", &name, text, 128).is_none());
        }
    }
    for (kind, label) in [
        (SyntaxFactKind::Signature, "thinkscript.input_value.read"),
        (
            SyntaxFactKind::Occurrence,
            "thinkscript.input_value.read.extra",
        ),
        (
            SyntaxFactKind::Occurrence,
            "thinkscript.input_value.signature",
        ),
        (
            SyntaxFactKind::Declaration,
            "thinkscript.foreign.declaration",
        ),
    ] {
        assert!(
            structural_captured_name_for_fact("thinkscript", &fact(kind, label), "value", 5)
                .is_none()
        );
    }
}
