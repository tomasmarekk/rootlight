//! Written Pug ownership is separate from template evaluation and DOM semantics.
//! Unsupported or recovered native contexts keep source-bound gaps.

use rootlight_adapter_sdk::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::{EntityKind, FactDomain};

pub(super) fn written_identity(kind: EntityKind, label: &str) -> bool {
    matches!(
        (kind, label),
        (EntityKind::MarkupElement, "pug.element.declaration")
            | (EntityKind::MarkupAttribute, "pug.attribute.declaration")
    )
}

pub(super) fn gaps(fact: &SyntaxFact) -> impl Iterator<Item = (FactDomain, &'static str)> {
    let detail = match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Root, "pug.source_file.root") => Some(
            "pug-explicit-static-tags-and-attributes-only-template-execution-embedded-binding-and-rendered-dom-unavailable",
        ),
        (SyntaxFactKind::Root, "pug.native_context_unavailable.root")
        | (SyntaxFactKind::Signature, "pug.native_context_unavailable.signature") => {
            Some("pug-complete-clean-native-source-context-unavailable")
        }
        (SyntaxFactKind::Signature, "pug.implicit_or_unavailable_owner.signature") => {
            Some("pug-explicit-static-name-or-direct-written-tag-owner-unavailable")
        }
        (SyntaxFactKind::Signature, "pug.construct_unavailable.signature") => {
            Some("pug-template-and-embedded-source-construct-semantics-unavailable")
        }
        _ => None,
    };
    [
        FactDomain::Entities,
        FactDomain::Occurrences,
        FactDomain::Relations,
    ]
    .into_iter()
    .filter_map(move |domain| detail.map(|detail| (domain, detail)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_adapter_sdk::SyntaxKindLabel;
    use rootlight_ids::FileId;
    use rootlight_ir::SourceSpan;

    fn fact(kind: SyntaxFactKind, label: &str) -> SyntaxFact {
        SyntaxFact::new(
            1,
            None,
            kind,
            SourceSpan::new(FileId::from_bytes([5; 20]), 0, 5).unwrap(),
            0,
            SyntaxKindLabel::new(label).unwrap(),
        )
    }

    #[test]
    fn unavailable_template_context_is_a_gap_in_all_three_semantic_domains() {
        for (kind, label) in [
            (SyntaxFactKind::Root, "pug.source_file.root"),
            (
                SyntaxFactKind::Signature,
                "pug.native_context_unavailable.signature",
            ),
            (SyntaxFactKind::Root, "pug.native_context_unavailable.root"),
            (
                SyntaxFactKind::Signature,
                "pug.implicit_or_unavailable_owner.signature",
            ),
            (
                SyntaxFactKind::Signature,
                "pug.construct_unavailable.signature",
            ),
        ] {
            let captured = fact(kind, label);
            assert_eq!(
                gaps(&captured)
                    .map(|(domain, _)| domain)
                    .collect::<Vec<_>>(),
                [
                    FactDomain::Entities,
                    FactDomain::Occurrences,
                    FactDomain::Relations
                ]
            );
            assert!(!super::super::is_signature_capture(&captured));
        }
        assert!(
            gaps(&fact(
                SyntaxFactKind::Declaration,
                "pug.element.declaration"
            ))
            .next()
            .is_none()
        );
        assert!(
            gaps(&fact(SyntaxFactKind::Occurrence, "pug.name.definition"))
                .next()
                .is_none()
        );
    }

    #[test]
    fn repeated_written_names_have_distinct_source_identities_without_runtime_binding() {
        for (kind, label) in [
            (EntityKind::MarkupElement, "pug.element.declaration"),
            (EntityKind::MarkupAttribute, "pug.attribute.declaration"),
        ] {
            assert!(written_identity(kind, label));
            assert!(super::super::uses_written_source_identity(
                "pug", kind, label
            ));
            let first = super::super::written_source_identity("pug", None, label, 0);
            let second = super::super::written_source_identity("pug", None, label, 1);
            assert_ne!(first, second);
        }
        assert!(!written_identity(
            EntityKind::Property,
            "pug.attribute.declaration"
        ));
        assert!(!written_identity(
            EntityKind::MarkupElement,
            "html.element.declaration"
        ));
    }
}
