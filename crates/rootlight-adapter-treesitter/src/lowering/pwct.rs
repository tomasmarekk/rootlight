//! Authenticate PWCT visual owners independently of physical syntax nesting.
//! Written StepIDs supply file-local identities; checked parent references supply
//! Contains owners without assigning executable meaning to captions or code.

use super::*;
use rootlight_adapter_sdk::pwct::{self, ErrorKind, Role, StepId};

pub(super) fn admit(
    facts: &[SyntaxFact],
    source: &SourceRef,
    bytes: &[u8],
    limits: &AnalysisLimits,
    cancellation: &Cancellation,
) -> Result<pwct::Plan, AdapterError> {
    cancellation.check()?;
    if source.span().start_byte() != 0
        || usize::try_from(source.span().end_byte()).ok() != Some(bytes.len())
    {
        return Err(provider_failure("pwct-complete-source-required"));
    }
    let mut hash = blake3::Hasher::new();
    for chunk in bytes.chunks(4096) {
        cancellation.check()?;
        hash.update(chunk);
    }
    if ContentHash::from_bytes(*hash.finalize().as_bytes()) != source.content_hash() {
        return Err(provider_failure("pwct-source-identity-mismatch"));
    }
    let decode_limits = pwct::Limits::for_parse(
        limits.max_source_bytes(),
        limits.max_syntax_nodes(),
        limits.max_syntax_depth(),
    )
    .map_err(model_error)?;
    let plan = pwct::plan(
        bytes,
        decode_limits,
        limits.max_syntax_nodes(),
        cancellation,
    )
    .map_err(model_error)?;
    if facts.len() != plan.records.len() {
        return Err(provider_failure("pwct-source-census-mismatch"));
    }
    let mut actual = HashMap::new();
    actual
        .try_reserve(facts.len())
        .map_err(|_| SinkError::AllocationFailed)?;
    for fact in facts {
        cancellation.check()?;
        if actual.insert(fact.local_id(), fact).is_some() {
            return Err(provider_failure("pwct-source-census-duplicate"));
        }
    }
    for expected in &plan.records {
        cancellation.check()?;
        let fact = actual
            .get(&expected.local_id)
            .ok_or_else(|| provider_failure("pwct-source-census-missing"))?;
        let span = SourceSpan::new(
            source.span().file(),
            u64::try_from(expected.span.start).map_err(|_| SinkError::AccountingOverflow)?,
            u64::try_from(expected.span.end).map_err(|_| SinkError::AccountingOverflow)?,
        )
        .map_err(|_| SinkError::InvalidIr)?;
        if fact.kind() != expected.role.kind()
            || fact.syntax_kind().as_str() != expected.role.syntax()
            || fact.parent() != expected.parent
            || fact.depth() != expected.role.depth()
            || fact.span() != span
        {
            return Err(provider_failure("pwct-source-census-mismatch"));
        }
    }
    Ok(plan)
}

fn model_error(error: pwct::Error) -> AdapterError {
    match error.kind {
        ErrorKind::Cancelled(error) => error.into(),
        ErrorKind::Allocation => SinkError::AllocationFailed.into(),
        ErrorKind::Malformed(_) | ErrorKind::Unsupported(_) | ErrorKind::Limit(_) => {
            provider_failure("pwct-source-plan-unavailable")
        }
    }
}

pub(super) fn resolve(
    plan: &pwct::Plan,
    drafts: &mut HashMap<u64, EntityDraft>,
    strings: &mut usize,
    limits: &IrLimits,
    cancellation: &Cancellation,
) -> Result<(), AdapterError> {
    let mut declarations = HashMap::<StepId, u64>::new();
    let mut definitions = HashMap::<StepId, u64>::new();
    declarations
        .try_reserve(plan.model.steps.len())
        .map_err(|_| SinkError::AllocationFailed)?;
    definitions
        .try_reserve(plan.model.steps.len())
        .map_err(|_| SinkError::AllocationFailed)?;
    for record in &plan.records {
        cancellation.check()?;
        let target = match record.role {
            Role::Step => &mut declarations,
            Role::Definition => &mut definitions,
            Role::Root | Role::Parent => continue,
        };
        let step = record
            .step
            .ok_or_else(|| provider_failure("pwct-source-plan-step"))?;
        if target.insert(step, record.local_id).is_some() {
            return Err(provider_failure("pwct-source-plan-duplicate"));
        }
    }
    if declarations.len() != plan.model.steps.len()
        || definitions.len() != plan.model.steps.len()
        || drafts.len() != plan.model.steps.len()
    {
        return Err(provider_failure("pwct-visual-draft-census"));
    }
    let mut depths = Vec::new();
    depths
        .try_reserve_exact(plan.model.steps.len())
        .map_err(|_| SinkError::AllocationFailed)?;
    depths.resize(plan.model.steps.len(), 0_usize);
    let mut path = Vec::new();
    path.try_reserve_exact(plan.model.steps.len())
        .map_err(|_| SinkError::AllocationFailed)?;
    for start in 0..plan.model.steps.len() {
        cancellation.check()?;
        let mut current = Some(start);
        while let Some(index) = current {
            cancellation.check()?;
            if *depths
                .get(index)
                .ok_or_else(|| provider_failure("pwct-visual-parent-index"))?
                != 0
            {
                break;
            }
            if path.len() >= plan.model.steps.len() {
                return Err(provider_failure("pwct-visual-parent-cycle"));
            }
            path.push(index);
            current = plan
                .model
                .steps
                .get(index)
                .ok_or_else(|| provider_failure("pwct-visual-parent-index"))?
                .parent_index;
        }
        let mut depth = current
            .and_then(|index| depths.get(index).copied())
            .unwrap_or(0);
        while let Some(index) = path.pop() {
            cancellation.check()?;
            depth = depth.checked_add(1).ok_or(SinkError::AccountingOverflow)?;
            *depths
                .get_mut(index)
                .ok_or_else(|| provider_failure("pwct-visual-parent-index"))? = depth;
        }
    }
    for (index, step) in plan.model.steps.iter().enumerate() {
        cancellation.check()?;
        let local = *declarations
            .get(&step.id)
            .ok_or_else(|| provider_failure("pwct-visual-declaration"))?;
        let definition = *definitions
            .get(&step.id)
            .ok_or_else(|| provider_failure("pwct-visual-definition"))?;
        let parent = step
            .parent_index
            .map(|index| {
                let owner = plan
                    .model
                    .steps
                    .get(index)
                    .ok_or_else(|| provider_failure("pwct-visual-parent-index"))?;
                declarations
                    .get(&owner.id)
                    .copied()
                    .ok_or_else(|| provider_failure("pwct-visual-parent-declaration"))
            })
            .transpose()?;
        let draft = drafts
            .get_mut(&local)
            .ok_or_else(|| provider_failure("pwct-visual-draft-missing"))?;
        if draft.kind != EntityKind::Property
            || draft.language != "pwct"
            || draft.definition_local_id != Some(definition)
        {
            return Err(provider_failure("pwct-visual-draft-identity"));
        }
        let name = step.id.0.to_string();
        let qualified = format!("step:{name}");
        account_string(strings, name.len(), limits)?;
        account_string(strings, qualified.len(), limits)?;
        let mut hash = blake3::Hasher::new();
        hash.update(b"rootlight.pwct-visual-step/1\0");
        hash.update(&step.id.0.to_le_bytes());
        draft.name = name;
        draft.parent_entity = parent;
        draft.lexical_module = None;
        draft.scope_identity = None;
        draft.scope_collision_guard = None;
        draft.qualified_prefix = None;
        draft.data_identity = Some(*hash.finalize().as_bytes());
        draft.data_root_entity = None;
        draft.qualified_length = qualified.len();
        draft.data_qualified_name = Some(qualified);
        draft.depth = *depths
            .get(index)
            .ok_or_else(|| provider_failure("pwct-visual-parent-index"))?;
        draft.signature.clear();
        draft.signature_evidence = None;
        draft.signature_span = None;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_adapter_sdk::{BatchThresholds, StreamLimits, SyntaxKindLabel};
    use rootlight_ids::GenerationId;

    const SOURCE: &[u8] = b"cVisualSourceFileVersion='1.0.0'\ncVisualLanguageName='RingPWCT'\ncNaturalLanguageName='english'\nnStepsID=2\nnInteractionsID=2\naStepsTable=[[1,0,[[\"name\",\"Start\"],[\"active\",1],[\"code\",\"\"],[\"interactionid\",1],[\"visible\",1],[\"stepnumber\",1],[\"steptype\",2]]]]\naInteractionsTable=[[1,1,'','01/01/2023','12:00:00','']]\n";

    fn input() -> (SourceRef, AnalysisLimits, Vec<SyntaxFact>) {
        let source = SourceRef::new(
            RepositoryId::from_bytes([1; 16]),
            GenerationId::from_bytes([2; 20]),
            SourceSpan::new(FileId::from_bytes([3; 20]), 0, SOURCE.len() as u64).unwrap(),
            content_hash(SOURCE),
            None,
        );
        let stream = StreamLimits::new(
            128,
            4096,
            1024 * 1024,
            128,
            8192,
            65_536,
            BatchThresholds::new(64, 65_536, 32, 8192).unwrap(),
        )
        .unwrap();
        let limits = AnalysisLimits::new(
            65_536,
            4096,
            64,
            1,
            1024 * 1024,
            stream.clone(),
            stream,
            IrLimits::default(),
        )
        .unwrap();
        let plan = pwct::plan(
            SOURCE,
            pwct::Limits::for_parse(65_536, 4096, 64).unwrap(),
            4096,
            &Cancellation::new(),
        )
        .unwrap();
        let facts = plan
            .records
            .iter()
            .map(|record| {
                SyntaxFact::new(
                    record.local_id,
                    record.parent,
                    record.role.kind(),
                    SourceSpan::new(
                        source.span().file(),
                        record.span.start as u64,
                        record.span.end as u64,
                    )
                    .unwrap(),
                    record.role.depth(),
                    SyntaxKindLabel::new(record.role.syntax()).unwrap(),
                )
            })
            .collect();
        (source, limits, facts)
    }

    #[test]
    fn complete_source_census_accepts_a_permuted_fact_view() {
        let (source, limits, mut facts) = input();
        facts.reverse();
        assert_eq!(
            admit(&facts, &source, SOURCE, &limits, &Cancellation::new())
                .unwrap()
                .model
                .steps
                .len(),
            1
        );
    }

    #[test]
    fn labels_do_not_authorize_a_relocated_or_reparented_visual_token() {
        let (source, limits, facts) = input();
        let index = facts
            .iter()
            .position(|fact| fact.syntax_kind().as_str() == Role::Parent.syntax())
            .unwrap();
        let definition = facts
            .iter()
            .find(|fact| fact.syntax_kind().as_str() == Role::Definition.syntax())
            .unwrap();
        let original = &facts[index];
        for (parent, span, kind, depth) in [
            (
                original.parent(),
                definition.span(),
                original.kind(),
                original.depth(),
            ),
            (Some(1), original.span(), original.kind(), original.depth()),
            (
                original.parent(),
                original.span(),
                SyntaxFactKind::Occurrence,
                original.depth(),
            ),
            (
                original.parent(),
                original.span(),
                original.kind(),
                original.depth() + 1,
            ),
        ] {
            let mut forged = facts.clone();
            forged[index] = SyntaxFact::new(
                original.local_id(),
                parent,
                kind,
                span,
                depth,
                original.syntax_kind().clone(),
            );
            assert!(admit(&forged, &source, SOURCE, &limits, &Cancellation::new()).is_err());
        }
    }

    #[test]
    fn missing_duplicate_extra_or_stale_source_censuses_are_refused() {
        let (source, limits, facts) = input();
        let mut missing = facts.clone();
        missing.pop();
        let mut duplicate = facts.clone();
        duplicate[1] = duplicate[0].clone();
        let mut extra = facts.clone();
        extra.push(extra[0].clone());
        for forged in [missing, duplicate, extra] {
            assert!(admit(&forged, &source, SOURCE, &limits, &Cancellation::new()).is_err());
        }
        let stale = SourceRef::new(
            source.repository(),
            source.generation(),
            source.span(),
            content_hash(b"another source"),
            None,
        );
        assert!(admit(&facts, &stale, SOURCE, &limits, &Cancellation::new()).is_err());
    }
}
