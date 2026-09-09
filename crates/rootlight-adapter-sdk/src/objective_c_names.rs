//! Objective-C selectors assembled from ordered, native source captures.
//! Parameter types and bindings lie between the captured parts, not in the name.

use crate::{SyntaxFact, SyntaxFactKind};

/// Assembles a written method selector without parsing its intervening header.
///
/// `parts` must be the complete, source-ordered selector captures from one
/// bounded syntax transaction. The parser must retain them atomically with the
/// definition. This boundary checks kinds, file/range membership, ordering and
/// outer endpoints; it cannot infer a missing interior capture from source text.
/// Both the enclosing source extent and result must fit `maximum_bytes`.
/// Invalid captures, invalid UTF-8 boundaries or allocation failure return `None`.
/// The caller keeps the original definition span as provenance, not the joined text.
#[must_use]
pub fn canonical_objective_c_selector(
    definition: &SyntaxFact,
    parts: &[&SyntaxFact],
    source: &str,
    maximum_bytes: usize,
) -> Option<String> {
    if definition.kind() != SyntaxFactKind::Occurrence
        || definition.syntax_kind().as_str() != "objective_c.method.definition"
    {
        return None;
    }
    let span = definition.span();
    let start = usize::try_from(span.start_byte()).ok()?;
    let end = usize::try_from(span.end_byte()).ok()?;
    let extent = source.get(start..end)?;
    if extent.is_empty() || extent.len() > maximum_bytes || parts.len() > extent.len() {
        return None;
    }
    let mut name = String::new();
    name.try_reserve_exact(extent.len()).ok()?;
    let mut previous_end = span.start_byte();
    let mut previous_label = false;
    let mut colon_seen = false;
    for (index, part) in parts.iter().enumerate() {
        let part_span = part.span();
        if part.kind() != SyntaxFactKind::Occurrence
            || part.syntax_kind().as_str() != "objective_c.selector.definition_part"
            || part_span.file() != span.file()
            || part_span.start_byte() < previous_end
            || part_span.end_byte() > span.end_byte()
            || (index == 0 && part_span.start_byte() != span.start_byte())
        {
            return None;
        }
        let text = source.get(
            usize::try_from(part_span.start_byte()).ok()?
                ..usize::try_from(part_span.end_byte()).ok()?,
        )?;
        if text == ":" {
            colon_seen = true;
            previous_label = false;
        } else {
            // Identifier spelling is grammar-reviewed. Whitespace or adjacent
            // labels would expose a malformed capture rather than a selector.
            if previous_label
                || text.is_empty()
                || text
                    .chars()
                    .any(|ch| ch.is_whitespace() || ch.is_control() || ch == ':')
            {
                return None;
            }
            previous_label = true;
        }
        name.push_str(text);
        previous_end = part_span.end_byte();
    }
    (previous_end == span.end_byte() && !name.is_empty() && !(colon_seen && previous_label))
        .then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SyntaxKindLabel;
    use rootlight_ids::FileId;
    use rootlight_ir::SourceSpan;

    fn fact(file: u8, start: u64, end: u64, definition: bool) -> SyntaxFact {
        SyntaxFact::new(
            1,
            None,
            SyntaxFactKind::Occurrence,
            SourceSpan::new(FileId::from_bytes([file; 20]), start, end).unwrap(),
            0,
            SyntaxKindLabel::new(if definition {
                "objective_c.method.definition"
            } else {
                "objective_c.selector.definition_part"
            })
            .unwrap(),
        )
    }

    #[test]
    fn selector_joins_only_authored_labels_and_colons() {
        for (source, ranges, expected) in [
            (
                "add:(int)x to:",
                vec![(0, 3), (3, 4), (11, 13), (13, 14)],
                "add:to:",
            ),
            (":(int)x :", vec![(0, 1), (8, 9)], "::"),
            ("value", vec![(0, 5)], "value"),
            ("méthode:", vec![(0, 8), (8, 9)], "méthode:"),
        ] {
            let definition = fact(1, 0, u64::try_from(source.len()).unwrap(), true);
            let parts: Vec<_> = ranges
                .into_iter()
                .map(|(start, end)| fact(1, start, end, false))
                .collect();
            let refs: Vec<_> = parts.iter().collect();
            assert_eq!(
                canonical_objective_c_selector(&definition, &refs, source, source.len()).as_deref(),
                Some(expected)
            );
            assert_eq!(
                canonical_objective_c_selector(&definition, &refs, source, source.len() - 1),
                None
            );
        }
    }

    #[test]
    fn selector_rejects_reordered_overlapping_foreign_and_incomplete_ranges() {
        let source = "add:(int)x to:";
        let definition = fact(1, 0, 14, true);
        let parts = [
            fact(1, 0, 3, false),
            fact(1, 3, 4, false),
            fact(1, 11, 13, false),
            fact(1, 13, 14, false),
        ];
        let foreign = fact(2, 0, 3, false);
        let overlap = fact(1, 2, 4, false);
        let adjacent_label = fact(1, 9, 10, false);
        for refs in [
            vec![],
            vec![&parts[0]],
            vec![&parts[1], &parts[0], &parts[2], &parts[3]],
            vec![&foreign, &parts[1], &parts[2], &parts[3]],
            vec![&parts[0], &overlap, &parts[2], &parts[3]],
            vec![&parts[0], &parts[1], &adjacent_label, &parts[2], &parts[3]],
            vec![&parts[0], &parts[1], &parts[2], &definition],
        ] {
            assert!(canonical_objective_c_selector(&definition, &refs, source, 1024).is_none());
        }
        assert!(canonical_objective_c_selector(&fact(1, 0, 15, true), &[], source, 1024).is_none());
        assert!(
            canonical_objective_c_selector(&definition, &[&parts[0], &parts[1]], source, 0)
                .is_none()
        );
    }
}
