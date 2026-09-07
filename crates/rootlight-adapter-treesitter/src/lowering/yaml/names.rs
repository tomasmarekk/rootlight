//! Constructs YAML key identities in their captured document and node context.
//! Native spans stay syntax evidence; block keys additionally retain every byte
//! needed for chomping, while unrecognized schema semantics remain explicit gaps.

use super::super::*;
use rootlight_adapter_sdk::{YamlBlockScalar, YamlDocumentContext};

pub(in super::super) struct Key {
    pub(in super::super) name: String,
    pub(in super::super) source: SourceSpan,
}

#[derive(Default)]
pub(in super::super) struct Names {
    pub(in super::super) keys: HashMap<u64, Option<Key>>,
    pub(in super::super) owners: HashMap<u64, u64>,
    pub(in super::super) warnings: Vec<(SourceSpan, &'static str)>,
}

#[derive(Default)]
struct Document<'a> {
    version: Option<&'a str>,
    tags: Vec<(&'a str, &'a str)>,
    invalid: bool,
}

impl Names {
    pub(in super::super) fn new(
        facts: &[SyntaxFact],
        source: &str,
        maximum: usize,
        cancellation: &Cancellation,
    ) -> Result<Self, AdapterError> {
        let mut ordered: Vec<_> = facts.iter().collect();
        crate::runtime::sort_cancellable_by(&mut ordered, cancellation, |left, right| {
            (left.depth(), left.local_id()).cmp(&(right.depth(), right.local_id()))
        })?;
        let mut documents = HashMap::<u64, Option<u64>>::new();
        let mut properties = HashMap::<u64, Option<SourceSpan>>::new();
        let mut property_ids = HashMap::<u64, Option<u64>>::new();
        let mut contexts = HashMap::<u64, Document<'_>>::new();
        let mut document_spans = HashMap::new();
        let mut result = Self::default();
        for fact in &ordered {
            cancellation.check()?;
            let mut document = fact
                .parent()
                .and_then(|parent| documents.get(&parent).copied().flatten());
            let mut property = fact
                .parent()
                .and_then(|parent| properties.get(&parent).copied().flatten());
            let mut property_id = fact
                .parent()
                .and_then(|parent| property_ids.get(&parent).copied().flatten());
            match fact.syntax_kind().as_str() {
                "yaml.document.scope" => {
                    document = Some(fact.local_id());
                    contexts.entry(fact.local_id()).or_default();
                    document_spans.insert(fact.local_id(), fact.span());
                }
                "yaml.property.declaration" => {
                    property = Some(fact.span());
                    property_id = Some(fact.local_id());
                }
                "yaml.key.definition"
                | "yaml.node_key.definition"
                | "yaml.empty_key.definition" => {
                    if let Some(owner) = property_id {
                        result.owners.insert(fact.local_id(), owner);
                    }
                }
                "yaml.version.signature"
                | "yaml.tag_directive.signature"
                | "yaml.reserved_directive.signature" => {
                    if let Some(document) = document {
                        let context = contexts.get_mut(&document).ok_or_else(invalid)?;
                        let text = text(source, fact.span())?;
                        if text.len() > maximum {
                            context.invalid = true;
                        } else {
                            let mut words = text.split_ascii_whitespace();
                            match (words.next(), words.next(), words.next(), words.next()) {
                                (Some("%YAML"), Some(version), None, None) => {
                                    if context.version.replace(version).is_some() {
                                        context.invalid = true;
                                    }
                                }
                                (Some("%TAG"), Some(handle), Some(prefix), None) => {
                                    context.tags.push((handle, prefix))
                                }
                                _ => context.invalid = true,
                            }
                        }
                    }
                }
                _ => {}
            }
            documents.insert(fact.local_id(), document);
            properties.insert(fact.local_id(), property);
            property_ids.insert(fact.local_id(), property_id);
        }
        let mut decoded = HashMap::new();
        for (id, context) in contexts {
            cancellation.check()?;
            let context = (!context.invalid)
                .then(|| YamlDocumentContext::new(context.version, &context.tags, maximum))
                .flatten();
            if context
                .as_ref()
                .is_none_or(YamlDocumentContext::has_version_warning)
            {
                result.warnings.push((
                    *document_spans.get(&id).ok_or_else(invalid)?,
                    "yaml-document-schema-uncertain",
                ));
            }
            decoded.insert(id, context);
        }
        for fact in ordered {
            cancellation.check()?;
            if !matches!(
                fact.syntax_kind().as_str(),
                "yaml.node_key.definition" | "yaml.empty_key.definition"
            ) {
                continue;
            }
            let key = documents
                .get(&fact.local_id())
                .copied()
                .flatten()
                .and_then(|id| decoded.get(&id))
                .and_then(Option::as_ref)
                .and_then(|context| {
                    let parent = properties.get(&fact.local_id()).copied().flatten()?;
                    let (scalar, span) = node_scalar(context, source, fact, parent, maximum)?;
                    if scalar.has_unrecognized_tag() {
                        result
                            .warnings
                            .push((span, "yaml-key-tag-semantics-unknown"));
                    }
                    Some(Key {
                        name: scalar.into_name(),
                        source: span,
                    })
                });
            result.keys.insert(fact.local_id(), key);
        }
        Ok(result)
    }
}

fn node_scalar(
    context: &YamlDocumentContext<'_>,
    source: &str,
    fact: &SyntaxFact,
    parent: SourceSpan,
    maximum: usize,
) -> Option<(rootlight_adapter_sdk::YamlScalarIdentity, SourceSpan)> {
    if fact.syntax_kind().as_str() == "yaml.empty_key.definition" {
        return Some((context.flow_scalar("", None)?, fact.span()));
    }
    let raw = text(source, fact.span()).ok()?;
    if raw.len() > maximum {
        return None;
    }
    let mut value = raw;
    let mut tag = None;
    let mut anchor = false;
    loop {
        let indicator = value.as_bytes().first().copied();
        if !matches!(indicator, Some(b'&' | b'!')) {
            break;
        }
        let end = value.find([' ', '\t', '\r', '\n']).unwrap_or(value.len());
        let property = value.get(..end)?;
        if indicator == Some(b'!') {
            if tag.replace(property).is_some() {
                return None;
            }
        } else {
            if anchor {
                return None;
            }
            anchor = true;
        }
        value = value
            .get(end..)?
            .trim_start_matches([' ', '\t', '\r', '\n']);
        while value.starts_with('#') {
            value = value
                .get(value.find(['\r', '\n']).unwrap_or(value.len())..)?
                .trim_start_matches([' ', '\t', '\r', '\n']);
        }
    }
    if value.starts_with(['|', '>']) {
        let start = usize::try_from(fact.span().start_byte())
            .ok()?
            .checked_add(raw.len().checked_sub(value.len())?)?;
        let end = usize::try_from(fact.span().end_byte()).ok()?;
        let block = YamlBlockScalar::parse(
            source,
            start..end,
            Some(column(source, parent.start_byte())?),
            maximum,
        )?;
        let span = SourceSpan::new(
            fact.span().file(),
            fact.span().start_byte(),
            u64::try_from(block.lexical_range().end).ok()?,
        )
        .ok()?;
        Some((context.block_scalar(&block, tag)?, span))
    } else {
        Some((context.flow_scalar(value, tag)?, fact.span()))
    }
}

fn column(source: &str, position: u64) -> Option<usize> {
    let end = usize::try_from(position).ok()?;
    let prefix = source.get(..end)?;
    Some(
        end - prefix
            .rfind(['\r', '\n'])
            .map_or(0, |position| position + 1),
    )
}

fn text(source: &str, span: SourceSpan) -> Result<&str, AdapterError> {
    let start = usize::try_from(span.start_byte()).map_err(|_| invalid())?;
    let end = usize::try_from(span.end_byte()).map_err(|_| invalid())?;
    source.get(start..end).ok_or_else(invalid)
}

fn invalid() -> AdapterError {
    provider_failure("treesitter-yaml-key-context")
}
