//! Written signature names declare the same function as its owned equation.

use std::collections::HashMap;

use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ir::{EntityKind, SourceSpan};

use super::{MaterializedEntity, span_contains};

pub(super) const PAIR_UNAVAILABLE: &str = "agda-signature-equation-identity-unavailable";

pub(super) fn signature_name(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Occurrence
        && fact.syntax_kind().as_str() == "agda.function.signature_name.declaration"
}

#[derive(Default)]
struct Witnesses {
    names: usize,
    definitions: usize,
    signatures: usize,
    signature: Option<u64>,
    definition: Option<u64>,
}

#[derive(Default)]
pub(super) struct SignatureNames {
    owners: HashMap<u64, Witnesses>,
    lhs_names: HashMap<SourceSpan, Vec<(u64, u64)>>,
}

impl SignatureNames {
    pub(super) fn new(
        facts: &[SyntaxFact],
        source_text: &str,
        cancellation: &Cancellation,
    ) -> Result<Self, AdapterError> {
        let mut owners = HashMap::<u64, Witnesses>::new();
        let mut lhs_names = HashMap::new();
        lhs_names
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        owners
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for (index, fact) in facts.iter().enumerate() {
            if index % 64 == 0 {
                cancellation.check()?;
            }
            let signature = fact.kind() == SyntaxFactKind::Signature
                && fact.syntax_kind().as_str() == "agda.function.signature";
            let definition = fact.kind() == SyntaxFactKind::Occurrence
                && fact.syntax_kind().as_str() == "agda.function.definition";
            if !signature && !definition && !signature_name(fact) {
                continue;
            }
            let Some(parent) = fact.parent() else {
                continue;
            };
            let witnesses = owners.entry(parent).or_default();
            if signature_name(fact) {
                witnesses.names += 1;
            }
            if signature {
                witnesses.signatures += 1;
                witnesses.signature = Some(fact.local_id());
                if let std::collections::hash_map::Entry::Vacant(entry) =
                    lhs_names.entry(fact.span())
                {
                    entry.insert(signature_lhs_names(fact.span(), source_text, cancellation)?);
                }
            }
            if definition {
                witnesses.definitions += 1;
                witnesses.definition = Some(fact.local_id());
            }
        }
        Ok(Self { owners, lhs_names })
    }

    pub(super) fn target<'a>(
        &self,
        fact: &SyntaxFact,
        facts: &HashMap<u64, &SyntaxFact>,
        entities: &'a HashMap<u64, MaterializedEntity>,
    ) -> Option<&'a MaterializedEntity> {
        if !signature_name(fact) {
            return None;
        }
        let parent = fact.parent()?;
        let witnesses = self.owners.get(&parent)?;
        if (witnesses.names, witnesses.signatures, witnesses.definitions) != (1, 1, 1) {
            return None;
        }
        let declaration = facts.get(&parent)?;
        let signature = facts.get(&witnesses.signature?)?;
        let definition = facts.get(&witnesses.definition?)?;
        let entity = entities.get(&parent)?;
        (declaration.kind() == SyntaxFactKind::Declaration
            && declaration.syntax_kind().as_str() == "agda.function.declaration"
            && matches!(
                entity.record.kind,
                EntityKind::Function | EntityKind::Method
            )
            && entity.record.language == "agda"
            && entity.definition_local_id == Some(definition.local_id())
            && entity.signature_span == Some(signature.span())
            && span_contains(declaration.span(), signature.span())
            && span_contains(declaration.span(), definition.span())
            && span_contains(signature.span(), fact.span())
            && self.lhs_names.get(&signature.span()).is_some_and(|names| {
                names
                    .binary_search(&(fact.span().start_byte(), fact.span().end_byte()))
                    .is_ok()
            })
            && signature.span().end_byte() <= definition.span().start_byte()
            && fact.span().end_byte() > fact.span().start_byte())
        .then_some(entity)
    }
}

// Agda's first-party provider accepts UTF-8. Physical UTF-8/BOM spans therefore
// index this source view directly; type names after ':' cannot declare a function.
fn signature_lhs_names(
    span: SourceSpan,
    source: &str,
    cancellation: &Cancellation,
) -> Result<Vec<(u64, u64)>, AdapterError> {
    let Some(text) = usize::try_from(span.start_byte())
        .ok()
        .zip(usize::try_from(span.end_byte()).ok())
        .and_then(|(start, end)| source.get(start..end))
    else {
        return Ok(Vec::new());
    };
    let mut names = Vec::new();
    let mut token = None;
    let mut invalid = false;
    let mut checkpoint = 0;
    for (offset, character) in text
        .char_indices()
        .chain(std::iter::once((text.len(), ' ')))
    {
        if offset >= checkpoint {
            cancellation.check()?;
            checkpoint = offset.saturating_add(64);
        }
        if character.is_ascii_whitespace() {
            if let Some(start) = token.take() {
                let word = &text[start..offset];
                if word == ":" {
                    return Ok(names);
                }
                if invalid || matches!(word, "_" | "λ" | "∀" | "→") {
                    return Ok(Vec::new());
                }
                names
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                names.push((
                    span.start_byte() + start as u64,
                    span.start_byte() + offset as u64,
                ));
            }
            invalid = false;
        } else {
            token.get_or_insert(offset);
            invalid |= character.is_control()
                || character.is_whitespace()
                || matches!(character, '.' | ';' | '{' | '}' | '(' | ')' | '@' | '"');
        }
    }
    Ok(Vec::new())
}
