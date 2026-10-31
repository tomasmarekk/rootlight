//! Admit only native-certified SPARQL prefixes and explicit AS alias names.
//! Variable identities use the bare name while source evidence retains its sigil.

use super::*;

pub(super) fn kind(fact: &SyntaxFact) -> Option<EntityKind> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "sparql.file.module") => Some(EntityKind::Module),
        (SyntaxFactKind::Declaration, "sparql.prefix.declaration") => Some(EntityKind::Namespace),
        (SyntaxFactKind::Declaration, "sparql.alias.declaration") => Some(EntityKind::Variable),
        _ => None,
    }
}

pub(super) fn name<'a>(fact: &SyntaxFact, text: &'a str, maximum: usize) -> Option<Cow<'a, str>> {
    if fact.kind() != SyntaxFactKind::Occurrence || text.len() > maximum {
        return None;
    }
    let valid = match fact.syntax_kind().as_str() {
        "sparql.prefix.definition" => {
            let mut chars = text.chars();
            chars.next().is_some_and(pn_base)
                && chars.all(|ch| pn_char(ch) || ch == '.')
                && !text.ends_with('.')
        }
        "sparql.alias.definition" => {
            let bare = text.strip_prefix('?').or_else(|| text.strip_prefix('$'))?;
            let mut chars = bare.chars();
            if chars.next().is_some_and(var_start) && chars.all(var_rest) {
                return Some(Cow::Borrowed(bare));
            }
            false
        }
        _ => false,
    };
    valid.then_some(Cow::Borrowed(text))
}

fn pn_base(ch: char) -> bool {
    ch.is_ascii_alphabetic()
        || matches!(ch,
            '\u{00c0}'..='\u{00d6}' | '\u{00d8}'..='\u{00f6}' | '\u{00f8}'..='\u{02ff}'
            | '\u{0370}'..='\u{037d}' | '\u{037f}'..='\u{1fff}' | '\u{200c}'..='\u{200d}'
            | '\u{2070}'..='\u{218f}' | '\u{2c00}'..='\u{2fef}' | '\u{3001}'..='\u{d7ff}'
            | '\u{f900}'..='\u{fdcf}' | '\u{fdf0}'..='\u{fffd}' | '\u{10000}'..='\u{effff}')
}

fn var_start(ch: char) -> bool {
    pn_base(ch) || ch == '_' || ch.is_ascii_digit()
}

fn var_rest(ch: char) -> bool {
    var_start(ch) || matches!(ch, '\u{00b7}' | '\u{0300}'..='\u{036f}' | '\u{203f}'..='\u{2040}')
}

fn pn_char(ch: char) -> bool {
    var_rest(ch) || ch == '-'
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_ids::FileId;
    use rootlight_ir::SourceSpan;

    #[test]
    fn sparql_entity_labels_never_fall_through_to_generic_alias_or_class_kinds() {
        for (role, label, expected) in [
            (
                SyntaxFactKind::Module,
                "sparql.file.module",
                Some(EntityKind::Module),
            ),
            (
                SyntaxFactKind::Declaration,
                "sparql.prefix.declaration",
                Some(EntityKind::Namespace),
            ),
            (
                SyntaxFactKind::Declaration,
                "sparql.alias.declaration",
                Some(EntityKind::Variable),
            ),
            (
                SyntaxFactKind::Declaration,
                "sparql.fake_class.declaration",
                None,
            ),
            (SyntaxFactKind::Occurrence, "sparql.alias.definition", None),
        ] {
            let fact = SyntaxFact::new(
                1,
                None,
                role,
                SourceSpan::new(FileId::from_bytes([3; 20]), 0, 4).unwrap(),
                0,
                crate::SyntaxKindLabel::new(label).unwrap(),
            );
            assert_eq!(super::super::structural_entity_kind(&fact), expected);
        }
    }

    #[test]
    fn sparql_names_keep_full_written_bounds_and_canonical_bare_variables() {
        for (language, role, label, text, maximum, expected) in [
            (
                "sparql",
                SyntaxFactKind::Occurrence,
                "sparql.prefix.definition",
                "ex",
                2,
                Some("ex"),
            ),
            (
                "sparql",
                SyntaxFactKind::Occurrence,
                "sparql.prefix.definition",
                "食",
                3,
                Some("食"),
            ),
            (
                "sparql",
                SyntaxFactKind::Occurrence,
                "sparql.prefix.definition",
                "0prefix",
                20,
                None,
            ),
            (
                "sparql",
                SyntaxFactKind::Occurrence,
                "sparql.prefix.definition",
                "ex.",
                20,
                None,
            ),
            (
                "sparql",
                SyntaxFactKind::Occurrence,
                "sparql.prefix.definition",
                "",
                20,
                None,
            ),
            (
                "sparql",
                SyntaxFactKind::Occurrence,
                "sparql.prefix.definition",
                "ex:",
                20,
                None,
            ),
            (
                "sparql",
                SyntaxFactKind::Occurrence,
                "sparql.alias.definition",
                "?x",
                2,
                Some("x"),
            ),
            (
                "sparql",
                SyntaxFactKind::Occurrence,
                "sparql.alias.definition",
                "$x",
                2,
                Some("x"),
            ),
            (
                "sparql",
                SyntaxFactKind::Occurrence,
                "sparql.alias.definition",
                "?0",
                2,
                Some("0"),
            ),
            (
                "sparql",
                SyntaxFactKind::Occurrence,
                "sparql.alias.definition",
                "$1",
                2,
                Some("1"),
            ),
            (
                "sparql",
                SyntaxFactKind::Occurrence,
                "sparql.alias.definition",
                "?食",
                4,
                Some("食"),
            ),
            (
                "sparql",
                SyntaxFactKind::Occurrence,
                "sparql.alias.definition",
                "?x·",
                4,
                Some("x·"),
            ),
            (
                "sparql",
                SyntaxFactKind::Occurrence,
                "sparql.alias.definition",
                "?·",
                4,
                None,
            ),
            (
                "sparql",
                SyntaxFactKind::Occurrence,
                "sparql.alias.definition",
                "?x-y",
                20,
                None,
            ),
            (
                "sparql",
                SyntaxFactKind::Occurrence,
                "sparql.alias.definition",
                "?x.y",
                20,
                None,
            ),
            (
                "sparql",
                SyntaxFactKind::Occurrence,
                "sparql.alias.definition",
                "?x:y",
                20,
                None,
            ),
            (
                "sparql",
                SyntaxFactKind::Occurrence,
                "sparql.alias.definition",
                "?x",
                1,
                None,
            ),
            (
                "sparql",
                SyntaxFactKind::Occurrence,
                "sparql.alias.definition",
                "?",
                20,
                None,
            ),
            (
                "sparql",
                SyntaxFactKind::Occurrence,
                "sparql.alias.definition",
                "x",
                20,
                None,
            ),
            (
                "sparql",
                SyntaxFactKind::Occurrence,
                "sparql.alias.reference",
                "?x",
                20,
                None,
            ),
            (
                "sparql",
                SyntaxFactKind::Signature,
                "sparql.alias.definition",
                "?x",
                20,
                None,
            ),
            (
                "n3",
                SyntaxFactKind::Occurrence,
                "sparql.alias.definition",
                "?x",
                20,
                None,
            ),
        ] {
            let fact = SyntaxFact::new(
                1,
                None,
                role,
                SourceSpan::new(FileId::from_bytes([3; 20]), 0, 4).unwrap(),
                0,
                crate::SyntaxKindLabel::new(label).unwrap(),
            );
            assert_eq!(
                super::super::structural_captured_name_for_fact(language, &fact, text, maximum)
                    .as_deref(),
                expected,
                "{language} {label} {text:?}"
            );
        }
    }
}
