//! Source-qualified VBA standard-library identities, not library implementations.
//! External entities have no repository definition; complete project evidence and
//! lexical barriers are required before a source occurrence can name one.

use super::*;
mod err;
mod functions;
use crate::ResolverFactContext;
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, EntityFlag, FactEvidence, NormalizedIrDocument,
    ProducerIdentity, ProvenanceRecord, SymbolIdentityClaim, derive_provenance_record_id,
    new_symbol_identity_claim_envelope,
};

const CATALOG_RULE: &str = "vba-standard-library-catalog-v1";
// MS-VBAL 6.1.2.2. Platform-dependent values are deliberately not evaluated.
// https://learn.microsoft.com/en-us/openspecs/microsoft_general_purpose_programming_languages/ms-vbal/52c853c1-fda2-4853-8069-172e515310f9
const CONSTANTS: &[&str] = &[
    "vbBack",
    "vbCr",
    "vbCrLf",
    "vbFormFeed",
    "vbLf",
    "vbNewLine",
    "vbNullChar",
    "vbTab",
    "vbVerticalTab",
    "vbNullString",
    "vbObjectError",
];

fn claim(repository: rootlight_ids::RepositoryId, name: &str) -> SymbolIdentityClaim {
    let mut signature = CATALOG_RULE.as_bytes().to_vec();
    if let Some(function) = functions::qualified(name) {
        signature.push(0);
        signature.extend_from_slice(function.signature.as_bytes());
    } else if let Some(member) = err::qualified(name) {
        signature.push(0);
        signature.extend_from_slice(member.signature.as_bytes());
    } else if name == "VBA.Err" {
        signature.extend_from_slice(b"\0ErrClass default instance");
    }
    let mut claim = SymbolIdentityClaim {
        symbol: SymbolId::from_bytes([0; 20]),
        repository,
        language: "vba".to_owned(),
        kind: EntityKind::ExternalSymbol,
        container: None,
        container_identity: Vec::new(),
        declared_identity: name.to_owned(),
        signature_discriminator: signature,
        build_context_discriminator: content_hash(CATALOG_RULE.as_bytes()).as_bytes().to_vec(),
    };
    claim.symbol = claim.derived_symbol();
    claim
}

pub(crate) fn materialize(
    document: &mut NormalizedIrDocument,
    context: ResolverFactContext,
    sources: &[ClassicProjectSource<'_>],
    cancellation: &Cancellation,
) -> Result<(), ResolutionError> {
    let mut inputs = BTreeMap::new();
    for source in sources {
        cancellation.check()?;
        inputs.insert(source.file, source.bytes);
    }
    let mut requested = BTreeSet::new();
    for site in &document.occurrences {
        cancellation.check()?;
        if site.syntax_kind == "vba.value_identifier.reference"
            && site_bytes(site, &inputs).is_some_and(|text| {
                [b"VBA".as_slice(), b"[VBA]", b"Err", b"[Err]"]
                    .iter()
                    .any(|name| text.eq_ignore_ascii_case(name))
            })
        {
            requested.insert(site.file);
        }
    }
    let Some(file) = document
        .files
        .iter()
        .filter(|file| file.language == "vba" && requested.contains(&file.id))
        .min_by(|a, b| a.path.cmp(&b.path))
    else {
        return Ok(());
    };
    let Some(source) = file.evidence.source.clone() else {
        return Ok(());
    };
    let file_id = file.id;
    let digest = content_hash(CATALOG_RULE.as_bytes());
    let mut provenance = ProvenanceRecord {
        id: FactId::from_bytes([0; 20]),
        repository: document.repository,
        generation: document.generation,
        producer_kind: ProducerKind::Rule,
        producer: ProducerIdentity::new(RESOLVER_PROVIDER_NAME, RESOLVER_PROVIDER_VERSION, digest)
            .map_err(ResolutionError::InvalidProducer)?,
        binary_digest: context.binary_digest(),
        frontend_version: Some("MS-VBAL-6.1.2/6.1.3.2".to_owned()),
        language: "vba".to_owned(),
        tier: AnalysisTier::TierC,
        build_context: BuildContextIdentity::new(digest),
        input_sources: vec![source.clone()],
        evidence_sources: Vec::new(),
        derivation_parents: vec![FactRef::File(file_id)],
        rule: Some(CATALOG_RULE.to_owned()),
    };
    provenance.id =
        derive_provenance_record_id(&provenance).map_err(ResolutionError::FactIdentity)?;
    let existing: BTreeSet<_> = document.entities.iter().map(|entity| entity.id).collect();
    let names = [
        "VBA".to_owned(),
        "VBA.Constants".to_owned(),
        "VBA.Err".to_owned(),
    ]
    .into_iter()
    .chain(CONSTANTS.iter().map(|name| format!("VBA.Constants.{name}")))
    .chain(
        functions::FUNCTIONS
            .iter()
            .map(|function| format!("VBA.{}", function.name)),
    )
    .chain(
        err::MEMBERS
            .iter()
            .map(|member| format!("VBA.Err.{}", member.name)),
    );
    let mut added = false;
    for name in names {
        cancellation.check()?;
        let claim = claim(document.repository, &name);
        if existing.contains(&claim.symbol) {
            continue;
        }
        document.entities.push(EntityRecord {
            id: claim.symbol,
            repository: document.repository,
            generation: document.generation,
            kind: EntityKind::ExternalSymbol,
            language: "vba".to_owned(),
            tier: AnalysisTier::TierC,
            canonical_name: name.clone(),
            display_name: name.clone(),
            qualified_name: name,
            container: None,
            visibility: EntityVisibility::Public,
            flags: vec![EntityFlag::External, EntityFlag::Synthetic],
            provenance: provenance.id,
            evidence: FactEvidence {
                source: None,
                derivation: vec![FactRef::Fact(provenance.id)],
            },
        });
        // This envelope witnesses model availability, not a source definition.
        document.extensions.push(
            new_symbol_identity_claim_envelope(
                &claim,
                document.generation,
                provenance.id,
                source.clone(),
            )
            .map_err(|_| ResolutionError::UnsupportedIdentityRemap)?,
        );
        added = true;
    }
    if added
        && !document
            .provenance
            .iter()
            .any(|record| record.id == provenance.id)
    {
        document.provenance.push(provenance);
    }
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "borrows the already bounded project and lexical inventories"
)]
pub(super) fn bind<'a>(
    result: &mut ClassicIndex<'a>,
    index: &CandidateIndex<'a>,
    qualifiers: &BTreeMap<rootlight_ir::SourceSpan, Option<&OccurrenceRecord>>,
    member_sites: &[(&OccurrenceRecord, Option<with_members::Member<'_>>)],
    receiver_sources: &BTreeMap<rootlight_ir::SourceSpan, Vec<SourceRef>>,
    memberships: &BTreeMap<FileId, Option<usize>>,
    projects: &[Option<ResolvedProject>],
    members: &BTreeMap<(SymbolId, String), Option<&EntityRecord>>,
    callees: &BTreeMap<FactId, (&OccurrenceRecord, bool)>,
    sources: &BTreeMap<FileId, &[u8]>,
    cancellation: &Cancellation,
    work: &mut ResolutionWorkBudget,
) -> Result<(), ResolutionError> {
    let Some(project) = projects
        .iter()
        .flatten()
        .find(|project| project.evidence.native)
    else {
        return Ok(());
    };
    let Some(file) = index.files.get(&project.evidence.manifest) else {
        return Ok(());
    };
    if !index
        .entities
        .contains_key(&claim(file.repository, "VBA").symbol)
    {
        return Ok(());
    }
    // Receiver syntax and spans were already checked by the shared member pass.
    // Reuse that inventory rather than traversing every expression a second time.
    let mut candidates = BTreeMap::new();
    for (site, member) in member_sites {
        cancellation.check()?;
        work.consume()?;
        let Some(member) = member else {
            continue;
        };
        if member.implicit_root {
            continue;
        }
        if let Some(root) = err::receiver_root(member.receiver, cancellation, work)? {
            if root == "err"
                && let Some(qualifier) = qualifiers.get(&member.span).and_then(|site| *site)
            {
                candidates.entry(qualifier.id).or_insert((
                    qualifier,
                    "VBA.Err".to_owned(),
                    ResolutionRule::VbaErrInstance,
                    root,
                ));
            }
            if let Some(contract) = err::find(member.name) {
                let rule = match (&contract.kind, site.syntax_kind.as_str()) {
                    (err::Kind::Property { .. }, "vba.get_member.reference")
                    | (err::Kind::Property { writable: true }, "vba.let_member.reference") => {
                        Some(ResolutionRule::VbaErrProperty)
                    }
                    (err::Kind::Method { .. }, "vba.invoked_member.reference") => {
                        Some(ResolutionRule::VbaErrMethod)
                    }
                    _ => None,
                };
                if let Some(rule) = rule {
                    candidates.insert(
                        site.id,
                        (*site, format!("VBA.Err.{}", contract.name), rule, root),
                    );
                }
            }
            continue;
        }
        if !starts_with_namespace(member.receiver.as_bytes()) {
            continue;
        }
        let root = identifier(member.receiver).as_deref() == Some("vba");
        if root && let Some(qualifier) = qualifiers.get(&member.span).and_then(|site| *site) {
            candidates.entry(qualifier.id).or_insert((
                qualifier,
                "VBA".to_owned(),
                ResolutionRule::VbaStandardNamespace,
                "vba",
            ));
        }
        let name = identifier(member.name);
        if root
            && name.as_deref() == Some("err")
            && receiver_sources.contains_key(&site.source.span())
            && matches!(
                site.syntax_kind.as_str(),
                "vba.member.reference" | "vba.get_member.reference"
            )
        {
            candidates.insert(
                site.id,
                (
                    *site,
                    "VBA.Err".to_owned(),
                    ResolutionRule::VbaErrInstance,
                    "vba",
                ),
            );
            continue;
        }
        if root
            && matches!(
                site.syntax_kind.as_str(),
                "vba.member.reference"
                    | "vba.get_member.reference"
                    | "vba.invoked_member.reference"
                    | "vba.invoked_get_member.reference"
            )
            && let Some(function) = functions::find(member.name)
        {
            candidates.insert(
                site.id,
                (
                    *site,
                    format!("VBA.{}", function.name),
                    ResolutionRule::VbaStandardFunction,
                    "vba",
                ),
            );
            continue;
        }
        let namespace = receiver_sources.contains_key(&site.source.span());
        if root
            && name.as_deref() == Some("constants")
            && namespace
            && matches!(
                site.syntax_kind.as_str(),
                "vba.member.reference" | "vba.get_member.reference"
            )
        {
            candidates.insert(
                site.id,
                (
                    *site,
                    "VBA.Constants".to_owned(),
                    ResolutionRule::VbaStandardNamespace,
                    "vba",
                ),
            );
            continue;
        }
        if site.syntax_kind != "vba.get_member.reference" || namespace {
            continue;
        }
        let Some(name) = CONSTANTS.iter().find(|candidate| {
            name.as_deref()
                .is_some_and(|name| candidate.eq_ignore_ascii_case(name))
        }) else {
            continue;
        };
        let constants =
            root || member_access_parts(member.receiver.as_bytes(), cancellation, work)?
                .is_some_and(|(root, module)| {
                    identifier(root).as_deref() == Some("vba")
                        && identifier(module).as_deref() == Some("constants")
                });
        if constants {
            candidates.insert(
                site.id,
                (
                    *site,
                    format!("VBA.Constants.{name}"),
                    ResolutionRule::VbaStandardConstant,
                    "vba",
                ),
            );
        }
    }
    let mut lexical_barriers = BTreeMap::new();
    for (site, name, rule, root) in candidates.into_values() {
        cancellation.check()?;
        work.consume()?;
        if site.role != OccurrenceRole::Reference
            || !matches!(site.target, OccurrenceTarget::Unresolved { .. })
            || result.proofs.contains_key(&site.id)
        {
            continue;
        }
        let Some(project) = memberships
            .get(&site.file)
            .copied()
            .flatten()
            .and_then(|i| projects.get(i))
            .and_then(Option::as_ref)
            .filter(|project| project.evidence.native)
        else {
            continue;
        };
        // Project/module names and all lexical declarations remain barriers,
        // including unresolved or conditional declarations. A referenced-library
        // label in retained container metadata is never used as a target proof.
        if project.name == root
            || project.types.contains_key(root)
            || project.modules.contains_key(root)
            || project.value_names.contains(root)
        {
            continue;
        }
        // Declarations cannot change during this pass. Repeated uses in the
        // same file/scope/root share a lexical barrier proof, never a target.
        let key = (site.file, site.enclosing, root);
        let unshadowed = if let Some(&unshadowed) = lexical_barriers.get(&key) {
            unshadowed
        } else {
            let mut scope = site.enclosing;
            let mut unshadowed = false;
            for _ in 0..index.entities.len() {
                cancellation.check()?;
                work.consume()?;
                let Some(owner) = scope.and_then(|id| index.entities.get(&id)) else {
                    break;
                };
                if members.contains_key(&(owner.id, root.to_owned())) {
                    break;
                }
                match owner.container {
                    Some(ContainerRef::Entity(parent)) => scope = Some(parent),
                    Some(ContainerRef::File(file)) if file == site.file => {
                        unshadowed = true;
                        break;
                    }
                    _ => break,
                }
            }
            lexical_barriers.insert(key, unshadowed);
            unshadowed
        };
        if !unshadowed {
            continue;
        }
        let target_id = claim(site.repository, &name).symbol;
        let Some(target) = index.entities.get(&target_id).copied().filter(|target| {
            target.kind == EntityKind::ExternalSymbol
                && target.evidence.source.is_none()
                && index
                    .provenance
                    .get(&target.provenance)
                    .is_some_and(|p| p.rule.as_deref() == Some(CATALOG_RULE))
        }) else {
            continue;
        };
        result.proofs.insert(
            site.id,
            Proof {
                target,
                context: Arc::clone(&project.evidence),
                receiver: None,
                rule,
                property_call: false,
            },
        );
    }
    for (site, member) in member_sites {
        cancellation.check()?;
        work.consume()?;
        if member.is_some() || site.syntax_kind != "vba.application.reference" {
            continue;
        }
        let Some(bytes) = site_bytes(site, sources) else {
            continue;
        };
        // Candidates require an authored VBA or Err qualifier. Avoid reparsing
        // unrelated application expressions; callee lookup proves the full prefix.
        if !starts_with_namespace(bytes)
            && ![b"Err".as_slice(), b"[Err]"].iter().any(|prefix| {
                bytes
                    .get(..prefix.len())
                    .is_some_and(|text| text.eq_ignore_ascii_case(prefix))
            })
            && !bytes
                .get(..4)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"Call"))
        {
            continue;
        }
        // The project pass selected callees against the same immutable source
        // and syntax inventory. Semantic failure cannot change that selection.
        let Some(&(callee, explicit)) = callees.get(&site.id) else {
            continue;
        };
        let Some(proof) = result.proofs.get(&callee.id).filter(|proof| {
            matches!(
                proof.rule,
                ResolutionRule::VbaStandardFunction | ResolutionRule::VbaErrMethod
            )
        }) else {
            continue;
        };
        let (parameters, kind, rule) = match proof.rule {
            ResolutionRule::VbaStandardFunction => {
                let Some(function) = functions::qualified(&proof.target.canonical_name) else {
                    continue;
                };
                (
                    function.parameters,
                    rootlight_vba::ProcedureKind::Function,
                    ResolutionRule::VbaStandardCall,
                )
            }
            ResolutionRule::VbaErrMethod => {
                let Some(err::Member {
                    kind: err::Kind::Method { parameters },
                    ..
                }) = err::qualified(&proof.target.canonical_name)
                else {
                    continue;
                };
                (
                    *parameters,
                    rootlight_vba::ProcedureKind::Sub,
                    ResolutionRule::VbaErrCall,
                )
            }
            _ => continue,
        };
        let Some(offset) = callee
            .source
            .span()
            .end_byte()
            .checked_sub(site.source.span().start_byte())
            .and_then(|offset| usize::try_from(offset).ok())
        else {
            continue;
        };
        let Some(tail) = bytes.get(offset..) else {
            continue;
        };
        if !calls::standard_arguments_match(tail, explicit, parameters, kind, &mut || {
            cancellation.check()?;
            work.consume()
        })? {
            continue;
        }
        let proof = Proof {
            target: proof.target,
            context: Arc::clone(&proof.context),
            receiver: Some((proof.target, callee.source.clone())),
            rule,
            property_call: true,
        };
        result.proofs.insert(site.id, proof);
    }
    Ok(())
}

fn site_bytes<'a>(
    site: &OccurrenceRecord,
    sources: &BTreeMap<FileId, &'a [u8]>,
) -> Option<&'a [u8]> {
    let span = site.source.span();
    sources
        .get(&site.file)?
        .get(usize::try_from(span.start_byte()).ok()?..usize::try_from(span.end_byte()).ok()?)
}

fn starts_with_namespace(bytes: &[u8]) -> bool {
    // Reject unrelated receivers before tokenizing their potentially large
    // call/member chains. The full parser still proves syntax and boundaries.
    bytes
        .get(..3)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"VBA"))
        || bytes
            .get(..5)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"[VBA]"))
}
