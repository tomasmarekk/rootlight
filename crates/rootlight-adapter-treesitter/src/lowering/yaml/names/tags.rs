//! Reports tag-construction gaps even when a node is not needed for key identity.
//! Collection tags are checked against their syntax kind without hashing their
//! contents or expanding aliases; scalar checks reuse the bounded Core decoder.

use super::*;
use rootlight_adapter_sdk::YamlCollectionKind;

pub(super) struct Input<'a, 'source> {
    pub(super) ordered: &'a [&'a SyntaxFact],
    pub(super) source: &'source str,
    pub(super) maximum: usize,
    pub(super) documents: &'a HashMap<u64, Option<u64>>,
    pub(super) contexts: &'a HashMap<u64, Option<YamlDocumentContext<'source>>>,
    pub(super) parents: &'a HashMap<u64, Option<SourceSpan>>,
}

pub(super) fn validate(
    input: Input<'_, '_>,
    warnings: &mut Vec<(SourceSpan, &'static str)>,
    cancellation: &Cancellation,
) -> Result<(), AdapterError> {
    let mut has_tag = false;
    for fact in input.ordered {
        cancellation.check()?;
        has_tag |= fact.syntax_kind().as_str() == "yaml.tag.signature";
    }
    if !has_tag {
        return Ok(());
    }
    let mut nearest = HashMap::<u64, Option<u64>>::new();
    let mut nodes = HashMap::new();
    let mut kinds = HashMap::new();
    let mut tagged = Vec::new();
    for fact in input.ordered {
        cancellation.check()?;
        let mut parent = fact
            .parent()
            .and_then(|id| nearest.get(&id).copied().flatten());
        match fact.syntax_kind().as_str() {
            "yaml.document.scope" => parent = None,
            "yaml.node.scope" | "yaml.key.scope" | "yaml.sequence_element.scope" => {
                parent = Some(fact.local_id());
                nodes.insert(fact.local_id(), *fact);
            }
            "yaml.mapping.scope" | "yaml.sequence.scope" => {
                if let Some(parent) = parent {
                    let kind = if fact.syntax_kind().as_str() == "yaml.mapping.scope" {
                        YamlCollectionKind::Mapping
                    } else {
                        YamlCollectionKind::Sequence
                    };
                    kinds.insert(parent, kind);
                }
            }
            "yaml.tag.signature" => {
                tagged
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                tagged.push((parent, fact.span()));
            }
            _ => {}
        }
        nearest.insert(fact.local_id(), parent);
    }
    for (owner, tag_span) in tagged {
        cancellation.check()?;
        let Some(owner) = owner else {
            warnings.push((tag_span, "yaml-node-tag-construction-unavailable"));
            continue;
        };
        let Some(context) = input
            .documents
            .get(&owner)
            .copied()
            .flatten()
            .and_then(|document| input.contexts.get(&document))
            .and_then(Option::as_ref)
        else {
            // Invalid document context already carries a document-scoped gap.
            continue;
        };
        let node = nodes.get(&owner).ok_or_else(invalid)?;
        let constructed = if let Some(kind) = kinds.get(&owner) {
            context
                .collection_tag(*kind, Some(text(input.source, tag_span)?))
                .map(|tag| (tag.has_unrecognized_tag(), node.span()))
        } else {
            node_scalar(
                context,
                input.source,
                node,
                input.parents.get(&owner).copied().flatten(),
                input.maximum,
            )
            .map(|(scalar, span)| (scalar.has_unrecognized_tag(), span))
        };
        let warning = match constructed {
            Some((false, _)) => None,
            Some((true, span)) => Some((span, "yaml-node-tag-semantics-unknown")),
            None => Some((node.span(), "yaml-node-tag-construction-unavailable")),
        };
        if let Some(warning) = warning {
            warnings
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            warnings.push(warning);
        }
    }
    Ok(())
}
