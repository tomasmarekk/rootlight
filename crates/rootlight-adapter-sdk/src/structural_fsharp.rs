//! F# structural classification preserves reviewed source shapes and written names.
//! Ambiguous and unsupported declarations supply entity gaps to shared lowering;
//! signature contracts never imply executable bodies or resolved type identity.

use std::borrow::Cow;

use rootlight_ir::{EntityKind, FactDomain, SkippedRegionReason, SourceSpan};

use crate::{SyntaxFact, SyntaxFactKind};

/// Describes an exact source-backed structural entity classification gap.
///
/// Lowering must retain this span as an Entities-domain UnsupportedConstruct
/// skipped region and account for bounded coverage instead of dropping the fact.
/// The detail is source-free; the original declaration spelling is not included.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StructuralClassificationGap {
    span: SourceSpan,
    detail: &'static str,
}

impl StructuralClassificationGap {
    /// Returns the exact declaration span whose entity shape remains unsupported.
    #[must_use]
    pub const fn span(self) -> SourceSpan {
        self.span
    }

    /// Returns the source-free detail used by the skipped-region record.
    #[must_use]
    pub const fn detail(self) -> &'static str {
        self.detail
    }

    /// Returns the affected fact domain, leaving unrelated domains unchanged.
    #[must_use]
    pub const fn domain(self) -> FactDomain {
        FactDomain::Entities
    }

    /// Returns the existing IR reason for an unsupported declaration shape.
    #[must_use]
    pub const fn reason(self) -> SkippedRegionReason {
        SkippedRegionReason::UnsupportedConstruct
    }
}

/// Classifies exact F# declaration labels without inferring implementation.
///
/// A `val` declares a value even when its type contains arrows. Explicit
/// signature method/property/new/field labels describe source contracts, not
/// proof of a body, invocation, type use or relation target. The exact file-module
/// role retains its shared structural Module kind. Other non-declarations and
/// facts outside the F# label namespace return `Ok(None)`.
///
/// # Errors
/// Returns an exact-span gap for unresolved member specs, ambiguous type
/// representations and unknown F# declaration labels. The caller must not
/// fall through to a generic substring classifier or report complete coverage.
pub(super) fn entity_kind(
    fact: &SyntaxFact,
) -> Result<Option<EntityKind>, StructuralClassificationGap> {
    let label = fact.syntax_kind().as_str();
    if fact.kind() == SyntaxFactKind::Module && label == "fsharp.file.module" {
        return Ok(Some(EntityKind::Module));
    }
    if fact.kind() != SyntaxFactKind::Declaration || !label.starts_with("fsharp.") {
        return Ok(None);
    }
    let kind = match label {
        "fsharp.namespace.declaration" => EntityKind::Namespace,
        "fsharp.module.declaration" | "fsharp.nested_module.declaration" => EntityKind::Module,
        "fsharp.function.declaration" => EntityKind::Function,
        "fsharp.variable.declaration" => EntityKind::Variable,
        "fsharp.record.declaration" | "fsharp.struct.declaration" => EntityKind::Struct,
        "fsharp.union.declaration" => EntityKind::Union,
        "fsharp.enum.declaration" => EntityKind::Enum,
        "fsharp.type_alias.declaration" => EntityKind::TypeAlias,
        "fsharp.class.declaration" => EntityKind::Class,
        "fsharp.interface.declaration" => EntityKind::Interface,
        "fsharp.record_field.declaration" | "fsharp.union_field.declaration" => EntityKind::Field,
        "fsharp.union_case.declaration" | "fsharp.constructor.declaration" => {
            EntityKind::Constructor
        }
        "fsharp.enum_constant.declaration" => EntityKind::Constant,
        "fsharp.method.declaration" => EntityKind::Method,
        "fsharp.property.declaration" => EntityKind::Property,
        "fsharp.signature_val.declaration" => EntityKind::Variable,
        "fsharp.signature_method.declaration" => EntityKind::Method,
        "fsharp.signature_property.declaration" => EntityKind::Property,
        "fsharp.signature_new.declaration" => EntityKind::Constructor,
        "fsharp.signature_field.declaration" => EntityKind::Field,
        _ => {
            return Err(StructuralClassificationGap {
                span: fact.span(),
                detail: match label {
                    // A bare name may denote a type alias or a single-case union
                    // after compiler name lookup; neither kind is syntax-proven.
                    "fsharp.ambiguous_type.declaration" => "fsharp-type-representation-ambiguous",
                    "fsharp.signature_member.declaration" => {
                        "fsharp-signature-member-shape-unresolved"
                    }
                    _ => "fsharp-declaration-shape-unsupported",
                },
            });
        }
    };
    Ok(Some(kind))
}

fn bare_component(text: &str) -> bool {
    let mut chars = text.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    let admitted = |ch: char| {
        ch == '_'
            || ch == '\''
            || ch.is_alphanumeric()
            || !ch.is_ascii() && !ch.is_control() && !ch.is_whitespace()
    };
    // Unicode XID membership comes from the pinned grammar token. This bounded
    // boundary must not reject combining marks by using only is_alphanumeric.
    (first == '_' || first.is_alphabetic() || !first.is_ascii() && admitted(first))
        && chars.all(admitted)
}

fn written_path(mut text: &str) -> bool {
    loop {
        let remaining = if let Some(content) = text.strip_prefix("``") {
            let Some(end) = content.find("``") else {
                return false;
            };
            let Some(quoted) = content.get(..end) else {
                return false;
            };
            if quoted.is_empty() || quoted.chars().any(char::is_control) {
                return false;
            }
            let Some(after) = end.checked_add(2).and_then(|end| content.get(end..)) else {
                return false;
            };
            after
        } else {
            let end = text.find('.').unwrap_or(text.len());
            let Some(component) = text.get(..end) else {
                return false;
            };
            if !bare_component(component) {
                return false;
            }
            let Some(after) = text.get(end..) else {
                return false;
            };
            after
        };
        if remaining.is_empty() {
            return true;
        }
        let Some(after_dot) = remaining.strip_prefix('.') else {
            return false;
        };
        if after_dot.is_empty() {
            return false;
        }
        text = after_dot;
    }
}

fn written_operator(text: &str) -> bool {
    let Some(operator) = text
        .strip_prefix('(')
        .and_then(|text| text.strip_suffix(')'))
    else {
        return false;
    };
    let operator = operator.trim();
    if operator == ".. .." {
        return true;
    }
    let mut chars = operator.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    "!%&*+./<=>@^|~$?-".contains(first) && chars.all(|ch| "!%&*+./<=>@^|~?-".contains(ch))
}

/// Preserves exact bounded source identity for reviewed F# definition captures.
///
/// Only exact F# language and capture labels enable quoted/operator names. All
/// source bytes count against the limit. This validates capture shape, not
/// arbitrary F# text, Unicode XID membership or name lookup; quoted and bare
/// spellings remain distinct until a resolver supplies equivalence.
#[must_use]
pub(super) fn captured_name<'a>(
    language: &str,
    label: &str,
    text: &'a str,
    maximum_bytes: usize,
) -> Option<Cow<'a, str>> {
    if !matches!(language, "fsharp" | "fsharp-signature")
        || text.is_empty()
        || text.len() > maximum_bytes
        || text.chars().any(char::is_control)
    {
        return None;
    }
    let accepted = match label {
        "fsharp.name.definition" => written_path(text),
        "fsharp.operator_name.definition" => written_operator(text),
        "fsharp.new_name.definition" => text == "new",
        _ => false,
    };
    accepted.then_some(Cow::Borrowed(text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        SyntaxKindLabel, structural_captured_name_for_fact, structural_captured_name_for_language,
        structural_entity_classification_gap, structural_entity_kind,
        structural_entity_kind_from_source, structural_type_member_kind,
    };
    use rootlight_ids::FileId;

    fn fact(kind: SyntaxFactKind, label: &str, span: SourceSpan) -> SyntaxFact {
        SyntaxFact::new(1, None, kind, span, 0, SyntaxKindLabel::new(label).unwrap())
    }

    fn declaration(label: &str) -> SyntaxFact {
        fact(
            SyntaxFactKind::Declaration,
            label,
            SourceSpan::new(FileId::from_bytes([3; 20]), 5, 20).unwrap(),
        )
    }

    #[test]
    fn fsharp_file_module_preserves_its_exact_shared_module_role() {
        let span = declaration("unused").span();
        let module = fact(SyntaxFactKind::Module, "fsharp.file.module", span);
        assert_eq!(entity_kind(&module), Ok(Some(EntityKind::Module)));
        assert_eq!(structural_entity_kind(&module), Some(EntityKind::Module));
        assert_eq!(
            structural_entity_kind_from_source(&module, "let value = 1"),
            Some(EntityKind::Module)
        );
        assert!(structural_entity_classification_gap(&module).is_none());

        for label in ["fsharp.unknown.module", "fsharp.file.module.extra"] {
            let unknown = fact(SyntaxFactKind::Module, label, span);
            assert_eq!(structural_entity_kind(&unknown), None);
            assert!(structural_entity_classification_gap(&unknown).is_none());
        }
        let occurrence = fact(SyntaxFactKind::Occurrence, "fsharp.file.module", span);
        assert_eq!(structural_entity_kind(&occurrence), None);
        assert!(structural_entity_classification_gap(&occurrence).is_none());
    }

    #[test]
    fn reviewed_fsharp_declarations_have_one_kind_without_entity_gaps() {
        for (label, expected) in [
            ("fsharp.namespace.declaration", EntityKind::Namespace),
            ("fsharp.module.declaration", EntityKind::Module),
            ("fsharp.nested_module.declaration", EntityKind::Module),
            ("fsharp.function.declaration", EntityKind::Function),
            ("fsharp.variable.declaration", EntityKind::Variable),
            ("fsharp.record.declaration", EntityKind::Struct),
            ("fsharp.struct.declaration", EntityKind::Struct),
            ("fsharp.union.declaration", EntityKind::Union),
            ("fsharp.enum.declaration", EntityKind::Enum),
            ("fsharp.type_alias.declaration", EntityKind::TypeAlias),
            ("fsharp.class.declaration", EntityKind::Class),
            ("fsharp.interface.declaration", EntityKind::Interface),
            ("fsharp.record_field.declaration", EntityKind::Field),
            ("fsharp.union_field.declaration", EntityKind::Field),
            ("fsharp.union_case.declaration", EntityKind::Constructor),
            ("fsharp.enum_constant.declaration", EntityKind::Constant),
            ("fsharp.method.declaration", EntityKind::Method),
            ("fsharp.property.declaration", EntityKind::Property),
            ("fsharp.constructor.declaration", EntityKind::Constructor),
            ("fsharp.signature_val.declaration", EntityKind::Variable),
            ("fsharp.signature_method.declaration", EntityKind::Method),
            (
                "fsharp.signature_property.declaration",
                EntityKind::Property,
            ),
            ("fsharp.signature_new.declaration", EntityKind::Constructor),
            ("fsharp.signature_field.declaration", EntityKind::Field),
        ] {
            let fact = declaration(label);
            assert_eq!(entity_kind(&fact), Ok(Some(expected)), "{label}");
            assert_eq!(structural_entity_kind(&fact), Some(expected), "{label}");
            assert!(
                structural_entity_classification_gap(&fact).is_none(),
                "{label}"
            );
        }
    }

    #[test]
    fn body_free_fsharp_contracts_do_not_infer_signature_callability() {
        for (label, source, expected) in [
            (
                "fsharp.signature_val.declaration",
                "val transform : int -> int",
                EntityKind::Variable,
            ),
            (
                "fsharp.signature_method.declaration",
                "abstract Transform : input:int -> int",
                EntityKind::Method,
            ),
            (
                "fsharp.signature_property.declaration",
                "abstract Count : int with get",
                EntityKind::Property,
            ),
        ] {
            let fact = declaration(label);
            assert_eq!(
                structural_entity_kind_from_source(&fact, source),
                Some(expected)
            );
            assert_eq!(structural_type_member_kind(&fact, expected), expected);
            assert!(structural_entity_classification_gap(&fact).is_none());
        }
    }

    #[test]
    fn ambiguous_and_unreviewed_fsharp_shapes_conserve_exact_entity_gaps() {
        for (label, detail) in [
            (
                "fsharp.ambiguous_type.declaration",
                "fsharp-type-representation-ambiguous",
            ),
            (
                "fsharp.signature_member.declaration",
                "fsharp-signature-member-shape-unresolved",
            ),
            (
                "fsharp.type.declaration",
                "fsharp-declaration-shape-unsupported",
            ),
            (
                "fsharp.unknown_constructor.declaration",
                "fsharp-declaration-shape-unsupported",
            ),
            (
                "fsharp.unsupported_binding.declaration",
                "fsharp-declaration-shape-unsupported",
            ),
            (
                "fsharp.signature_method.unrefined",
                "fsharp-declaration-shape-unsupported",
            ),
        ] {
            let fact = declaration(label);
            let gap = structural_entity_classification_gap(&fact).unwrap();
            assert_eq!(entity_kind(&fact), Err(gap), "{label}");
            assert_eq!(gap.span(), fact.span());
            assert_eq!(gap.detail(), detail);
            assert_eq!(gap.domain(), FactDomain::Entities);
            assert_eq!(gap.reason(), SkippedRegionReason::UnsupportedConstruct);
            assert_eq!(structural_entity_kind(&fact), None, "{label}");
            assert_eq!(
                structural_entity_kind_from_source(&fact, "type Alias = string"),
                None,
                "{label}"
            );
        }
    }

    #[test]
    fn fsharp_entity_policy_does_not_capture_other_languages_or_fact_roles() {
        for (label, expected) in [
            ("rust.function_item.declaration", EntityKind::Function),
            ("haskell.type_synonym.declaration", EntityKind::TypeAlias),
            ("fsharpish.type.declaration", EntityKind::TypeAlias),
        ] {
            let fact = declaration(label);
            assert_eq!(entity_kind(&fact), Ok(None));
            assert_eq!(structural_entity_kind(&fact), Some(expected));
            assert!(structural_entity_classification_gap(&fact).is_none());
        }
        for kind in [SyntaxFactKind::Occurrence, SyntaxFactKind::Module] {
            let fact = fact(
                kind,
                "fsharp.unknown.declaration",
                declaration("unused").span(),
            );
            assert_eq!(entity_kind(&fact), Ok(None));
            assert_eq!(structural_entity_kind(&fact), None);
            assert!(structural_entity_classification_gap(&fact).is_none());
        }
        let go = declaration("go.type.declaration");
        assert_eq!(
            structural_entity_kind_from_source(&go, "type Reader interface {}"),
            Some(EntityKind::Interface)
        );
        assert!(structural_entity_classification_gap(&go).is_none());
    }

    #[test]
    fn quoted_fsharp_definitions_retain_exact_byte_bounded_identity() {
        let definition = fact(
            SyntaxFactKind::Occurrence,
            "fsharp.name.definition",
            declaration("unused").span(),
        );
        for source in [
            "identity",
            "name'",
            "Λambda",
            "a\u{0301}",
            "``two names``",
            "``a `b` c``",
            "``Root.Name``",
            "Sample.``Root.Name``",
            "``first name``.last",
        ] {
            for language in ["fsharp", "fsharp-signature"] {
                let name =
                    structural_captured_name_for_fact(language, &definition, source, source.len());
                assert_eq!(name.as_deref(), Some(source), "{language}: {source}");
                assert!(matches!(name, Some(Cow::Borrowed(_))));
                assert!(
                    structural_captured_name_for_fact(
                        language,
                        &definition,
                        source,
                        source.len() - 1,
                    )
                    .is_none()
                );
            }
        }
        for source in [
            "",
            "````",
            "``name",
            "name``",
            "``a``tail",
            "``line\nname``",
            "a b",
            "a . b",
            ".a",
            "a.",
            "(+)",
            " name ",
            "path/name",
            "f(x)",
        ] {
            assert!(
                structural_captured_name_for_fact("fsharp", &definition, source, 128).is_none(),
                "{source:?}"
            );
        }
    }

    #[test]
    fn fsharp_operator_and_new_names_require_their_exact_definition_roles() {
        let span = declaration("unused").span();
        let operator = fact(
            SyntaxFactKind::Occurrence,
            "fsharp.operator_name.definition",
            span,
        );
        let name = fact(SyntaxFactKind::Occurrence, "fsharp.name.definition", span);
        for source in ["(+)", "( /+ )", "(?)", "($?)", "(.. ..)"] {
            for language in ["fsharp", "fsharp-signature"] {
                assert_eq!(
                    structural_captured_name_for_fact(language, &operator, source, source.len())
                        .as_deref(),
                    Some(source)
                );
                assert!(
                    structural_captured_name_for_fact(language, &name, source, source.len())
                        .is_none()
                );
                assert!(
                    structural_captured_name_for_fact(
                        language,
                        &operator,
                        source,
                        source.len() - 1,
                    )
                    .is_none()
                );
            }
        }
        for source in ["+", "()", "(foo)", "(+ $)", "($$)", "(\n+)", " (+)"] {
            assert!(
                structural_captured_name_for_fact("fsharp", &operator, source, 128).is_none(),
                "{source:?}"
            );
        }
        let new = fact(
            SyntaxFactKind::Occurrence,
            "fsharp.new_name.definition",
            span,
        );
        assert_eq!(
            structural_captured_name_for_fact("fsharp-signature", &new, "new", 3).as_deref(),
            Some("new")
        );
        for source in ["New", " new", "new ", "``new``"] {
            assert!(structural_captured_name_for_fact("fsharp", &new, source, 128).is_none());
        }
        assert!(structural_captured_name_for_fact("fsharp", &new, "new", 2).is_none());
    }

    #[test]
    fn fsharp_name_admission_does_not_loosen_shared_or_other_language_boundaries() {
        let span = declaration("unused").span();
        let definition = fact(SyntaxFactKind::Occurrence, "fsharp.name.definition", span);
        for source in ["``two names``", "(+)", "ordinary", "``simple``"] {
            for language in ["rust", "forth", "FSharp", "fsharp-signature-extra"] {
                assert_eq!(
                    structural_captured_name_for_fact(language, &definition, source, 128),
                    structural_captured_name_for_language(language, source, 128)
                );
            }
        }
        for language in ["fsharp", "fsharp-signature"] {
            for source in ["``two names``", "(+)"] {
                assert!(structural_captured_name_for_language(language, source, 128).is_none());
            }
            for label in [
                "fsharp.name.reference",
                "fsharp.name.definition.extra",
                "fsharpish.name.definition",
                "rust.name.definition",
            ] {
                let unreviewed = fact(SyntaxFactKind::Occurrence, label, span);
                assert!(
                    structural_captured_name_for_fact(language, &unreviewed, "ordinary", 128)
                        .is_none(),
                    "{language}: {label}"
                );
            }
            let wrong_role = fact(SyntaxFactKind::Declaration, "fsharp.name.definition", span);
            assert!(
                structural_captured_name_for_fact(language, &wrong_role, "``two names``", 128)
                    .is_none()
            );
        }
    }
}
