//! Isolated data-language plans within a source-owning host document.
//! Local syntax IDs and byte spans stay unchanged; only the internal file-module
//! parent is detached so data address construction cannot cross example boundaries.

use super::*;

pub(super) struct YamlCompleteness {
    markers: Vec<(SourceSpan, bool)>,
    unclipped: bool,
}

impl YamlCompleteness {
    pub(super) fn new(
        facts: &[SyntaxFact],
        diagnostics: &[AdapterDiagnostic],
        cancellation: &Cancellation,
    ) -> Result<Self, AdapterError> {
        cancellation.check()?;
        // A native-clean fence proves its parse context, not capture retention.
        // Query, normalization and sink omissions all carry this clipping diagnostic.
        for diagnostic in diagnostics {
            cancellation.check()?;
            if diagnostic.code().as_str() == "syntax-extraction-limit" {
                return Ok(Self {
                    markers: Vec::new(),
                    unclipped: false,
                });
            }
        }
        let mut markers = Vec::new();
        for fact in facts {
            cancellation.check()?;
            let clean = match fact.syntax_kind().as_str() {
                "markdown.code_parsed.signature" => fact.kind() == SyntaxFactKind::Signature,
                "markdown.code_block.signature"
                | "markdown.code_error.signature"
                | "markdown.code_limit.signature"
                | "markdown.classic_code_parsed.signature"
                | "markdown.classic_code_error.signature"
                | "markdown.fsharp_code_parsed.signature"
                | "markdown.fsharp_code_error.signature"
                | "markdown.fsharp_signature_code_parsed.signature"
                | "markdown.fsharp_signature_code_error.signature"
                | "markdown.cobol_fixed_code_parsed.signature"
                | "markdown.cobol_fixed_code_error.signature"
                | "markdown.cobol_free_code_parsed.signature"
                | "markdown.cobol_free_code_error.signature" => false,
                _ => continue,
            };
            markers
                .try_reserve(1)
                .map_err(|_| provider_failure("embedded-data-allocation"))?;
            markers.push((fact.span(), clean));
        }
        Ok(Self {
            markers,
            unclipped: true,
        })
    }

    pub(super) fn certifies(
        &self,
        facts: &[SyntaxFact],
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        cancellation.check()?;
        if !self.unclipped {
            return Ok(false);
        }
        let mut module = None;
        for fact in facts {
            cancellation.check()?;
            if fact.kind() != SyntaxFactKind::Module
                && fact.syntax_kind().as_str() != "yaml.file.module"
            {
                continue;
            }
            if fact.kind() != SyntaxFactKind::Module
                || fact.syntax_kind().as_str() != "yaml.file.module"
                || fact.parent().is_some()
                || module.replace(fact).is_some()
            {
                return Ok(false);
            }
        }
        let Some(module) = module else {
            return Ok(false);
        };
        let body = module.span();
        if body.start_byte() >= body.end_byte() {
            return Ok(false);
        }
        for fact in facts {
            cancellation.check()?;
            if fact.span().file() != body.file()
                || fact.span().start_byte() < body.start_byte()
                || fact.span().end_byte() > body.end_byte()
                || (fact.parent().is_none() && fact.local_id() != module.local_id())
            {
                return Ok(false);
            }
        }
        // A different fence's recovery does not invalidate this clean body.
        // Missing or overlapping fence evidence cannot establish local completeness.
        let mut owner = false;
        for &(marker, clean) in &self.markers {
            cancellation.check()?;
            if marker.file() != body.file()
                || marker.start_byte() >= body.end_byte()
                || marker.end_byte() <= body.start_byte()
            {
                continue;
            }
            if owner
                || !clean
                || marker.start_byte() > body.start_byte()
                || marker.end_byte() < body.end_byte()
            {
                return Ok(false);
            }
            owner = true;
        }
        Ok(owner)
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_adapter_sdk::SyntaxKindLabel;
    use rootlight_cancel::CancellationReason;

    fn fact(
        id: u64,
        parent: Option<u64>,
        kind: SyntaxFactKind,
        syntax: &str,
        file: FileId,
        range: std::ops::Range<u64>,
    ) -> SyntaxFact {
        SyntaxFact::new(
            id,
            parent,
            kind,
            SourceSpan::new(file, range.start, range.end).unwrap(),
            usize::from(parent.is_some()),
            SyntaxKindLabel::new(syntax).unwrap(),
        )
    }

    fn partition() -> Vec<SyntaxFact> {
        let file = FileId::from_bytes([1; 20]);
        vec![
            fact(
                1,
                None,
                SyntaxFactKind::Module,
                "yaml.file.module",
                file,
                20..40,
            ),
            fact(
                2,
                Some(1),
                SyntaxFactKind::Scope,
                "yaml.document.scope",
                file,
                22..38,
            ),
        ]
    }

    fn marker(id: u64, syntax: &str, range: std::ops::Range<u64>) -> SyntaxFact {
        fact(
            id,
            None,
            SyntaxFactKind::Signature,
            syntax,
            FileId::from_bytes([1; 20]),
            range,
        )
    }

    fn certified(markers: &[SyntaxFact], facts: &[SyntaxFact]) -> bool {
        let cancellation = Cancellation::new();
        YamlCompleteness::new(markers, &[], &cancellation)
            .unwrap()
            .certifies(facts, &cancellation)
            .unwrap()
    }

    #[test]
    fn yaml_completeness_accepts_one_clean_owner_despite_a_recovered_neighbor() {
        let markers = [
            marker(10, "markdown.code_parsed.signature", 10..50),
            marker(11, "markdown.code_error.signature", 60..90),
        ];
        assert!(certified(&markers, &partition()));
    }

    #[test]
    fn yaml_completeness_rejects_missing_or_invalid_detached_modules() {
        let file = FileId::from_bytes([1; 20]);
        let facts = partition();
        let owner = [marker(10, "markdown.code_parsed.signature", 10..50)];
        for invalid in [
            Vec::new(),
            vec![facts[1].clone()],
            vec![fact(
                1,
                None,
                SyntaxFactKind::Scope,
                "yaml.file.module",
                file,
                20..40,
            )],
            vec![fact(
                1,
                None,
                SyntaxFactKind::Module,
                "yaml.document.scope",
                file,
                20..40,
            )],
            vec![fact(
                1,
                Some(9),
                SyntaxFactKind::Module,
                "yaml.file.module",
                file,
                20..40,
            )],
            vec![fact(
                1,
                None,
                SyntaxFactKind::Module,
                "yaml.file.module",
                file,
                20..20,
            )],
            vec![
                facts[0].clone(),
                fact(
                    3,
                    None,
                    SyntaxFactKind::Module,
                    "yaml.file.module",
                    file,
                    21..39,
                ),
            ],
            vec![
                facts[0].clone(),
                fact(
                    2,
                    Some(1),
                    SyntaxFactKind::Scope,
                    "yaml.document.scope",
                    FileId::from_bytes([2; 20]),
                    22..38,
                ),
            ],
            vec![
                facts[0].clone(),
                fact(
                    2,
                    Some(1),
                    SyntaxFactKind::Scope,
                    "yaml.document.scope",
                    file,
                    22..41,
                ),
            ],
            vec![
                facts[0].clone(),
                fact(
                    2,
                    None,
                    SyntaxFactKind::Scope,
                    "yaml.document.scope",
                    file,
                    22..38,
                ),
            ],
        ] {
            assert!(!certified(&owner, &invalid), "{invalid:?}");
        }
    }

    #[test]
    fn yaml_completeness_rejects_missing_or_unqualified_fence_evidence() {
        let file = FileId::from_bytes([1; 20]);
        for markers in [
            Vec::new(),
            vec![fact(
                10,
                None,
                SyntaxFactKind::Scope,
                "markdown.code_parsed.signature",
                file,
                10..50,
            )],
            vec![fact(
                10,
                None,
                SyntaxFactKind::Signature,
                "markdown.code_parsed.signature",
                FileId::from_bytes([2; 20]),
                10..50,
            )],
            vec![marker(10, "markdown.code_parsed.signature", 10..30)],
            vec![marker(10, "markdown.code_parsed.signature", 30..50)],
            vec![marker(10, "markdown.code_parsed.signature", 20..20)],
            vec![marker(10, "markdown.code_parsed.signature", 40..50)],
            vec![marker(10, "markdown.code_error.signature", 10..50)],
            vec![marker(10, "markdown.code_limit.signature", 10..50)],
            vec![marker(10, "markdown.code_block.signature", 10..50)],
            vec![marker(10, "markdown.fsharp_code_parsed.signature", 10..50)],
        ] {
            assert!(!certified(&markers, &partition()), "{markers:?}");
        }
    }

    #[test]
    fn yaml_completeness_rejects_ambiguous_or_conflicting_overlapping_markers() {
        let owner = marker(10, "markdown.code_parsed.signature", 10..50);
        for conflicting in [
            marker(11, "markdown.code_parsed.signature", 10..50),
            marker(11, "markdown.code_parsed.signature", 20..40),
            marker(11, "markdown.code_error.signature", 35..60),
            marker(11, "markdown.code_limit.signature", 10..25),
            fact(
                11,
                None,
                SyntaxFactKind::Scope,
                "markdown.code_parsed.signature",
                FileId::from_bytes([1; 20]),
                10..50,
            ),
        ] {
            assert!(!certified(&[owner.clone(), conflicting], &partition()));
        }
    }

    #[test]
    fn yaml_completeness_vetoes_global_extraction_clipping() {
        let cancellation = Cancellation::new();
        let markers = [marker(10, "markdown.code_parsed.signature", 10..50)];
        for (code, expected) in [
            ("syntax-extraction-limit", false),
            ("syntax-context-limit", true),
            ("syntax-parse-error", true),
        ] {
            let diagnostic = AdapterDiagnostic::new(
                DiagnosticCode::new(code).unwrap(),
                DiagnosticSeverity::Warning,
                None,
                CoverageStatus::Unknown,
            );
            let proof = YamlCompleteness::new(&markers, &[diagnostic], &cancellation).unwrap();
            assert_eq!(
                proof.certifies(&partition(), &cancellation).unwrap(),
                expected
            );
        }
    }

    #[test]
    fn yaml_completeness_propagates_cancellation_even_without_facts() {
        let cancellation = Cancellation::new();
        let proof = YamlCompleteness::new(&[], &[], &cancellation).unwrap();
        assert!(cancellation.cancel(CancellationReason::ClientRequest));
        assert!(matches!(
            YamlCompleteness::new(&[], &[], &cancellation),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
        assert!(matches!(
            proof.certifies(&[], &cancellation),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
    }
}
