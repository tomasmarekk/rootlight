//! Ordinary PHP labels carry proof obligations rather than portable declarations.
//! Only the current-source lowering consumer can admit their native parameter entity.

use super::*;

#[test]
fn ordinary_parameter_and_counterfeit_labels_cannot_enable_portable_parameter_materialization() {
    let range =
        rootlight_ir::SourceSpan::new(rootlight_ids::FileId::from_bytes([1; 20]), 0, 4).unwrap();
    for label in [
        "php.ordinary_parameter.declaration",
        "php.ordinary_counterfeit_parameter.declaration",
        "php.ordinary_function_context.signature",
        "php.ordinary_parameter.extra",
    ] {
        let fact = SyntaxFact::new(
            1,
            None,
            SyntaxFactKind::Declaration,
            range,
            0,
            crate::SyntaxKindLabel::new(label).unwrap(),
        );
        assert_eq!(structural_entity_kind(&fact), None);
        assert_eq!(structural_entity_kind_from_source(&fact, "$lat"), None);
    }
}
