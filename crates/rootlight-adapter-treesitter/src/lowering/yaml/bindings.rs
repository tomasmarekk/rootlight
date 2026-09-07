//! Resolves YAML serialization names to earlier same-document anchor captures.
//! Bindings reference declaration IDs; alias graphs are never recursively
//! expanded, and missing or ambiguous definitions still shadow earlier names.

use super::*;

#[derive(Clone, Copy)]
pub(super) struct Document {
    pub(super) ordinal: u64,
    pub(super) module: Option<u64>,
}

#[derive(Clone, Copy)]
pub(super) struct Anchor {
    pub(super) document: Document,
    pub(super) ordinal: u64,
}

pub(super) struct Bindings {
    pub(super) anchors: HashMap<u64, Anchor>,
    pub(super) aliases: HashMap<u64, u64>,
}

struct Latest {
    definition: SourceSpan,
    declaration: Option<u64>,
    epoch: u64,
}

impl Bindings {
    pub(super) fn new(
        facts: &[SyntaxFact],
        source: &str,
        maximum_name_bytes: usize,
        cancellation: &Cancellation,
    ) -> Result<Self, AdapterError> {
        cancellation.check()?;
        let mut ordered: Vec<_> = facts.iter().collect();
        crate::runtime::sort_cancellable_by(&mut ordered, cancellation, |left, right| {
            (left.span().start_byte(), left.depth(), left.local_id()).cmp(&(
                right.span().start_byte(),
                right.depth(),
                right.local_id(),
            ))
        })?;
        let mut documents = HashMap::<u64, Option<Document>>::new();
        let mut modules = HashMap::<u64, Option<u64>>::new();
        let mut declarations = HashMap::<u64, Option<u64>>::new();
        let mut definitions = HashMap::<u64, Option<&SyntaxFact>>::new();
        let mut anchor_declarations = BTreeSet::new();
        let mut document_count = 0_u64;
        for fact in &ordered {
            cancellation.check()?;
            let (mut document, mut module, mut declaration) = match fact.parent() {
                Some(parent) => (
                    *documents.get(&parent).ok_or_else(invalid_capture)?,
                    *modules.get(&parent).ok_or_else(invalid_capture)?,
                    *declarations.get(&parent).ok_or_else(invalid_capture)?,
                ),
                None => (None, None, None),
            };
            if fact.kind() == SyntaxFactKind::Module
                && fact.syntax_kind().as_str() == "yaml.file.module"
            {
                module = Some(fact.local_id());
            }
            if fact.kind() == SyntaxFactKind::Scope
                && fact.syntax_kind().as_str() == "yaml.document.scope"
            {
                if document.is_some() {
                    return Err(invalid_capture());
                }
                document = Some(Document {
                    ordinal: document_count,
                    module,
                });
                document_count = document_count
                    .checked_add(1)
                    .ok_or(SinkError::AccountingOverflow)?;
            }
            if fact.kind() == SyntaxFactKind::Declaration {
                declaration = Some(fact.local_id());
                if fact.syntax_kind().as_str() == "yaml.anchor.declaration" {
                    anchor_declarations.insert(fact.local_id());
                }
            }
            if fact.kind() == SyntaxFactKind::Occurrence
                && fact.syntax_kind().as_str() == "yaml.anchor.definition"
                && let Some(declaration) = declaration
            {
                definitions
                    .entry(declaration)
                    .and_modify(|selected| {
                        if selected.is_some_and(|selected| selected.span() != fact.span()) {
                            *selected = None;
                        }
                    })
                    .or_insert(Some(fact));
            }
            documents.insert(fact.local_id(), document);
            modules.insert(fact.local_id(), module);
            declarations.insert(fact.local_id(), declaration);
        }

        let mut result = Self {
            anchors: HashMap::new(),
            aliases: HashMap::new(),
        };
        let mut latest = HashMap::<(u64, &str), Latest>::new();
        let mut ordinals = HashMap::<(u64, &str), u64>::new();
        let mut epochs = HashMap::<u64, u64>::new();
        for fact in ordered {
            cancellation.check()?;
            let Some(document) = documents.get(&fact.local_id()).copied().flatten() else {
                continue;
            };
            let epoch = epochs.entry(document.ordinal).or_default();
            if anchor_declarations.contains(&fact.local_id())
                && definitions
                    .get(&fact.local_id())
                    .copied()
                    .flatten()
                    .is_none()
            {
                // An unidentified anchor may shadow any earlier spelling. An
                // epoch invalidates those bindings without repeatedly clearing
                // a document-sized map; later proven definitions recover.
                *epoch = epoch.checked_add(1).ok_or(SinkError::AccountingOverflow)?;
            }
            if fact.kind() != SyntaxFactKind::Occurrence {
                continue;
            }
            let indicator = match fact.syntax_kind().as_str() {
                "yaml.anchor.definition" => b'&',
                "yaml.alias.reference" => b'*',
                _ => continue,
            };
            let start = usize::try_from(fact.span().start_byte()).map_err(|_| invalid_capture())?;
            let end = usize::try_from(fact.span().end_byte()).map_err(|_| invalid_capture())?;
            let name = source.get(start..end).ok_or_else(invalid_capture)?;
            if start
                .checked_sub(1)
                .and_then(|start| source.as_bytes().get(start))
                != Some(&indicator)
            {
                return Err(invalid_capture());
            }
            if rootlight_adapter_sdk::structural_captured_name_for_fact(
                "yaml",
                fact,
                name,
                maximum_name_bytes,
            )
            .is_none()
            {
                continue;
            }
            let key = (document.ordinal, name);
            if indicator == b'*' {
                if let Some(binding) = latest.get(&key)
                    && binding.epoch == *epoch
                    && binding.definition.end_byte() <= fact.span().start_byte()
                    && let Some(declaration) = binding.declaration
                {
                    result.aliases.insert(fact.local_id(), declaration);
                }
                continue;
            }
            let owner = declarations.get(&fact.local_id()).copied().flatten();
            let unique = owner
                .and_then(|owner| definitions.get(&owner))
                .copied()
                .flatten();
            if let Some(unique) = unique
                && unique.local_id() != fact.local_id()
            {
                continue;
            }
            let mut declaration =
                owner.filter(|owner| anchor_declarations.contains(owner) && unique.is_some());
            let ordinal = ordinals.entry(key).or_default();
            let current = *ordinal;
            *ordinal = ordinal
                .checked_add(1)
                .ok_or(SinkError::AccountingOverflow)?;
            if let Some(previous) = latest.get(&key)
                && previous.definition.start_byte() == fact.span().start_byte()
            {
                if let Some(previous) = previous.declaration {
                    result.anchors.remove(&previous);
                }
                declaration = None;
            }
            if let Some(declaration) = declaration {
                result.anchors.insert(
                    declaration,
                    Anchor {
                        document,
                        ordinal: current,
                    },
                );
            }
            latest.insert(
                key,
                Latest {
                    definition: fact.span(),
                    declaration,
                    epoch: *epoch,
                },
            );
        }
        Ok(result)
    }
}

fn invalid_capture() -> AdapterError {
    provider_failure("treesitter-yaml-binding-capture")
}
