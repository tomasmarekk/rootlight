//! Isolated data-language plans within a source-owning host document.
//! Local syntax IDs and byte spans stay unchanged; only the internal file-module
//! parent is detached so data address construction cannot cross example boundaries.

use super::*;

pub(super) fn partitions(
    facts: &[SyntaxFact],
    language: &str,
    cancellation: &Cancellation,
) -> Result<Vec<Vec<SyntaxFact>>, AdapterError> {
    let mut modules = Vec::new();
    for fact in facts {
        cancellation.check()?;
        if fact.kind() == SyntaxFactKind::Module
            && fact.syntax_kind().as_str().split_once('.') == Some((language, "file.module"))
        {
            modules
                .try_reserve(1)
                .map_err(|_| provider_failure("embedded-data-allocation"))?;
            modules.push(fact);
        }
    }
    crate::runtime::sort_cancellable_by(&mut modules, cancellation, |a, b| {
        a.span().start_byte().cmp(&b.span().start_byte())
    })?;
    let mut result = Vec::new();
    result
        .try_reserve_exact(modules.len())
        .map_err(|_| provider_failure("embedded-data-allocation"))?;
    result.resize_with(modules.len(), Vec::new);
    for fact in facts {
        cancellation.check()?;
        if !fact.syntax_kind().as_str().starts_with(language)
            || fact.syntax_kind().as_str().as_bytes().get(language.len()) != Some(&b'.')
        {
            continue;
        }
        let position = modules
            .partition_point(|module| module.span().start_byte() <= fact.span().start_byte());
        let Some(index) = position.checked_sub(1) else {
            return Err(provider_failure("embedded-data-owner"));
        };
        let module = modules
            .get(index)
            .ok_or_else(|| provider_failure("embedded-data-owner"))?;
        if fact.span().end_byte() > module.span().end_byte() {
            return Err(provider_failure("embedded-data-range"));
        }
        let group = result
            .get_mut(index)
            .ok_or_else(|| provider_failure("embedded-data-owner"))?;
        group
            .try_reserve(1)
            .map_err(|_| provider_failure("embedded-data-allocation"))?;
        group.push(SyntaxFact::new(
            fact.local_id(),
            if fact.local_id() == module.local_id() {
                None
            } else {
                fact.parent()
            },
            fact.kind(),
            fact.span(),
            fact.depth(),
            fact.syntax_kind().clone(),
        ));
    }
    Ok(result)
}

pub(super) fn restore_host_depths(
    facts: &[SyntaxFact],
    drafts: &mut HashMap<u64, EntityDraft>,
    base: usize,
    cancellation: &Cancellation,
) -> Result<(), AdapterError> {
    let root = facts
        .iter()
        .find(|fact| fact.parent().is_none())
        .ok_or_else(|| provider_failure("embedded-data-root-missing"))?
        .local_id();
    for fact in facts {
        cancellation.check()?;
        if let Some(draft) = drafts.get_mut(&fact.local_id()) {
            draft.depth = draft
                .depth
                .checked_add(base)
                .ok_or(SinkError::AccountingOverflow)?;
            if draft.data_identity.is_some() {
                draft.data_root_entity = Some(root);
            }
        }
    }
    Ok(())
}
