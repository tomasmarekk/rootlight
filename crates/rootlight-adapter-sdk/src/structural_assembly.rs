//! Assembly classifications describe written owners, not assembled symbols.
//! Native callers must prove complete owner/name captures and matching closers.

use rootlight_ir::EntityKind;

use crate::{SyntaxFact, SyntaxFactKind};

pub(super) fn entity_kind(fact: &SyntaxFact) -> Option<EntityKind> {
    if fact.kind() == SyntaxFactKind::Module
        && fact.syntax_kind().as_str() == "assembly.file.module"
    {
        return Some(EntityKind::Module);
    }
    if fact.kind() != SyntaxFactKind::Declaration {
        return None;
    }
    match fact.syntax_kind().as_str() {
        "assembly.procedure.declaration" | "assembly.macro.declaration" => {
            Some(EntityKind::Function)
        }
        // A segment is a written container, not a namespace or resolved address.
        "assembly.segment.declaration" => Some(EntityKind::Module),
        "assembly.constant.declaration" => Some(EntityKind::Constant),
        "assembly.data.declaration" => Some(EntityKind::Variable),
        "assembly.source_label.declaration" => Some(EntityKind::StatementLabel),
        "assembly.source_constant.declaration" => Some(EntityKind::Constant),
        _ => None,
    }
}

/// Borrows one complete MASM identifier without case folding or macro expansion.
///
/// The caller proves its physical declaration context; this lexical boundary
/// does not establish reserved-word acceptance, binding or dialect equivalence.
/// The ASCII character and 247-character boundaries follow the MASM `id` rule:
/// <https://learn.microsoft.com/en-us/cpp/assembler/masm/masm-bnf-grammar>.
pub(super) fn captured_name(text: &str, maximum_bytes: usize) -> Option<&str> {
    let alpha = |byte: u8| byte.is_ascii_alphabetic() || matches!(byte, b'@' | b'_' | b'$' | b'?');
    let bytes = text.as_bytes();
    (bytes.len() <= maximum_bytes.min(247)
        && bytes.first().is_some_and(|byte| alpha(*byte))
        && bytes
            .iter()
            .all(|byte| alpha(*byte) || byte.is_ascii_digit()))
    .then_some(text)
}

pub(super) fn captured_name_for_fact<'a>(
    fact: &SyntaxFact,
    text: &'a str,
    maximum_bytes: usize,
) -> Option<&'a str> {
    match fact.syntax_kind().as_str() {
        "assembly.masm_name.definition" => captured_name(text, maximum_bytes),
        "assembly.source_label.definition" => source_name(text, maximum_bytes, true),
        "assembly.source_constant.definition" => source_name(text, maximum_bytes, false),
        _ => None,
    }
}

/// Retains the reviewed native label/word subset without dialect normalization.
///
/// Numeric labels need directional occurrence proof, and `.` is the location
/// counter rather than an ordinary symbol. Neither is admitted by this boundary.
/// Native callers distinguish labels/constants from instruction and operand names;
/// this is not the complete GNU assembler symbol grammar or assembled identity.
/// <https://sourceware.org/binutils/docs/as/Symbol-Names.html>.
fn source_name(text: &str, maximum_bytes: usize, dotted: bool) -> Option<&str> {
    let first = |byte: u8| byte.is_ascii_alphabetic() || byte == b'_' || (dotted && byte == b'.');
    let bytes = text.as_bytes();
    (bytes.len() <= maximum_bytes
        && text != "."
        && bytes.first().is_some_and(|byte| first(*byte))
        && bytes
            .iter()
            .all(|byte| first(*byte) || byte.is_ascii_digit()))
    .then_some(text)
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use rootlight_ids::FileId;
    use rootlight_ir::{EntityKind, SourceSpan};

    use crate::{
        SyntaxFact, SyntaxFactKind, SyntaxKindLabel, structural_captured_name_for_fact,
        structural_captured_name_for_language, structural_entity_kind,
        structural_entity_kind_from_source,
    };

    fn fact(kind: SyntaxFactKind, label: &str) -> SyntaxFact {
        SyntaxFact::new(
            1,
            None,
            kind,
            SourceSpan::new(FileId::from_bytes([7; 20]), 0, 1).unwrap(),
            0,
            SyntaxKindLabel::new(label).unwrap(),
        )
    }

    #[test]
    fn assembly_exact_written_owners_have_distinct_kinds() {
        for (label, kind, expected) in [
            (
                "assembly.file.module",
                SyntaxFactKind::Module,
                EntityKind::Module,
            ),
            (
                "assembly.procedure.declaration",
                SyntaxFactKind::Declaration,
                EntityKind::Function,
            ),
            (
                "assembly.macro.declaration",
                SyntaxFactKind::Declaration,
                EntityKind::Function,
            ),
            (
                "assembly.segment.declaration",
                SyntaxFactKind::Declaration,
                EntityKind::Module,
            ),
            (
                "assembly.constant.declaration",
                SyntaxFactKind::Declaration,
                EntityKind::Constant,
            ),
            (
                "assembly.data.declaration",
                SyntaxFactKind::Declaration,
                EntityKind::Variable,
            ),
        ] {
            let owner = fact(kind, label);
            assert_eq!(structural_entity_kind(&owner), Some(expected), "{label}");
            assert_eq!(
                structural_entity_kind_from_source(&owner, "written source"),
                Some(expected)
            );
            for wrong in [
                SyntaxFactKind::Root,
                SyntaxFactKind::Scope,
                SyntaxFactKind::Occurrence,
            ] {
                assert_eq!(
                    structural_entity_kind(&fact(wrong, label)),
                    None,
                    "{label}: {wrong:?}"
                );
            }
        }
    }

    #[test]
    fn assembly_unknown_labels_do_not_use_shared_substring_classification() {
        for label in [
            "assembly.unknown_function.declaration",
            "assembly.procedure.declaration.extra",
            "assembly.label.declaration",
            "assembly.instruction.declaration",
            "assembly.register.declaration",
            "assembly.conditional.module",
        ] {
            for kind in [SyntaxFactKind::Declaration, SyntaxFactKind::Module] {
                assert_eq!(structural_entity_kind(&fact(kind, label)), None, "{label}");
            }
        }
        assert_eq!(
            structural_entity_kind(&fact(SyntaxFactKind::Declaration, "assembly.file.module")),
            None
        );
        assert_eq!(
            structural_entity_kind(&fact(
                SyntaxFactKind::Module,
                "assembly.procedure.declaration"
            )),
            None
        );
        assert_eq!(
            structural_entity_kind(&fact(
                SyntaxFactKind::Declaration,
                "rust.function.declaration"
            )),
            Some(EntityKind::Function)
        );
    }

    #[test]
    fn assembly_masm_names_preserve_exact_spelling_and_bounds() {
        let definition = fact(SyntaxFactKind::Occurrence, "assembly.masm_name.definition");
        for name in [
            "Mixed", "mixed", "_entry2", "@Local", "$storage", "?mark", "$_@?9",
        ] {
            assert_eq!(
                structural_captured_name_for_language("assembly", name, name.len()),
                Some(Cow::Borrowed(name))
            );
            assert_eq!(
                structural_captured_name_for_fact("assembly", &definition, name, name.len()),
                Some(Cow::Borrowed(name))
            );
            assert!(
                structural_captured_name_for_language("assembly", name, name.len() - 1).is_none()
            );
        }
        assert_ne!(
            structural_captured_name_for_language("assembly", "Mixed", 64),
            structural_captured_name_for_language("assembly", "mixed", 64)
        );
        let longest = "a".repeat(247);
        assert_eq!(
            structural_captured_name_for_language("assembly", &longest, 247),
            Some(Cow::Borrowed(longest.as_str()))
        );
        let too_long = "a".repeat(248);
        assert!(structural_captured_name_for_language("assembly", &too_long, 248).is_none());
        for name in [
            "",
            " name",
            "name ",
            "two names",
            "1name",
            "name\0tail",
            "name\n",
            "%eax",
            "[value]",
            "name:part",
            ".local",
            "name.part",
            "name\\tail",
            "λ",
        ] {
            assert!(
                structural_captured_name_for_language("assembly", name, 64).is_none(),
                "{name:?}"
            );
            assert!(
                structural_captured_name_for_fact("assembly", &definition, name, 64).is_none(),
                "{name:?}"
            );
        }
    }

    #[test]
    fn assembly_name_capture_requires_exact_masm_label_role_and_language() {
        for label in [
            "assembly.name.definition",
            "assembly.masm_name.reference",
            "assembly.masm_name.definition.extra",
            "rust.name.definition",
        ] {
            assert!(
                structural_captured_name_for_fact(
                    "assembly",
                    &fact(SyntaxFactKind::Occurrence, label),
                    "name",
                    64
                )
                .is_none(),
                "{label}"
            );
        }
        for kind in [
            SyntaxFactKind::Root,
            SyntaxFactKind::Module,
            SyntaxFactKind::Declaration,
            SyntaxFactKind::Scope,
        ] {
            assert!(
                structural_captured_name_for_fact(
                    "assembly",
                    &fact(kind, "assembly.masm_name.definition"),
                    "name",
                    64
                )
                .is_none()
            );
        }
        for language in ["rust", "asm", "masm", "ASSEMBLY"] {
            assert!(
                structural_captured_name_for_fact(
                    language,
                    &fact(SyntaxFactKind::Occurrence, "assembly.masm_name.definition"),
                    "name",
                    64
                )
                .is_none()
            );
        }
    }

    #[test]
    fn assembly_source_labels_and_constants_keep_physical_kinds() {
        for (label, expected) in [
            (
                "assembly.source_label.declaration",
                EntityKind::StatementLabel,
            ),
            ("assembly.source_constant.declaration", EntityKind::Constant),
        ] {
            assert_eq!(
                structural_entity_kind(&fact(SyntaxFactKind::Declaration, label)),
                Some(expected),
                "{label}"
            );
            for kind in [
                SyntaxFactKind::Module,
                SyntaxFactKind::Scope,
                SyntaxFactKind::Occurrence,
            ] {
                assert_eq!(structural_entity_kind(&fact(kind, label)), None);
            }
            assert_eq!(
                structural_entity_kind(&fact(
                    SyntaxFactKind::Declaration,
                    &format!("{label}.extra")
                )),
                None
            );
        }
    }

    #[test]
    fn assembly_source_name_captures_do_not_inherit_masm_or_numeric_binding() {
        let label = fact(
            SyntaxFactKind::Occurrence,
            "assembly.source_label.definition",
        );
        let constant = fact(
            SyntaxFactKind::Occurrence,
            "assembly.source_constant.definition",
        );
        for name in ["entry", "Entry", "local.label", ".label", ".L1", "_entry2"] {
            assert_eq!(
                structural_captured_name_for_fact("assembly", &label, name, name.len()),
                Some(Cow::Borrowed(name))
            );
            assert!(
                structural_captured_name_for_fact("assembly", &label, name, name.len() - 1)
                    .is_none()
            );
        }
        for name in ["mask", "Mask", "_value2"] {
            assert_eq!(
                structural_captured_name_for_fact("assembly", &constant, name, name.len()),
                Some(Cow::Borrowed(name))
            );
        }
        for name in [
            "",
            ".",
            "1",
            "1f",
            "1b",
            " entry",
            "entry ",
            "entry:",
            "two words",
            "%eax",
            "$entry",
            "@entry",
            "?entry",
            "λ",
            "a\0b",
        ] {
            assert!(
                structural_captured_name_for_fact("assembly", &label, name, 64).is_none(),
                "{name:?}"
            );
            assert!(
                structural_captured_name_for_fact("assembly", &constant, name, 64).is_none(),
                "{name:?}"
            );
        }
        for name in ["local.label", ".label"] {
            assert!(structural_captured_name_for_fact("assembly", &constant, name, 64).is_none());
        }
        for syntax in [
            "assembly.source_label.definition",
            "assembly.source_constant.definition",
        ] {
            for kind in [
                SyntaxFactKind::Declaration,
                SyntaxFactKind::Module,
                SyntaxFactKind::Scope,
            ] {
                assert!(
                    structural_captured_name_for_fact("assembly", &fact(kind, syntax), "entry", 64)
                        .is_none()
                );
            }
            assert!(
                structural_captured_name_for_fact(
                    "rust",
                    &fact(SyntaxFactKind::Occurrence, syntax),
                    "entry",
                    64
                )
                .is_none()
            );
        }
        assert!(
            structural_captured_name_for_fact(
                "assembly",
                &fact(
                    SyntaxFactKind::Occurrence,
                    "assembly.source_label.reference"
                ),
                "entry",
                64
            )
            .is_none()
        );
        // A label-specific native proof never broadens the language-only fallback.
        assert!(structural_captured_name_for_language("assembly", ".label", 64).is_none());
    }
}
