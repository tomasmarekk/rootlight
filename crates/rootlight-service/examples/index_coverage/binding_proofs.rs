//! Roots emitted diagnostic fields and their deduplicated provenance closure.
//! Base fact identities are terminal source witnesses, not copies of their
//! semantic records; complete proof records retain their derivation parents.

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
};

use rootlight_ids::{FactId, FileId, GenerationId, RepositoryId};
use rootlight_ir::{
    EntityKind, FactRef, FileRecord, NormalizedIrDocument, OccurrenceRecord, OccurrenceRole,
    OccurrenceTarget, ProvenanceRecord, RelationEndpoint, RelationPredicate, SkippedRegion,
    SourceRef,
};
use serde_json::{Value, json};

struct BaseIdentity<'a> {
    repository: RepositoryId,
    generation: GenerationId,
    source: Option<&'a SourceRef>,
    subtype: Option<&'static str>,
}

enum Node<'a> {
    Identity(BaseIdentity<'a>),
    Proof(&'a ProvenanceRecord),
}

/// Exact admitted report records and their complete reachable proof closure.
///
/// Base identities remain terminal source witnesses, not semantic records.
pub(super) struct BindingEvidence {
    pub(super) external_bindings: BTreeMap<FileId, Vec<Value>>,
    pub(super) supplemental_calls: BTreeMap<FileId, Vec<Value>>,
    pub(super) provenance: Vec<Value>,
    pub(super) fact_identities: Vec<Value>,
    roots: BTreeSet<FactRef>,
}

impl BindingEvidence {
    /// Roots a normalized file and its actual provenance after row admission.
    pub(super) fn admit_file(&mut self, file: &FileRecord) {
        self.admit(FactRef::File(file.id), file.provenance);
    }

    /// Roots an emitted normalized gap and its actual provenance.
    pub(super) fn admit_gap(&mut self, gap: &SkippedRegion) {
        self.admit(FactRef::Fact(gap.id), gap.provenance);
    }

    /// Roots an exact occurrence and returns its printed target/provenance pairs.
    ///
    /// # Errors
    /// Rejects missing targets or allocation failure for their bounded witnesses.
    pub(super) fn admit_occurrence(
        &mut self,
        document: &NormalizedIrDocument,
        occurrence: &OccurrenceRecord,
    ) -> Result<Vec<Value>, Box<dyn Error>> {
        self.admit(FactRef::Fact(occurrence.id), occurrence.provenance);
        let targets: &[rootlight_ids::SymbolId] = match &occurrence.target {
            OccurrenceTarget::Resolved { symbol } => std::slice::from_ref(symbol),
            OccurrenceTarget::Candidates { symbols, .. } => symbols,
            OccurrenceTarget::Unresolved { .. } => &[],
        };
        let mut target_provenance = Vec::new();
        target_provenance.try_reserve_exact(targets.len())?;
        for symbol in targets {
            let target = document
                .entities
                .binary_search_by_key(symbol, |entity| entity.id)
                .ok()
                .and_then(|index| document.entities.get(index))
                .ok_or("diagnostic occurrence has no target entity")?;
            self.admit(FactRef::Entity(target.id), target.provenance);
            target_provenance.push(json!({"symbol":target.id, "provenance":target.provenance}));
        }
        Ok(target_provenance)
    }

    fn admit(&mut self, identity: FactRef, provenance: FactId) {
        // Base identities are terminal witnesses. Root their actual provenance
        // explicitly rather than pretending the terminal record contains it.
        self.roots.extend([identity, FactRef::Fact(provenance)]);
    }

    /// Finalizes the complete proof closure after all emitted fields are admitted.
    ///
    /// # Errors
    /// Rejects duplicate identities, missing parents, foreign owners, and sources
    /// whose identity or byte range differs from the inspected generation.
    pub(super) fn finish(&mut self, document: &NormalizedIrDocument) -> Result<(), Box<dyn Error>> {
        let mut pending = std::mem::take(&mut self.roots);
        if pending.is_empty() {
            return Ok(());
        }
        let nodes = index_nodes(document)?;
        let mut visited = BTreeSet::new();
        // Each typed snapshot identity is admitted once. Cyclic or shared input
        // references cannot grow the walk beyond the finite inspected snapshot.
        while let Some(reference) = pending.pop_first() {
            if !visited.insert(reference) {
                continue;
            }
            match nodes
                .get(&reference)
                .ok_or("external binding proof has a missing parent")?
            {
                Node::Proof(proof) => {
                    check_owner(document, proof.repository, proof.generation)?;
                    pending.extend(proof.derivation_parents.iter().copied());
                    for source in proof.input_sources.iter().chain(&proof.evidence_sources) {
                        check_source(document, source)?;
                        pending.insert(FactRef::File(source.span().file()));
                    }
                    self.provenance.push(serde_json::to_value(proof)?);
                }
                Node::Identity(identity) => {
                    check_owner(document, identity.repository, identity.generation)?;
                    if let Some(source) = identity.source {
                        check_source(document, source)?;
                        pending.insert(FactRef::File(source.span().file()));
                    }
                    let mut value = serde_json::to_value(reference)?;
                    value["repository"] = json!(identity.repository);
                    value["generation"] = json!(identity.generation);
                    value["source"] = json!(identity.source);
                    if let Some(subtype) = identity.subtype {
                        value["subtype"] = json!(subtype);
                    }
                    self.fact_identities.push(value);
                }
            }
        }
        Ok(())
    }
}

struct SelectedBindings {
    external_bindings: BTreeMap<FileId, Vec<Value>>,
    supplemental_calls: BTreeMap<FileId, Vec<Value>>,
    roots: BTreeSet<FactRef>,
}

/// Selects binding records before emitted file fields add their proof roots.
///
/// Binding selection and its roots share one pass. Call `finish` only after
/// admitting normalized files, gaps, and their exact matching occurrences.
///
/// # Errors
/// Rejects binding records without their required target, relation or provenance.
pub(super) fn select_report_evidence(
    document: &NormalizedIrDocument,
) -> Result<BindingEvidence, Box<dyn Error>> {
    let SelectedBindings {
        external_bindings,
        supplemental_calls,
        roots,
    } = select_bindings(document)?;
    Ok(BindingEvidence {
        external_bindings,
        supplemental_calls,
        provenance: Vec::new(),
        fact_identities: Vec::new(),
        roots,
    })
}

fn select_bindings(document: &NormalizedIrDocument) -> Result<SelectedBindings, Box<dyn Error>> {
    let entities: BTreeMap<_, _> = document
        .entities
        .iter()
        .map(|entity| (entity.id, entity))
        .collect();
    let occurrences: BTreeMap<_, _> = document
        .occurrences
        .iter()
        .map(|site| (site.id, site))
        .collect();
    let provenance: BTreeMap<_, _> = document
        .provenance
        .iter()
        .map(|proof| (proof.id, proof))
        .collect();
    let external: BTreeMap<_, _> = document
        .entities
        .iter()
        .filter(|entity| {
            entity.kind == EntityKind::ExternalSymbol && entity.evidence.source.is_none()
        })
        .map(|entity| (entity.id, entity))
        .collect();
    let mut relations: BTreeMap<_, Vec<_>> = BTreeMap::new();
    for edge in &document.relations {
        if let (RelationEndpoint::Occurrence(site), RelationEndpoint::Entity(symbol)) =
            (edge.subject, edge.object)
            && matches!(
                edge.predicate,
                RelationPredicate::RefersTo
                    | RelationPredicate::Calls
                    | RelationPredicate::UsesType
            )
            && external.contains_key(&symbol)
        {
            relations.entry((site, symbol)).or_default().push(edge);
        }
    }
    let mut selected = SelectedBindings {
        external_bindings: BTreeMap::new(),
        supplemental_calls: BTreeMap::new(),
        roots: BTreeSet::new(),
    };
    for site in &document.occurrences {
        let OccurrenceTarget::Resolved { symbol } = site.target else {
            continue;
        };
        let Some(target) = external.get(&symbol) else {
            continue;
        };
        let edges = relations
            .get(&(site.id, symbol))
            .map(Vec::as_slice)
            .unwrap_or_default();
        let [edge] = edges else {
            return Err("external binding has no unique reference edge".into());
        };
        let expected = match site.role {
            OccurrenceRole::CallSite => RelationPredicate::Calls,
            OccurrenceRole::TypeUse => RelationPredicate::UsesType,
            _ => RelationPredicate::RefersTo,
        };
        if edge.predicate != expected {
            return Err("external binding edge does not match occurrence role".into());
        }
        selected.external_bindings.entry(site.file).or_default().push(json!({
            "occurrence":site, "target":target, "relation":edge,
            "provenance":provenance.get(&site.provenance).ok_or("external binding has no provenance")?,
            "target_provenance":provenance.get(&target.provenance).ok_or("external binding has no provenance")?,
        }));
        selected.roots.extend([
            FactRef::Fact(site.id),
            FactRef::Fact(site.provenance),
            FactRef::Entity(target.id),
            FactRef::Fact(target.provenance),
            FactRef::Fact(edge.id),
            FactRef::Fact(edge.provenance),
        ]);
    }
    for relation in &document.relations {
        if relation.predicate != RelationPredicate::Calls {
            continue;
        }
        let (RelationEndpoint::Occurrence(id), RelationEndpoint::Entity(target)) =
            (relation.subject, relation.object)
        else {
            continue;
        };
        let site = occurrences.get(&id).ok_or("call has no occurrence")?;
        if site.target == (OccurrenceTarget::Resolved { symbol: target }) {
            continue;
        }
        let target = entities.get(&target).ok_or("call has no target entity")?;
        let lexical_target = match site.target {
            OccurrenceTarget::Resolved { symbol } => Some(
                *entities
                    .get(&symbol)
                    .ok_or("call has no lexical target entity")?,
            ),
            _ => None,
        };
        provenance
            .get(&site.provenance)
            .ok_or("call occurrence has no provenance")?;
        provenance
            .get(&target.provenance)
            .ok_or("call target has no provenance")?;
        if let Some(lexical_target) = lexical_target {
            provenance
                .get(&lexical_target.provenance)
                .ok_or("call lexical target has no provenance")?;
        }
        // Supplemental effects have different callable and lexical targets.
        // Selecting and rooting the same record prevents either proof branch
        // from disappearing when no external symbol happens to seed the walk.
        selected
            .supplemental_calls
            .entry(site.file)
            .or_default()
            .push(json!({
                "relation":relation, "occurrence":site, "lexical_target":lexical_target,
                "target":target,
                "provenance":provenance.get(&relation.provenance).ok_or("call has no provenance")?,
            }));
        selected.roots.extend([
            FactRef::Fact(site.id),
            FactRef::Fact(site.provenance),
            FactRef::Fact(relation.id),
            FactRef::Fact(relation.provenance),
            FactRef::Entity(target.id),
            FactRef::Fact(target.provenance),
        ]);
        if let Some(lexical_target) = lexical_target {
            selected.roots.extend([
                FactRef::Entity(lexical_target.id),
                FactRef::Fact(lexical_target.provenance),
            ]);
        }
    }
    Ok(selected)
}

fn index_nodes(
    document: &NormalizedIrDocument,
) -> Result<BTreeMap<FactRef, Node<'_>>, Box<dyn Error>> {
    let mut nodes = BTreeMap::new();
    for file in &document.files {
        insert_identity(
            &mut nodes,
            FactRef::File(file.id),
            file.repository,
            file.generation,
            file.evidence.source.as_ref(),
            None,
        )?;
    }
    for entity in &document.entities {
        insert_identity(
            &mut nodes,
            FactRef::Entity(entity.id),
            entity.repository,
            entity.generation,
            entity.evidence.source.as_ref(),
            None,
        )?;
    }
    for occurrence in &document.occurrences {
        insert_identity(
            &mut nodes,
            FactRef::Fact(occurrence.id),
            occurrence.repository,
            occurrence.generation,
            Some(&occurrence.source),
            Some("occurrence"),
        )?;
    }
    for relation in &document.relations {
        insert_identity(
            &mut nodes,
            FactRef::Fact(relation.id),
            relation.repository,
            relation.generation,
            relation.evidence.source.as_ref(),
            Some("relation"),
        )?;
    }
    for proof in &document.provenance {
        if nodes
            .insert(FactRef::Fact(proof.id), Node::Proof(proof))
            .is_some()
        {
            return Err("external binding proof has a duplicate identity".into());
        }
    }
    for mapping in &document.source_mappings {
        insert_identity(
            &mut nodes,
            FactRef::Fact(mapping.id),
            mapping.repository,
            mapping.generation,
            Some(&mapping.from),
            Some("source_mapping"),
        )?;
    }
    for coverage in &document.coverage_records {
        insert_identity(
            &mut nodes,
            FactRef::Fact(coverage.id),
            coverage.repository,
            coverage.generation,
            coverage.evidence.source.as_ref(),
            Some("coverage"),
        )?;
    }
    for region in &document.skipped_regions {
        insert_identity(
            &mut nodes,
            FactRef::Fact(region.id),
            region.repository,
            region.generation,
            Some(&region.source),
            Some("skipped_region"),
        )?;
    }
    for diagnostic in &document.diagnostics {
        insert_identity(
            &mut nodes,
            FactRef::Fact(diagnostic.id),
            diagnostic.repository,
            diagnostic.generation,
            diagnostic
                .source
                .as_ref()
                .or(diagnostic.evidence.source.as_ref()),
            Some("diagnostic"),
        )?;
    }
    for extension in &document.extensions {
        insert_identity(
            &mut nodes,
            FactRef::Fact(extension.id),
            extension.repository,
            extension.generation,
            extension.evidence.source.as_ref(),
            Some("extension"),
        )?;
    }
    Ok(nodes)
}

fn insert_identity<'a>(
    nodes: &mut BTreeMap<FactRef, Node<'a>>,
    reference: FactRef,
    repository: RepositoryId,
    generation: GenerationId,
    source: Option<&'a SourceRef>,
    subtype: Option<&'static str>,
) -> Result<(), Box<dyn Error>> {
    if nodes
        .insert(
            reference,
            Node::Identity(BaseIdentity {
                repository,
                generation,
                source,
                subtype,
            }),
        )
        .is_some()
    {
        return Err("external binding proof has a duplicate identity".into());
    }
    Ok(())
}

fn check_owner(
    document: &NormalizedIrDocument,
    repository: RepositoryId,
    generation: GenerationId,
) -> Result<(), Box<dyn Error>> {
    if repository != document.repository || generation != document.generation {
        return Err("external binding proof belongs to a different generation".into());
    }
    Ok(())
}

fn check_source(document: &NormalizedIrDocument, source: &SourceRef) -> Result<(), Box<dyn Error>> {
    check_owner(document, source.repository(), source.generation())?;
    let file = document
        .files
        .binary_search_by_key(&source.span().file(), |file| file.id)
        .ok()
        .and_then(|index| document.files.get(index))
        .ok_or("external binding proof source has no file")?;
    if source.content_hash() != file.content_hash || source.span().end_byte() > file.byte_length {
        return Err("external binding proof source differs from its file identity".into());
    }
    Ok(())
}
