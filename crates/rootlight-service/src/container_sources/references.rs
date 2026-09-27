//! Projects native reference declarations without claiming external resolution.
//! Exact decompressed directory bytes are retained as a derived source, so every
//! dependency is readable after restart without reopening a library or registry.

use super::*;
use rootlight_ir::{
    Confidence, ContainerRef, EntityFlag, EntityVisibility, EvidenceKind, SymbolIdentityClaim,
    new_symbol_identity_claim_envelope,
};
use rootlight_source::{
    vba_dir::ReferencePayload,
    vba_reference::{ReferenceField, ReferenceIdentity},
    vba_source::VbaProject,
};

pub(super) struct DeclaredReference {
    file: FileId,
    owner: FileId,
    range: std::ops::Range<usize>,
    identity: String,
    display: String,
    name: Option<DeclaredName>,
}

struct DeclaredName {
    text: String,
    range: std::ops::Range<usize>,
}

pub(super) fn prepare(
    input: &ManifestInput,
    project: &VbaProject<'_, '_>,
    repository: RepositoryId,
    budget: &mut Budget,
    prepared: &mut PreparedSources,
    snapshots: &mut Vec<SourceSnapshot>,
    cancellation: &Cancellation,
) -> Result<(), FirstSliceError> {
    if project.references().is_empty() {
        return Ok(());
    }
    let bytes = project.decoded_directory().bytes();
    if budget.files == 0 || bytes.len() > budget.bytes {
        return prepared.issue(
            input.file,
            "vba reference metadata admission exhausted".to_owned(),
        );
    }
    let path = match RelativePath::parse(Path::new(&format!("{}/@vba/references.bin", input.path)))
    {
        Ok(path) => path,
        Err(_) => {
            return prepared.issue(
                input.file,
                "vba reference metadata path exceeds canonical path limits".to_owned(),
            );
        }
    };
    let file = derive_file(rootlight_ids::FileIdentity {
        repository,
        path_identity: path.identity_bytes(),
    })
    .id();
    let hash = content_hash(bytes);
    let mut retained = Vec::new();
    retained
        .try_reserve_exact(bytes.len())
        .map_err(|_| FirstSliceError::Limits)?;
    retained.extend_from_slice(bytes);
    let snapshot = SourceSnapshot::from_persisted(repository, path.clone(), file, hash, retained)
        .map_err(|_| FirstSliceError::Identity)?;
    prepared
        .inputs
        .try_reserve(1)
        .map_err(|_| FirstSliceError::Limits)?;
    prepared
        .origins
        .try_reserve(1)
        .map_err(|_| FirstSliceError::Limits)?;
    prepared
        .fingerprints
        .try_reserve(2)
        .map_err(|_| FirstSliceError::Limits)?;
    snapshots
        .try_reserve(1)
        .map_err(|_| FirstSliceError::Limits)?;
    prepared.inputs.push(ManifestInput {
        file,
        path: path.as_str().to_owned(),
        content_hash: hash,
        bytes: u64::try_from(bytes.len()).map_err(|_| FirstSliceError::Limits)?,
        encoding: "binary".to_owned(),
        class: input.class,
        language_signals: vec![LanguageSignal {
            language: "vba".to_owned(),
            evidence: LanguageEvidence::Content,
        }],
        decisive_rule: None,
    });
    prepared.origins.push((file, input.file));
    prepared.fingerprints.extend([
        InputFingerprint::new(InputKey::FileContent(file), hash),
        InputFingerprint::new(
            InputKey::FilePath(file),
            content_hash(path.identity_bytes()),
        ),
    ]);
    snapshots.push(snapshot);
    budget.files -= 1;
    budget.bytes = budget
        .bytes
        .checked_sub(bytes.len())
        .ok_or(FirstSliceError::Limits)?;
    for (ordinal, reference) in project.references().iter().enumerate() {
        check_cancellation(cancellation)?;
        let fields: &[ReferenceField] = match reference.payload() {
            ReferencePayload::Registered { .. } => &[ReferenceField::Registered],
            ReferencePayload::Project { .. } => &[ReferenceField::ProjectAbsolute],
            ReferencePayload::Control {
                original_libid: Some(_),
                ..
            } => &[
                ReferenceField::ControlOriginal,
                ReferenceField::ControlTwiddled,
                ReferenceField::ControlExtended,
            ],
            ReferencePayload::Control {
                original_libid: None,
                ..
            } => &[
                ReferenceField::ControlTwiddled,
                ReferenceField::ControlExtended,
            ],
            _ => {
                prepared.issue(
                    file,
                    format!("vba reference {ordinal} unsupported reference record"),
                )?;
                continue;
            }
        };
        for field in fields {
            let reserved = budget.reserve();
            let mut limits = TextLimits::default();
            limits.input_bytes = reserved;
            limits.output_bytes = reserved;
            let identifier =
                match project.reference_identifier(ordinal, *field, limits, cancellation) {
                    Ok(identifier) => identifier,
                    Err(error) => {
                        check_cancellation(cancellation)?;
                        prepared.issue(file, reference_error(ordinal, *field, &error))?;
                        continue;
                    }
                };
            let used = identifier
                .path()
                .len()
                .checked_add(identifier.display_name().map_or(0, str::len))
                .and_then(|n| n.checked_add(128))
                .ok_or(FirstSliceError::Limits)?;
            budget.settle(reserved, used.min(reserved))?;
            let (identity, range) = match identifier.identity() {
                ReferenceIdentity::Library {
                    platform,
                    guid,
                    major,
                    minor,
                    lcid,
                } => {
                    if absent_twiddled(*field, identifier.original_bytes()) {
                        continue;
                    }
                    use std::fmt::Write as _;
                    let mut guid_text = String::new();
                    guid_text
                        .try_reserve_exact(32)
                        .map_err(|_| FirstSliceError::Limits)?;
                    for byte in guid {
                        write!(guid_text, "{byte:02x}").map_err(|_| FirstSliceError::Limits)?;
                    }
                    let platform = match platform {
                        rootlight_source::vba_reference::ReferencePlatform::Windows => "windows",
                        rootlight_source::vba_reference::ReferencePlatform::Macintosh => {
                            "macintosh"
                        }
                    };
                    (
                        format!("vba-library:{platform}:{guid_text}:{major:x}.{minor:x}:{lcid:x}"),
                        identifier.source_range(),
                    )
                }
                ReferenceIdentity::Project { .. } => {
                    // One project record has two path representations, not two
                    // independent dependencies. Retain both in its exact source.
                    let reserved = budget.reserve();
                    limits.input_bytes = reserved;
                    limits.output_bytes = reserved;
                    let relative = match project.reference_identifier(
                        ordinal,
                        ReferenceField::ProjectRelative,
                        limits,
                        cancellation,
                    ) {
                        Ok(relative) => relative,
                        Err(error) => {
                            check_cancellation(cancellation)?;
                            prepared.issue(
                                file,
                                reference_error(ordinal, ReferenceField::ProjectRelative, &error),
                            )?;
                            continue;
                        }
                    };
                    budget.settle(
                        reserved,
                        relative.path().len().saturating_add(3).min(reserved),
                    )?;
                    if relative.identity() != identifier.identity() {
                        prepared.issue(
                            file,
                            format!("vba reference {ordinal} conflicting project reference kinds"),
                        )?;
                        continue;
                    }
                    let range = reference.range();
                    let raw = bytes.get(range.clone()).ok_or(FirstSliceError::Identity)?;
                    (
                        format!("vba-project-reference:{}", content_hash(raw)),
                        range,
                    )
                }
                _ => {
                    prepared.issue(
                        file,
                        format!("vba reference {ordinal} unsupported identifier kind"),
                    )?;
                    continue;
                }
            };
            let display = identifier
                .display_name()
                .filter(|name| !name.is_empty())
                .unwrap_or(&identity)
                .to_owned();
            // A control's extended library has its own name. Never infer it
            // from the leading name of the original/twiddled library.
            let name = match (field, reference.payload()) {
                (
                    ReferenceField::ControlExtended,
                    ReferencePayload::Control { extended_name, .. },
                ) => extended_name.as_ref(),
                _ => reference.name(),
            };
            let name = if let Some(name) = name {
                let reserved = budget.reserve();
                limits.input_bytes = reserved;
                limits.output_bytes = reserved;
                match declared_name(
                    bytes,
                    name.name_range(),
                    name.unicode_name_range(),
                    project.code_page(),
                    limits,
                    cancellation,
                ) {
                    Ok(name) => {
                        budget.settle(
                            reserved,
                            name.range.len().max(name.text.len()).min(reserved),
                        )?;
                        Some(name)
                    }
                    Err(error) => {
                        check_cancellation(cancellation)?;
                        prepared.issue(
                            file,
                            format!("{} name rejected", reference_error(ordinal, *field, &error)),
                        )?;
                        None
                    }
                }
            } else {
                None
            };
            prepared
                .references
                .try_reserve(1)
                .map_err(|_| FirstSliceError::Limits)?;
            prepared.references.push(DeclaredReference {
                file,
                owner: input.file,
                range,
                identity,
                display,
                name,
            });
        }
    }
    Ok(())
}

fn declared_name(
    bytes: &[u8],
    range: std::ops::Range<usize>,
    unicode_range: std::ops::Range<usize>,
    code_page: u16,
    limits: TextLimits,
    cancellation: &Cancellation,
) -> Result<DeclaredName, rootlight_source::vba_reference::ReferenceError> {
    use rootlight_source::{
        vba_reference::ReferenceError,
        vba_text::{DecodedText, TextError},
    };
    if unicode_range.start < range.end {
        return Err(ReferenceError::Range);
    }
    let original = bytes.get(range.clone()).ok_or(ReferenceError::Range)?;
    let unicode = bytes
        .get(unicode_range.clone())
        .ok_or(ReferenceError::Range)?;
    let length = original
        .len()
        .checked_add(unicode.len())
        .ok_or(ReferenceError::Range)?;
    if length > limits.input_bytes {
        return Err(ReferenceError::Text(TextError::Limit {
            resource: "name bytes",
            maximum: limits.input_bytes,
        }));
    }
    let decoded = DecodedText::decode(original, code_page, limits, cancellation)
        .map_err(ReferenceError::Text)?;
    if decoded.text().is_empty() {
        return Err(ReferenceError::Invalid {
            offset: range.start,
        });
    }
    // REFERENCENAME requires identical text in both encodings. Comparing exact
    // UTF-16LE bytes avoids normalization, lossy fallback and a second allocation.
    let mut expected = decoded.text().encode_utf16().flat_map(u16::to_le_bytes);
    for (offset, byte) in unicode.iter().enumerate() {
        if offset % 1024 == 0 {
            cancellation
                .check()
                .map_err(TextError::Cancelled)
                .map_err(ReferenceError::Text)?;
        }
        if expected.next() != Some(*byte) {
            return Err(ReferenceError::Invalid {
                offset: unicode_range.start + offset,
            });
        }
    }
    if expected.next().is_some() {
        return Err(ReferenceError::Invalid {
            offset: unicode_range.end,
        });
    }
    let mut text = String::new();
    text.try_reserve_exact(decoded.text().len())
        .map_err(|_| ReferenceError::Text(TextError::Limits))?;
    text.push_str(decoded.text());
    Ok(DeclaredName {
        text,
        range: range.start..unicode_range.end,
    })
}

fn absent_twiddled(field: ReferenceField, bytes: &[u8]) -> bool {
    // MS-OVBA 2.3.4.2.2.3 defines a case-sensitive sentinel, not a general
    // equivalence class of nil-GUID library identifiers.
    field == ReferenceField::ControlTwiddled
        && bytes == br"*\G{00000000-0000-0000-0000-000000000000}#0.0#0##"
}

fn reference_error(
    ordinal: usize,
    field: ReferenceField,
    error: &rootlight_source::vba_reference::ReferenceError,
) -> String {
    use rootlight_source::{vba_reference::ReferenceError, vba_text::TextError};
    let field = match field {
        ReferenceField::Registered => "registered",
        ReferenceField::ProjectAbsolute => "absolute project",
        ReferenceField::ProjectRelative => "relative project",
        ReferenceField::ControlOriginal => "original control",
        ReferenceField::ControlTwiddled => "twiddled control",
        ReferenceField::ControlExtended => "extended control",
    };
    // Public index receipts only admit a source-free diagnostic alphabet. Match
    // typed errors instead of sanitizing arbitrary display text into that channel.
    let detail = match error {
        ReferenceError::Unavailable => "identifier unavailable".to_owned(),
        ReferenceError::Range => "invalid identifier range".to_owned(),
        ReferenceError::Invalid { offset } => format!("invalid identifier at byte {offset}"),
        ReferenceError::Text(TextError::Unsupported { code_page }) => {
            format!("unsupported code page {code_page}")
        }
        ReferenceError::Text(TextError::Invalid { code_page, offset }) => {
            format!("invalid code page {code_page} sequence at byte {offset}")
        }
        ReferenceError::Text(TextError::Limit { .. }) => {
            "decoding resource limit exceeded".to_owned()
        }
        ReferenceError::Text(TextError::Limits) => "invalid decoding limits".to_owned(),
        ReferenceError::Text(TextError::Cancelled(_)) => "decoding cancelled".to_owned(),
        _ => "unsupported reference decoding error".to_owned(),
    };
    format!("vba reference {ordinal} {field} {detail}")
}

pub(super) fn append(
    references: &[DeclaredReference],
    document: &mut NormalizedIrDocument,
    limits: &IrLimits,
    cancellation: &Cancellation,
) -> Result<BTreeMap<(FileId, IrFactDomain), u64>, FirstSliceError> {
    let mut increments = BTreeMap::new();
    if references.is_empty() {
        return Ok(increments);
    }
    let module_count = references
        .iter()
        .map(|reference| reference.file)
        .collect::<BTreeSet<_>>()
        .len();
    let entity_count = references
        .len()
        .checked_add(module_count)
        .ok_or(FirstSliceError::Limits)?;
    checked_combined_length(
        normalized_record_count(document)?,
        references
            .len()
            .checked_mul(4)
            .and_then(|count| {
                module_count
                    .checked_mul(2)
                    .and_then(|modules| count.checked_add(modules))
            })
            .ok_or(FirstSliceError::Limits)?,
        limits.max_total_records,
    )?;
    reserve_records(&mut document.entities, entity_count, limits.max_entities)?;
    reserve_records(
        &mut document.relations,
        references.len(),
        limits.max_relations,
    )?;
    reserve_records(
        &mut document.extensions,
        entity_count,
        limits.max_extensions,
    )?;
    reserve_records(
        &mut document.provenance,
        references.len(),
        limits.max_provenance_records,
    )?;
    let files: BTreeMap<_, _> = document.files.iter().map(|file| (file.id, file)).collect();
    let mut modules = BTreeMap::new();
    for reference in references {
        check_cancellation(cancellation)?;
        let file = files
            .get(&reference.file)
            .ok_or(FirstSliceError::Identity)?;
        if !files.contains_key(&reference.owner) {
            return Err(FirstSliceError::Identity);
        }
        let evidence_range = reference.name.as_ref().map_or_else(
            || reference.range.clone(),
            |name| {
                reference.range.start.min(name.range.start)..reference.range.end.max(name.range.end)
            },
        );
        let source = SourceRef::new(
            document.repository,
            document.generation,
            SourceSpan::new(
                file.id,
                u64::try_from(evidence_range.start).map_err(|_| FirstSliceError::Limits)?,
                u64::try_from(evidence_range.end).map_err(|_| FirstSliceError::Limits)?,
            )
            .map_err(|_| FirstSliceError::Identity)?,
            file.content_hash,
            None,
        );
        let inventory_source = SourceRef::new(
            document.repository,
            document.generation,
            SourceSpan::new(file.id, 0, file.byte_length).map_err(|_| FirstSliceError::Identity)?,
            file.content_hash,
            None,
        );
        let mut provenance = ProvenanceRecord {
            id: FactId::from_bytes([0; 20]),
            repository: document.repository,
            generation: document.generation,
            producer_kind: ProducerKind::Rule,
            producer: ProducerIdentity::new(
                "rootlight-container-sources",
                "1.0.0",
                first_slice_build_context(),
            )
            .map_err(|_| FirstSliceError::Identity)?,
            binary_digest: content_hash(REVISION),
            frontend_version: Some("office-vba-1".to_owned()),
            language: "vba".to_owned(),
            tier: AnalysisTier::TierC,
            build_context: BuildContextIdentity::new(first_slice_build_context()),
            input_sources: vec![inventory_source.clone()],
            evidence_sources: vec![source.clone()],
            derivation_parents: Vec::new(),
            rule: Some("declared-vba-reference-unresolved".to_owned()),
        };
        provenance.id =
            derive_provenance_record_id(&provenance).map_err(|_| FirstSliceError::Identity)?;
        let module = if let Some(module) = modules.get(&file.id) {
            *module
        } else {
            let mut container_identity = vec![1];
            container_identity.extend_from_slice(reference.owner.as_bytes());
            let mut claim = SymbolIdentityClaim {
                symbol: SymbolId::from_bytes([0; 20]),
                repository: document.repository,
                language: "vba".to_owned(),
                kind: EntityKind::Module,
                container: Some(ContainerRef::File(reference.owner)),
                container_identity,
                declared_identity: file.path.clone(),
                signature_discriminator: Vec::new(),
                build_context_discriminator: first_slice_build_context().as_bytes().to_vec(),
            };
            claim.symbol = claim.derived_symbol();
            // The query graph uses symbol endpoints. This explicit synthetic
            // inventory node is not a VBA source module or library definition.
            document.entities.push(EntityRecord {
                id: claim.symbol,
                repository: document.repository,
                generation: document.generation,
                kind: EntityKind::Module,
                language: claim.language.clone(),
                tier: AnalysisTier::TierC,
                canonical_name: file.path.clone(),
                display_name: file.path.clone(),
                qualified_name: file.path.clone(),
                container: claim.container,
                visibility: EntityVisibility::Unknown,
                flags: vec![EntityFlag::Synthetic],
                provenance: provenance.id,
                evidence: FactEvidence {
                    source: Some(inventory_source.clone()),
                    derivation: Vec::new(),
                },
            });
            document.extensions.push(
                new_symbol_identity_claim_envelope(
                    &claim,
                    document.generation,
                    provenance.id,
                    inventory_source,
                )
                .map_err(|_| FirstSliceError::Identity)?,
            );
            modules.insert(file.id, claim.symbol);
            for domain in [IrFactDomain::Entities, IrFactDomain::Extensions] {
                increments.insert((file.id, domain), 1);
            }
            claim.symbol
        };
        let mut container_identity = vec![1];
        container_identity.extend_from_slice(file.id.as_bytes());
        // Declaration occurrences remain distinct even when libraries repeat;
        // these proxies do not assert globally resolved library symbol identity.
        let mut claim = SymbolIdentityClaim {
            symbol: SymbolId::from_bytes([0; 20]),
            repository: document.repository,
            language: "vba".to_owned(),
            kind: EntityKind::ExternalSymbol,
            container: Some(ContainerRef::File(file.id)),
            container_identity,
            declared_identity: reference.identity.clone(),
            signature_discriminator: u64::try_from(reference.range.start)
                .map_err(|_| FirstSliceError::Limits)?
                .to_le_bytes()
                .to_vec(),
            build_context_discriminator: first_slice_build_context().as_bytes().to_vec(),
        };
        claim.symbol = claim.derived_symbol();
        let evidence = FactEvidence {
            source: Some(source.clone()),
            derivation: Vec::new(),
        };
        document.entities.push(EntityRecord {
            id: claim.symbol,
            repository: document.repository,
            generation: document.generation,
            kind: EntityKind::ExternalSymbol,
            language: claim.language.clone(),
            tier: AnalysisTier::TierC,
            canonical_name: reference.identity.clone(),
            display_name: reference.display.clone(),
            // This is a source-authored lookup label, not proof of identifier
            // grammar, uniqueness, or a resolved external namespace.
            qualified_name: reference
                .name
                .as_ref()
                .map_or(&reference.identity, |name| &name.text)
                .clone(),
            container: claim.container,
            visibility: EntityVisibility::Unknown,
            flags: vec![EntityFlag::External, EntityFlag::Synthetic],
            provenance: provenance.id,
            evidence: evidence.clone(),
        });
        document.extensions.push(
            new_symbol_identity_claim_envelope(&claim, document.generation, provenance.id, source)
                .map_err(|_| FirstSliceError::Identity)?,
        );
        let mut relation = RelationRecord {
            id: FactId::from_bytes([0; 20]),
            repository: document.repository,
            generation: document.generation,
            subject: RelationEndpoint::Entity(module),
            predicate: RelationPredicate::Imports,
            object: RelationEndpoint::Entity(claim.symbol),
            confidence: Confidence::new(1000).map_err(|_| FirstSliceError::Identity)?,
            evidence_kind: EvidenceKind::Syntax,
            provenance: provenance.id,
            evidence,
        };
        relation.id =
            derive_relation_record_id(&relation).map_err(|_| FirstSliceError::Identity)?;
        document.relations.push(relation);
        document.provenance.push(provenance);
        for domain in [
            IrFactDomain::Entities,
            IrFactDomain::Relations,
            IrFactDomain::Extensions,
            IrFactDomain::Provenance,
        ] {
            let count = increments.entry((file.id, domain)).or_insert(0u64);
            *count = count.checked_add(1).ok_or(FirstSliceError::Limits)?;
        }
    }
    Ok(increments)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_source::{vba_reference::ReferenceError, vba_text::TextError};

    #[test]
    fn declared_names_enforce_decoding_limits_and_exact_encoding_equivalence() {
        for (original, text, page) in [
            (b"Alias".as_slice(), "Alias", 1252),
            (b"Caf\xe9".as_slice(), "Café", 1252),
            ("Δ𝄞".as_bytes(), "Δ𝄞", 65001),
        ] {
            let mut bytes = original.to_vec();
            let start = bytes.len();
            bytes.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
            let mut limits = TextLimits::default();
            limits.input_bytes = bytes.len();
            limits.output_bytes = text.len();
            let cancellation = Cancellation::new();
            let decoded = declared_name(
                &bytes,
                0..start,
                start..bytes.len(),
                page,
                limits,
                &cancellation,
            )
            .unwrap();
            assert_eq!(decoded.text, text);
            assert_eq!(decoded.range, 0..bytes.len());
            limits.input_bytes -= 1;
            assert!(matches!(
                declared_name(
                    &bytes,
                    0..start,
                    start..bytes.len(),
                    page,
                    limits,
                    &cancellation
                ),
                Err(ReferenceError::Text(TextError::Limit { .. }))
            ));
            limits.input_bytes += 1;
            limits.output_bytes -= 1;
            assert!(matches!(
                declared_name(
                    &bytes,
                    0..start,
                    start..bytes.len(),
                    page,
                    limits,
                    &cancellation
                ),
                Err(ReferenceError::Text(TextError::Limit { .. }))
            ));
            limits.output_bytes += 1;
            assert!(matches!(
                declared_name(
                    &bytes,
                    0..start,
                    start..bytes.len(),
                    932,
                    limits,
                    &cancellation
                ),
                Err(ReferenceError::Text(TextError::Unsupported {
                    code_page: 932
                }))
            ));
            for unicode in [
                start..bytes.len() - 1,
                start..bytes.len() + 1,
                0..bytes.len(),
            ] {
                assert!(
                    declared_name(&bytes, 0..start, unicode, page, limits, &cancellation).is_err()
                );
            }
            cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest);
            assert!(matches!(
                declared_name(
                    &bytes,
                    0..start,
                    start..bytes.len(),
                    page,
                    limits,
                    &cancellation
                ),
                Err(ReferenceError::Text(TextError::Cancelled(_)))
            ));
        }
    }

    #[test]
    fn only_the_exact_twiddled_sentinel_omits_an_external_dependency() {
        let sentinel = br"*\G{00000000-0000-0000-0000-000000000000}#0.0#0##";
        assert!(absent_twiddled(ReferenceField::ControlTwiddled, sentinel));
        for field in [
            ReferenceField::Registered,
            ReferenceField::ControlOriginal,
            ReferenceField::ControlExtended,
        ] {
            assert!(!absent_twiddled(field, sentinel));
        }
        for other in [
            br"*\H{00000000-0000-0000-0000-000000000000}#0.0#0##".as_slice(),
            br"*\G{00000000-0000-0000-0000-000000000000}#00.0#0##".as_slice(),
            br"*\G{00000000-0000-0000-0000-000000000000}#0.0#0##name".as_slice(),
        ] {
            assert!(!absent_twiddled(ReferenceField::ControlTwiddled, other));
        }
    }

    #[test]
    fn typed_reference_diagnostics_preserve_public_source_free_boundaries() {
        for error in [
            ReferenceError::Unavailable,
            ReferenceError::Range,
            ReferenceError::Invalid { offset: 123 },
            ReferenceError::Text(TextError::Unsupported { code_page: 932 }),
            ReferenceError::Text(TextError::Invalid {
                code_page: 1252,
                offset: 9,
            }),
            ReferenceError::Text(TextError::Limits),
            ReferenceError::Text(TextError::Limit {
                resource: "untrusted-private-payload",
                maximum: 99,
            }),
        ] {
            for field in [
                ReferenceField::Registered,
                ReferenceField::ProjectAbsolute,
                ReferenceField::ProjectRelative,
                ReferenceField::ControlOriginal,
                ReferenceField::ControlTwiddled,
                ReferenceField::ControlExtended,
            ] {
                let message = reference_error(4, field, &error);
                assert!(public_diagnostic_message(&message));
                assert!(!message.contains("untrusted-private-payload"));
            }
        }
    }
}
