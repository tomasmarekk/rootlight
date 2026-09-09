//! Source-backed ownership for TypeScript infer declarations.
//! Native conditional-field captures connect an infer written in an extends
//! operand to its true branch, shared by structural and project lowering.

use std::collections::BTreeMap;

use rootlight_cancel::Cancellation;
use rootlight_ir::SourceSpan;

use crate::{AdapterError, SyntaxFact, SyntaxFactKind};

/// The native conditional scope and true-branch visibility of an infer binder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TypeScriptInferBinding {
    scope: u64,
    consequence: SourceSpan,
}

impl TypeScriptInferBinding {
    /// Returns the owning conditional scope's syntax-local identifier.
    pub const fn scope(self) -> u64 {
        self.scope
    }

    /// Returns the true-branch region; the binder also sees its own constraint.
    pub const fn consequence(self) -> SourceSpan {
        self.consequence
    }
}

/// Maps infer declaration IDs to complete, source-backed conditional ownership.
///
/// Accepts one already bounded syntax transaction. Missing or ambiguous branch
/// fields do not establish a binding; ancestry traversal is bounded by fact count.
/// Nested conditionals qualify only when their extends operand contains the infer.
///
/// # Errors
/// Returns cancellation errors without publishing a partial binding map.
pub fn typescript_infer_bindings(
    facts: &[SyntaxFact],
    cancellation: &Cancellation,
) -> Result<BTreeMap<u64, TypeScriptInferBinding>, AdapterError> {
    cancellation.check()?;
    let mut by_id = BTreeMap::new();
    let mut metadata = Vec::new();
    let mut declarations = Vec::new();
    for fact in facts {
        cancellation.check()?;
        match fact.syntax_kind().as_str() {
            "typescript.conditional_right.signature"
            | "typescript.conditional_consequence.signature"
                if fact.kind() == SyntaxFactKind::Signature =>
            {
                metadata.push(fact)
            }
            "typescript.infer_parameter.declaration"
                if fact.kind() == SyntaxFactKind::Declaration =>
            {
                declarations.push(fact)
            }
            _ => {}
        }
    }
    if declarations.is_empty() {
        return Ok(BTreeMap::new());
    }
    for fact in facts {
        cancellation.check()?;
        by_id.insert(fact.local_id(), fact);
    }
    let mut fields = BTreeMap::<(u64, bool), Vec<SourceSpan>>::new();
    for field in metadata {
        let mut parent = field.parent();
        for _ in 0..facts.len() {
            cancellation.check()?;
            let Some(ancestor) = parent.and_then(|id| by_id.get(&id).copied()) else {
                break;
            };
            if ancestor.syntax_kind().as_str() == "typescript.conditional.scope"
                && ancestor.kind() == SyntaxFactKind::Scope
                && ancestor.span() != field.span()
            {
                fields
                    .entry((
                        ancestor.local_id(),
                        field.syntax_kind().as_str() == "typescript.conditional_right.signature",
                    ))
                    .or_default()
                    .push(field.span());
                break;
            }
            parent = ancestor.parent();
        }
    }
    let mut bindings = BTreeMap::new();
    for declaration in declarations {
        let mut parent = declaration.parent();
        for _ in 0..facts.len() {
            cancellation.check()?;
            let Some(ancestor) = parent.and_then(|id| by_id.get(&id).copied()) else {
                break;
            };
            if ancestor.syntax_kind().as_str() == "typescript.conditional.scope" {
                let Some([right]) = fields.get(&(ancestor.local_id(), true)).map(Vec::as_slice)
                else {
                    // An incomplete inner scope cannot authorize a fallback to
                    // an outer condition with a coincidentally containing span.
                    break;
                };
                if !contains(ancestor.span(), *right) {
                    break;
                }
                if !contains(*right, declaration.span()) {
                    parent = ancestor.parent();
                    continue;
                }
                if let Some([consequence]) =
                    fields.get(&(ancestor.local_id(), false)).map(Vec::as_slice)
                    && contains(ancestor.span(), *consequence)
                    && right.end_byte() <= consequence.start_byte()
                {
                    bindings.insert(
                        declaration.local_id(),
                        TypeScriptInferBinding {
                            scope: ancestor.local_id(),
                            consequence: *consequence,
                        },
                    );
                }
                break;
            }
            parent = ancestor.parent();
        }
    }
    Ok(bindings)
}

fn contains(parent: SourceSpan, child: SourceSpan) -> bool {
    parent.file() == child.file()
        && parent.start_byte() <= child.start_byte()
        && parent.end_byte() >= child.end_byte()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SyntaxKindLabel;
    use rootlight_cancel::CancellationReason;
    use rootlight_ids::FileId;

    fn fact(
        id: u64,
        parent: Option<u64>,
        kind: SyntaxFactKind,
        start: u64,
        end: u64,
        label: &str,
    ) -> SyntaxFact {
        SyntaxFact::new(
            id,
            parent,
            kind,
            SourceSpan::new(FileId::from_bytes([3; 20]), start, end).unwrap(),
            0,
            SyntaxKindLabel::new(label).unwrap(),
        )
    }

    fn nested() -> Vec<SyntaxFact> {
        use SyntaxFactKind::{Declaration, Scope, Signature};
        vec![
            fact(1, None, Scope, 0, 100, "typescript.conditional.scope"),
            fact(2, Some(1), Scope, 10, 50, "typescript.conditional.scope"),
            fact(
                3,
                Some(2),
                Signature,
                10,
                50,
                "typescript.conditional_right.signature",
            ),
            fact(
                4,
                Some(2),
                Signature,
                20,
                30,
                "typescript.conditional_right.signature",
            ),
            fact(
                5,
                Some(2),
                Signature,
                35,
                40,
                "typescript.conditional_consequence.signature",
            ),
            fact(
                6,
                Some(1),
                Signature,
                60,
                80,
                "typescript.conditional_consequence.signature",
            ),
            fact(
                7,
                Some(4),
                Declaration,
                22,
                25,
                "typescript.infer_parameter.declaration",
            ),
            fact(
                8,
                Some(2),
                Declaration,
                12,
                15,
                "typescript.infer_parameter.declaration",
            ),
        ]
    }

    #[test]
    fn infer_ownership_uses_extends_fields_not_nearest_conditional() {
        let bindings = typescript_infer_bindings(&nested(), &Cancellation::new()).unwrap();
        assert_eq!(bindings.len(), 2);
        assert_eq!(bindings[&7].scope(), 2);
        assert_eq!(bindings[&7].consequence().start_byte(), 35);
        assert_eq!(bindings[&8].scope(), 1);
        assert_eq!(bindings[&8].consequence().start_byte(), 60);
    }

    #[test]
    fn incomplete_or_ambiguous_inner_fields_cannot_select_an_outer_binding() {
        for missing in [4, 5] {
            let mut facts = nested();
            facts.retain(|fact| fact.local_id() != missing);
            assert!(
                !typescript_infer_bindings(&facts, &Cancellation::new())
                    .unwrap()
                    .contains_key(&7)
            );
        }
        let mut ambiguous = nested();
        ambiguous.push(fact(
            9,
            Some(2),
            SyntaxFactKind::Signature,
            21,
            29,
            "typescript.conditional_right.signature",
        ));
        assert!(
            !typescript_infer_bindings(&ambiguous, &Cancellation::new())
                .unwrap()
                .contains_key(&7)
        );
        let mut outside = nested();
        outside.retain(|fact| fact.local_id() != 5);
        outside.push(fact(
            5,
            Some(2),
            SyntaxFactKind::Signature,
            90,
            95,
            "typescript.conditional_consequence.signature",
        ));
        assert!(
            !typescript_infer_bindings(&outside, &Cancellation::new())
                .unwrap()
                .contains_key(&7)
        );
    }

    #[test]
    fn inference_rejects_cancellation_and_bounds_broken_parent_cycles() {
        let cancellation = Cancellation::new();
        cancellation.cancel(CancellationReason::ClientRequest);
        assert!(typescript_infer_bindings(&[], &cancellation).is_err());
        let cyclic = [fact(
            1,
            Some(1),
            SyntaxFactKind::Declaration,
            0,
            1,
            "typescript.infer_parameter.declaration",
        )];
        assert!(
            typescript_infer_bindings(&cyclic, &Cancellation::new())
                .unwrap()
                .is_empty()
        );
    }
}
