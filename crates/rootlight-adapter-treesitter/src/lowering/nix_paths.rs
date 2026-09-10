//! Source-backed implicit owners for native Nix attribute-path components.
//! Repeated prefixes share one identity but keep every written definition site;
//! the value-bearing leaf retains its authored name and separate callable kind.

use super::*;

pub(super) const OWNER_UNAVAILABLE: &str = "nix-implicit-attribute-owner-unavailable";

pub(super) fn is_segment(fact: &SyntaxFact) -> bool {
    matches!(
        fact.syntax_kind().as_str(),
        "nix.path_segment.definition_part" | "nix.dynamic_segment.definition_part"
    )
}

pub(super) fn resolve(
    facts: &[SyntaxFact],
    source: &str,
    contexts: &HashMap<u64, Option<[u8; 32]>>,
    drafts: &mut HashMap<u64, EntityDraft>,
    strings: &mut usize,
    limits: &IrLimits,
    cancellation: &Cancellation,
) -> Result<BTreeSet<u64>, AdapterError> {
    cancellation.check()?;
    let mut unavailable = BTreeSet::new();
    if !facts
        .iter()
        .any(|fact| fact.syntax_kind().as_str().starts_with("nix."))
    {
        return Ok(unavailable);
    }
    let mut paths = BTreeMap::<u64, Vec<&SyntaxFact>>::new();
    for fact in facts {
        cancellation.check()?;
        if is_segment(fact)
            && let Some(owner) = fact.parent()
        {
            paths.entry(owner).or_default().push(fact);
        }
    }
    let mut original_names = HashMap::new();
    for &owner in paths.keys() {
        if let Some(draft) = drafts.get(&owner) {
            original_names.insert(owner, qualified(draft, drafts, limits, cancellation)?);
        }
    }
    let mut representatives = HashMap::<(Option<u64>, [u8; 32]), u64>::new();
    let facts_by_id: HashMap<_, _> = facts.iter().map(|fact| (fact.local_id(), fact)).collect();
    for (owner, mut parts) in paths {
        cancellation.check()?;
        if parts.len() < 2 {
            continue;
        }
        crate::runtime::sort_cancellable_by(&mut parts, cancellation, |left, right| {
            (left.span().start_byte(), left.span().end_byte())
                .cmp(&(right.span().start_byte(), right.span().end_byte()))
        })?;
        let owner_fact = facts_by_id.get(&owner).ok_or_else(invalid)?;
        if !matches!(
            owner_fact.syntax_kind().as_str(),
            "nix.variable.declaration"
                | "nix.function.declaration"
                | "nix.dynamic_variable.declaration"
                | "nix.dynamic_function.declaration"
                | "nix.dynamic_path_variable.declaration"
                | "nix.dynamic_path_function.declaration"
        ) {
            return Err(invalid());
        }
        for pair in parts.windows(2) {
            cancellation.check()?;
            if let [left, right] = pair
                && left.span().end_byte() > right.span().start_byte()
            {
                return Err(invalid());
            }
        }
        let Some(base) = drafts.get(&owner).cloned() else {
            unavailable.extend(
                parts
                    .iter()
                    .take(parts.len() - 1)
                    .map(|part| part.local_id()),
            );
            continue;
        };
        let mut parent = base.parent_entity;
        let mut path = String::new();
        let mut digest = blake3::Hasher::new_derive_key("rootlight.nix-implicit-attribute-path/1");
        if let Some(Some(context)) = contexts.get(&owner) {
            digest.update(context);
        }
        let parent_name = base
            .parent_entity
            .and_then(|id| drafts.get(&id))
            .map(|parent| qualified(parent, drafts, limits, cancellation))
            .transpose()?;
        let mut last_digest = None;
        for (position, part) in parts.iter().take(parts.len() - 1).enumerate() {
            cancellation.check()?;
            let written = text(source, part.span())?;
            let Some(name) = rootlight_adapter_sdk::nix_canonical_attribute_name(
                written,
                limits.max_string_bytes,
                cancellation,
            )?
            else {
                unavailable.extend(
                    parts
                        .iter()
                        .take(parts.len() - 1)
                        .skip(position)
                        .map(|part| part.local_id()),
                );
                break;
            };
            if !path.is_empty() {
                append(&mut path, ".", limits)?;
            }
            append(&mut path, &name, limits)?;
            digest.update(
                &u64::try_from(name.len())
                    .map_err(|_| SinkError::AccountingOverflow)?
                    .to_be_bytes(),
            );
            digest.update(name.as_bytes());
            let address = *digest.clone().finalize().as_bytes();
            let mut qualified_name = String::new();
            if let Some(prefix) = &parent_name {
                append(&mut qualified_name, prefix, limits)?;
                append(&mut qualified_name, "::", limits)?;
            }
            append(&mut qualified_name, &path, limits)?;
            for length in [3, path.len(), path.len(), qualified_name.len()] {
                account_string(strings, length, limits)?;
            }
            let local_id = part.local_id();
            drafts.insert(
                local_id,
                EntityDraft {
                    local_id,
                    parent_entity: parent,
                    lexical_module: None,
                    scope_identity: None,
                    data_identity: Some(address),
                    data_root_entity: base.parent_entity,
                    data_qualified_name: Some(qualified_name.clone()),
                    scope_collision_guard: None,
                    qualified_prefix: None,
                    synthetic: false,
                    is_test: base.is_test,
                    definition_local_id: Some(local_id),
                    span: part.span(),
                    depth: 0,
                    kind: EntityKind::Variable,
                    name: path.clone(),
                    signature: String::new(),
                    signature_evidence: None,
                    signature_span: None,
                    language: "nix".to_owned(),
                    qualified_length: qualified_name.len(),
                },
            );
            let representative = *representatives
                .entry((base.parent_entity, address))
                .or_insert(local_id);
            parent = Some(representative);
            last_digest = Some(address);
        }
        if let Some(address) = last_digest {
            // Preserve the full authored leaf name, qualified spelling and
            // occurrence discriminator while moving containment under its owner.
            let original = original_names.get(&owner).ok_or_else(invalid)?;
            let leaf = drafts.get_mut(&owner).ok_or_else(invalid)?;
            leaf.parent_entity = parent;
            leaf.data_identity = Some(address);
            leaf.data_root_entity = base.parent_entity;
            leaf.data_qualified_name = Some(original.clone());
        }
    }
    merge_attributes(facts, source, drafts, strings, limits, cancellation)?;
    restore_depths(drafts, cancellation)?;
    Ok(unavailable)
}

fn merge_attributes(
    facts: &[SyntaxFact],
    source: &str,
    drafts: &mut HashMap<u64, EntityDraft>,
    strings: &mut usize,
    limits: &IrLimits,
    cancellation: &Cancellation,
) -> Result<(), AdapterError> {
    let attributes = crate::nix_attributes::Attributes::build(
        facts,
        source.as_bytes(),
        limits.max_string_bytes,
        cancellation,
    )?;
    if !attributes
        .groups
        .values()
        .any(|group| group.valid && group.is_set && group.members.len() > 1)
    {
        return Ok(());
    }
    let mut merged = HashMap::new();
    let mut before: HashMap<_, _> = drafts
        .iter()
        .map(|(&id, draft)| {
            let length = draft
                .name
                .len()
                .checked_mul(2)
                .and_then(|length| length.checked_add(draft.qualified_length))
                .ok_or(SinkError::AccountingOverflow)?;
            Ok((id, length))
        })
        .collect::<Result<_, SinkError>>()?;
    for group in attributes
        .groups
        .values()
        .filter(|group| group.valid && group.is_set && group.members.len() > 1)
    {
        cancellation.check()?;
        let Some(first) = group
            .members
            .first()
            .and_then(|member| drafts.get(&member.draft))
            .cloned()
        else {
            continue;
        };
        if group
            .members
            .iter()
            .any(|member| !drafts.contains_key(&member.draft))
        {
            continue;
        }
        for member in &group.members {
            cancellation.check()?;
            let draft = drafts.get_mut(&member.draft).ok_or_else(invalid)?;
            let replacement = first
                .name
                .len()
                .checked_mul(2)
                .and_then(|length| length.checked_add(first.qualified_length))
                .ok_or(SinkError::AccountingOverflow)?;
            let reserved = before.get_mut(&member.draft).ok_or_else(invalid)?;
            if replacement > *reserved {
                account_string(strings, replacement - *reserved, limits)?;
                *reserved = replacement;
            }
            // Identity follows one written owner while each definition keeps its
            // own exact source site. Children are rebound to that shared owner.
            draft.name.clone_from(&first.name);
            draft.parent_entity = first.parent_entity;
            draft.scope_identity = first.scope_identity;
            draft.scope_collision_guard = first.scope_collision_guard;
            draft.data_identity = first.data_identity;
            draft.data_root_entity = first.data_root_entity;
            draft
                .data_qualified_name
                .clone_from(&first.data_qualified_name);
            draft.qualified_length = first.qualified_length;
            merged.insert(member.draft, first.local_id);
        }
    }
    if merged.is_empty() {
        return Ok(());
    }
    for draft in drafts.values_mut() {
        cancellation.check()?;
        if let Some(parent) = draft.parent_entity.and_then(|id| merged.get(&id)) {
            draft.parent_entity = Some(*parent);
        }
        if let Some(root) = draft.data_root_entity.and_then(|id| merged.get(&id)) {
            draft.data_root_entity = Some(*root);
        }
    }
    let mut lengths = Vec::new();
    for (&id, draft) in drafts.iter().filter(|(_, draft)| draft.language == "nix") {
        cancellation.check()?;
        lengths.push((id, qualified(draft, drafts, limits, cancellation)?.len()));
    }
    for (id, length) in lengths {
        let draft = drafts.get_mut(&id).ok_or_else(invalid)?;
        draft.qualified_length = length;
        let current = draft
            .name
            .len()
            .checked_mul(2)
            .and_then(|size| size.checked_add(length))
            .ok_or(SinkError::AccountingOverflow)?;
        let original = before.get(&id).copied().ok_or_else(invalid)?;
        if current > original {
            account_string(strings, current - original, limits)?;
        }
    }
    Ok(())
}

fn text(source: &str, span: SourceSpan) -> Result<&str, AdapterError> {
    let start = usize::try_from(span.start_byte()).map_err(|_| invalid())?;
    let end = usize::try_from(span.end_byte()).map_err(|_| invalid())?;
    source.get(start..end).ok_or_else(invalid)
}

fn append(output: &mut String, value: &str, limits: &IrLimits) -> Result<(), AdapterError> {
    let length = output
        .len()
        .checked_add(value.len())
        .ok_or(SinkError::AccountingOverflow)?;
    require_resource_limit(ResourceKind::StringBytes, length, limits.max_string_bytes)?;
    output
        .try_reserve(value.len())
        .map_err(|_| SinkError::AllocationFailed)?;
    output.push_str(value);
    Ok(())
}

fn qualified(
    draft: &EntityDraft,
    drafts: &HashMap<u64, EntityDraft>,
    limits: &IrLimits,
    cancellation: &Cancellation,
) -> Result<String, AdapterError> {
    if let Some(name) = &draft.data_qualified_name {
        return Ok(name.clone());
    }
    let mut chain = Vec::new();
    let mut output = String::new();
    let mut current = Some(draft.local_id);
    for _ in 0..=drafts.len() {
        cancellation.check()?;
        let Some(id) = current else {
            break;
        };
        let node = drafts.get(&id).ok_or_else(invalid)?;
        if let Some(name) = &node.data_qualified_name {
            append(&mut output, name, limits)?;
            current = None;
            break;
        }
        if node.parent_entity.is_none()
            && let Some(prefix) = &node.qualified_prefix
        {
            append(&mut output, prefix, limits)?;
        }
        chain.push(node);
        current = node.parent_entity;
    }
    if current.is_some() {
        return Err(invalid());
    }
    for node in chain.into_iter().rev() {
        if !output.is_empty() {
            append(&mut output, "::", limits)?;
        }
        append(&mut output, &node.name, limits)?;
    }
    Ok(output)
}

fn restore_depths(
    drafts: &mut HashMap<u64, EntityDraft>,
    cancellation: &Cancellation,
) -> Result<(), AdapterError> {
    let mut depths = HashMap::<u64, usize>::new();
    for &id in drafts.keys() {
        let mut chain = Vec::new();
        let mut current = Some(id);
        for _ in 0..=drafts.len() {
            cancellation.check()?;
            let Some(id) = current else {
                break;
            };
            if depths.contains_key(&id) {
                break;
            }
            chain.push(id);
            current = drafts.get(&id).ok_or_else(invalid)?.parent_entity;
        }
        let mut depth = match current {
            Some(id) => depths
                .get(&id)
                .copied()
                .ok_or_else(invalid)?
                .checked_add(1)
                .ok_or(SinkError::AccountingOverflow)?,
            None => 0,
        };
        for id in chain.into_iter().rev() {
            depths.insert(id, depth);
            depth = depth.checked_add(1).ok_or(SinkError::AccountingOverflow)?;
        }
    }
    for (&id, draft) in drafts {
        draft.depth = *depths.get(&id).ok_or_else(invalid)?;
    }
    Ok(())
}

fn invalid() -> AdapterError {
    provider_failure("nix-attribute-path-capture")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_precedes_even_empty_path_planning() {
        let cancellation = Cancellation::new();
        cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(matches!(
            resolve(
                &[],
                "",
                &HashMap::new(),
                &mut HashMap::new(),
                &mut 0,
                &IrLimits::default(),
                &cancellation
            ),
            Err(AdapterError::Cancelled { .. })
        ));
    }

    #[test]
    fn nonbinding_owners_and_overlapping_segments_are_rejected() {
        for owner_kind in ["nix.file.module", "nix.variable.declaration"] {
            let file = FileId::from_bytes([1; 20]);
            let fact = |id, parent, start, end, syntax: &str| {
                SyntaxFact::new(
                    id,
                    parent,
                    SyntaxFactKind::Occurrence,
                    SourceSpan::new(file, start, end).unwrap(),
                    0,
                    rootlight_adapter_sdk::SyntaxKindLabel::new(syntax).unwrap(),
                )
            };
            let facts = [
                fact(1, None, 0, 4, owner_kind),
                fact(2, Some(1), 0, 3, "nix.path_segment.definition_part"),
                fact(3, Some(1), 2, 4, "nix.path_segment.definition_part"),
            ];
            let error = resolve(
                &facts,
                "abcd",
                &HashMap::new(),
                &mut HashMap::new(),
                &mut 0,
                &IrLimits::default(),
                &Cancellation::new(),
            )
            .unwrap_err();
            assert!(
                matches!(error, AdapterError::ProviderFailed { code } if code.as_str() == "nix-attribute-path-capture")
            );
        }
    }
}
