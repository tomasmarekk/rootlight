//! Bounded RDF extension contribution to the existing neutral digest recipe.
//!
//! Only compact old/new IDs survive the first pass. The second pass rereads one
//! authenticated statement at a time in the same neutral order as flat IR.

use super::*;

/// Data source for complete RDF v1 digest projection, granting no graph authority.
pub trait RdfDigestSource {
    /// Visits authenticated neutral-ID hints in strict original FactId order.
    ///
    /// None selects the ordinary record pass and must not call the visitor.
    /// Every hint is checked against its exact reread before entering a digest.
    ///
    /// # Errors
    /// A supported hint stream rejects missing/order/count drift or interruption.
    fn visit_neutral_ids(
        &mut self,
        _visitor: &mut dyn FnMut(FactId, FactId) -> Result<(), NormalizedRebindError>,
    ) -> Option<Result<(), NormalizedRebindError>> {
        None
    }

    /// Visits every RDF statement exactly once in strict original FactId order.
    ///
    /// # Errors
    /// Preserves source refusal or interruption through the caller's adapter.
    fn visit(
        &mut self,
        visitor: &mut dyn FnMut(ExtensionEnvelope) -> Result<(), NormalizedRebindError>,
    ) -> Result<(), NormalizedRebindError>;
    /// Rereads exactly one previously visited immutable statement.
    ///
    /// # Errors
    /// Rejects absent, changed, cancelled or unavailable source records.
    fn read(&mut self, id: FactId) -> Result<ExtensionEnvelope, NormalizedRebindError>;
}

/// Projects the unchanged complete digest with RDF payload outside the base.
///
/// The base retains its ordinary canonical limits. The caller separately admits
/// the exact logical record count, compact ID workspace and bounded source reads;
/// this function neither resets budgets nor establishes a verified capability.
///
/// # Errors
/// Rejects a noncanonical base, non-v1 RDF, count/order/identity drift, duplicate
/// neutral identities, resource exhaustion and interruption.
pub fn canonical_generation_neutral_digests_with_rdf(
    document: &NormalizedIrDocument,
    limits: &IrLimits,
    extensions: &ExtensionSupport,
    source: &mut impl RdfDigestSource,
    count: usize,
    mut checkpoint: impl FnMut() -> bool,
) -> Result<CanonicalGenerationNeutralDigests, NormalizedRebindError> {
    ensure_projection_continues(&mut checkpoint)?;
    validate_canonical_ir_document(document, limits, extensions)?;
    super::digest_with_rdf(document, source, count, limits, &mut checkpoint)
}

/// Returns additional compact-ID workspace for a logical RDF contribution.
///
/// This covers retained old/neutral ID pairs and their simultaneous extension
/// order entries. The existing base-projection and bounded-record workspace are
/// separate caller-owned reservations.
///
/// # Errors
/// Rejects platform-size or checked byte arithmetic overflow.
pub fn rdf_digest_workspace_bytes(count: usize) -> Result<u64, NormalizedRebindError> {
    let per_record = std::mem::size_of::<(FactId, FactId)>()
        .checked_add(std::mem::size_of::<(FactId, usize)>())
        .ok_or(NormalizedRebindError::ResourceLimit)?;
    count
        .checked_mul(per_record)
        .and_then(|bytes| u64::try_from(bytes).ok())
        .ok_or(NormalizedRebindError::ResourceLimit)
}

pub(super) struct EmptyRdfSource;
impl RdfDigestSource for EmptyRdfSource {
    fn visit(
        &mut self,
        _: &mut dyn FnMut(ExtensionEnvelope) -> Result<(), NormalizedRebindError>,
    ) -> Result<(), NormalizedRebindError> {
        Ok(())
    }
    fn read(&mut self, _: FactId) -> Result<ExtensionEnvelope, NormalizedRebindError> {
        Err(NormalizedRebindError::IdentityRecipe)
    }
}

pub(super) fn require_record(
    document: &NormalizedIrDocument,
    record: &ExtensionEnvelope,
    limits: &IrLimits,
) -> Result<(), NormalizedRebindError> {
    if record.repository != document.repository
        || record.generation != document.generation
        || record.namespace != RDF_STATEMENT_NAMESPACE
        || record.version != "1"
    {
        return Err(NormalizedRebindError::IdentityRecipe);
    }
    crate::IrDocumentUsage::measure_extension(record, limits).map_err(|error| match error {
        crate::IrDocumentUsageError::Validation(error) => NormalizedRebindError::from(error),
        crate::IrDocumentUsageError::AccountingOverflow
        | crate::IrDocumentUsageError::Interrupted => NormalizedRebindError::ResourceLimit,
    })?;
    decode_rdf_statement_envelope(record).map_err(|_| NormalizedRebindError::IdentityRecipe)?;
    Ok(())
}

pub(super) fn collect(
    document: &NormalizedIrDocument,
    ids: &FactIdMap,
    source: &mut impl RdfDigestSource,
    count: usize,
    limits: &IrLimits,
    checkpoint: &mut impl FnMut() -> bool,
) -> Result<Vec<(FactId, FactId)>, NormalizedRebindError> {
    rdf_digest_workspace_bytes(count)?;
    let mut records = Vec::new();
    records
        .try_reserve_exact(count)
        .map_err(|_| NormalizedRebindError::ResourceLimit)?;
    let mut previous = None;
    let hinted = source.visit_neutral_ids(&mut |original, neutral| {
        ensure_projection_continues(checkpoint)?;
        append_id(ids, &mut records, &mut previous, count, original, neutral)
    });
    if let Some(result) = hinted {
        result?;
    } else {
        if !records.is_empty() {
            return Err(NormalizedRebindError::IdentityRecipe);
        }
        source.visit(&mut |record| {
            ensure_projection_continues(checkpoint)?;
            require_record(document, &record, limits)?;
            let original = record.id;
            let neutral = rebind_extension(
                record,
                document.repository,
                GENERATION_NEUTRAL_ID,
                ids,
                checkpoint,
            )?;
            append_id(
                ids,
                &mut records,
                &mut previous,
                count,
                original,
                neutral.id,
            )
        })?;
    }
    ensure_projection_continues(checkpoint)?;
    if records.len() != count {
        return Err(NormalizedRebindError::IdentityRecipe);
    }
    Ok(records)
}

fn append_id(
    ids: &FactIdMap,
    records: &mut Vec<(FactId, FactId)>,
    previous: &mut Option<FactId>,
    count: usize,
    original: FactId,
    neutral: FactId,
) -> Result<(), NormalizedRebindError> {
    if records.len() == count
        || previous.is_some_and(|old| old >= original)
        || ids.get(original).is_some()
    {
        return Err(NormalizedRebindError::IdentityRecipe);
    }
    *previous = Some(original);
    records.push((neutral, original));
    Ok(())
}
