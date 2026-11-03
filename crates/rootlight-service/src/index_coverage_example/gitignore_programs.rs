//! Observe native Gitignore programs through the public generation inspection.
//! Canonical decoding and matcher reconstruction qualify this inventory only;
//! repository-wide ignore application remains an explicit analysis gap.

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
};

use rootlight_adapters::GitignoreMatcher;
use rootlight_cancel::Cancellation;
use rootlight_ir::{
    GITIGNORE_PROGRAM_NAMESPACE, NormalizedIrDocument, decode_gitignore_program_envelope,
};
use serde_json::{Value, json};

pub(super) fn inspect(document: &NormalizedIrDocument) -> Result<Option<Value>, Box<dyn Error>> {
    let files = document
        .files
        .iter()
        .filter(|file| file.language == "gitignore")
        .map(|file| (file.id, file))
        .collect::<BTreeMap<_, _>>();
    let envelopes = document
        .extensions
        .iter()
        .filter(|envelope| envelope.namespace == GITIGNORE_PROGRAM_NAMESPACE);
    let mut seen = BTreeSet::new();
    let mut programs = Vec::new();
    let mut total_rules = 0_usize;
    for envelope in envelopes {
        let program = decode_gitignore_program_envelope(envelope)?;
        let source = program.source();
        let file = files
            .get(&source.span().file())
            .ok_or("Gitignore program has no native file")?;
        if !seen.insert(file.id)
            || source.repository() != document.repository
            || source.generation() != document.generation
            || source.content_hash() != file.content_hash
            || source.span().start_byte() != 0
            || source.span().end_byte() != file.byte_length
            || u64::try_from(program.text().len())? != file.byte_length
        {
            return Err("Gitignore program source identity or uniqueness differs".into());
        }
        program.verify_source_text(program.text())?;
        let matcher = GitignoreMatcher::from_program(program, &Cancellation::new())?;
        let program = matcher.program();
        total_rules = total_rules
            .checked_add(program.rules().len())
            .ok_or("Gitignore rule count overflow")?;
        let context_gaps = document
            .skipped_regions
            .iter()
            .filter(|gap| {
                gap.source.span().file() == file.id
                    && gap.detail == "gitignore-repository-context-not-analyzed"
            })
            .count();
        programs.push(json!({
            "path":file.path, "file":file.id, "content_hash":file.content_hash,
            "bytes":file.byte_length, "program_id":envelope.id,
            "rule_count":program.rules().len(), "rules":program.rules(),
            "canonical_verified":true, "source_identity_verified":true,
            "matcher_recompiled":true, "repository_context_gaps":context_gaps,
            "repository_context_qualified":false
        }));
    }
    if files.is_empty() && programs.is_empty() {
        return Ok(None);
    }
    programs.sort_by(|left, right| left["path"].as_str().cmp(&right["path"].as_str()));
    let missing = files.values().filter(|file| !seen.contains(&file.id))
        .map(|file| json!({"path":file.path,"file":file.id,"reason":"native_program_absent_or_refused"}))
        .collect::<Vec<_>>();
    Ok(Some(json!({
        "schema":"rootlight-gitignore-program-observation/1",
        "scope":"public_generation_snapshot_canonical_programs_and_recompiled_matchers",
        "repository":document.repository, "generation":document.generation,
        "retained_gitignore_files":files.len(), "verified_programs":programs.len(),
        "verified_rules":total_rules, "all_retained_files_have_programs":missing.is_empty(),
        "programs":programs, "missing_or_refused":missing,
        "repository_context_qualified":false, "whole_language_qualified":false
    })))
}
