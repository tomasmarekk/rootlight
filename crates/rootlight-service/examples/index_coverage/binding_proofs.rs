//! Exports one deduplicated provenance closure for external binding diagnostics.
//! Base fact identities are terminal source witnesses, not copies of their
//! semantic records; complete proof records retain their derivation parents.

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
};

use rootlight_ids::{GenerationId, RepositoryId};
use rootlight_ir::{
    EntityKind, FactRef, NormalizedIrDocument, OccurrenceTarget, ProvenanceRecord,
    RelationEndpoint, RelationPredicate, SourceRef,
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

/// Returns complete reachable proofs and compact terminal base identities.
///
/// # Errors
/// Rejects duplicate identities, missing parents, foreign owners, and sources
/// whose identity or byte range differs from the inspected generation.
pub(super) fn export_external_binding_proofs(
    document: &NormalizedIrDocument,
) -> Result<(Vec<Value>, Vec<Value>), Box<dyn Error>> {
    let mut pending = external_binding_roots(document);
    if pending.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    let nodes = index_nodes(document)?;
    let mut visited = BTreeSet::new();
    let mut proofs = Vec::new();
    let mut identities = Vec::new();
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
                proofs.push(serde_json::to_value(proof)?);
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
                identities.push(value);
            }
        }
    }
    Ok((proofs, identities))
}

fn external_binding_roots(document: &NormalizedIrDocument) -> BTreeSet<FactRef> {
    let external: BTreeSet<_> = document
        .entities
        .iter()
        .filter(|entity| {
            entity.kind == EntityKind::ExternalSymbol && entity.evidence.source.is_none()
        })
        .map(|entity| entity.id)
        .collect();
    let mut roots = BTreeSet::new();
    let mut bindings = BTreeSet::new();
    for site in &document.occurrences {
        if let OccurrenceTarget::Resolved { symbol } = site.target
            && external.contains(&symbol)
        {
            bindings.insert((site.id, symbol));
            roots.extend([
                FactRef::Fact(site.id),
                FactRef::Fact(site.provenance),
                FactRef::Entity(symbol),
            ]);
        }
    }
    for target in &document.entities {
        if roots.contains(&FactRef::Entity(target.id)) {
            roots.insert(FactRef::Fact(target.provenance));
        }
    }
    for edge in &document.relations {
        if let (RelationEndpoint::Occurrence(site), RelationEndpoint::Entity(symbol)) =
            (edge.subject, edge.object)
            && bindings.contains(&(site, symbol))
            && matches!(
                edge.predicate,
                RelationPredicate::RefersTo
                    | RelationPredicate::Calls
                    | RelationPredicate::UsesType
            )
        {
            roots.extend([FactRef::Fact(edge.id), FactRef::Fact(edge.provenance)]);
        }
    }
    roots
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
