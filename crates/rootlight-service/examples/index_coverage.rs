//! Audits a fresh durable index through public service and source APIs.
//! Explicit configuration and default detection are separate invocations; counts
//! describe recorded evidence, not an independent semantic completeness oracle.

use std::{
    collections::BTreeMap,
    error::Error,
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use rootlight_cancel::Cancellation;
use rootlight_ir::{OccurrenceTarget, SourceRef, SourceSpan};
use rootlight_runtime::RuntimePaths;
use rootlight_service::{FirstSliceBudget, FirstSliceService, FirstSliceStoragePolicy};
use rootlight_source::{SourceEncoding, SourceReadOptions};
use rootlight_storage::{
    HARD_MAX_SHARED_GENERATION_BUNDLE_BYTES, HARD_MAX_SHARED_GENERATION_HEADER_BYTES,
    SharedGenerationExpectation, SharedGenerationLimits,
};
use serde_json::json;

fn deadline() -> Result<Cancellation, Box<dyn Error>> {
    let end = Instant::now()
        .checked_add(Duration::from_secs(3600))
        .ok_or("diagnostic deadline is not representable")?;
    Ok(Cancellation::with_deadline(end))
}

fn read_bounded(path: &Path, maximum: u64) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(maximum.checked_add(1).ok_or("read bound overflow")?)
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len())? > maximum {
        return Err("diagnostic input exceeds its read bound".into());
    }
    Ok(bytes)
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if !(2..=3).contains(&args.len()) {
        return Err("usage: index_coverage REPOSITORY NEW_STATE_DIRECTORY [CONFIG_FILE]".into());
    }
    let repository = PathBuf::from(&args[0]);
    let state = PathBuf::from(&args[1]);
    let config = args
        .get(2)
        .map(|path| -> Result<String, Box<dyn Error>> {
            Ok(String::from_utf8(read_bounded(
                Path::new(path),
                u64::try_from(rootlight_config::MAX_CONFIG_LAYER_BYTES)?,
            )?)?)
        })
        .transpose()?;
    let report = inspect(&repository, &state, config.as_deref())?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

fn inspect(
    repository: &Path,
    state: &Path,
    config: Option<&str>,
) -> Result<serde_json::Value, Box<dyn Error>> {
    let repository = fs::canonicalize(repository)?;
    let policy = FirstSliceStoragePolicy::resolve_user_config(config, 2)?;
    // Refuse existing state so a previous generation cannot masquerade as a cold run.
    fs::create_dir(state)?;
    let paths = RuntimePaths::new(state.join("state"), state.join("runtime"))?;
    paths.prepare_owner()?;
    let mut service =
        FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline()?)?;
    let cold_start = Instant::now();
    let cold = service.index_repository(&repository, &deadline()?)?;
    let cold_micros = cold_start.elapsed().as_micros();
    let no_op_start = Instant::now();
    let no_op = service.index_repository(&repository, &deadline()?)?;
    let no_op_micros = no_op_start.elapsed().as_micros();
    if cold.generation != no_op.generation {
        return Err("unchanged repository produced a different generation".into());
    }
    let status = service.repository_status(cold.repository, Some(cold.generation))?;
    // This local inspection requests the existing transfer ceiling; it does not
    // change the default wire policy or the indexing/generation resource budgets.
    let transfer_limits = SharedGenerationLimits::new(
        HARD_MAX_SHARED_GENERATION_HEADER_BYTES,
        HARD_MAX_SHARED_GENERATION_BUNDLE_BYTES,
    )?;
    let exported = service
        .export_shared_generation(
            cold.repository,
            Some(cold.generation),
            transfer_limits,
            &deadline()?,
        )
        .map_err(|error| {
            format!(
                "generation export failed: {error}; indexed files={}, entities={}",
                cold.indexed_files, cold.entities
            )
        })?;
    let imported = service
        .import_shared_generation(
            exported.bundle(),
            SharedGenerationExpectation::new(cold.repository, exported.source_set_hash())
                .with_generation(cold.generation),
            transfer_limits,
            &deadline()?,
        )
        .map_err(|error| {
            format!(
                "generation import failed: {error}; bundle bytes={}",
                exported.bundle().len()
            )
        })?;
    let document = imported.generation().document();
    let mut rows = Vec::new();
    for file in &document.files {
        let source = file
            .evidence
            .source
            .clone()
            .ok_or("file has no generation-bound source")?;
        if source.generation() != cold.generation {
            return Err("file source belongs to a different generation".into());
        }
        let relative = rootlight_vfs::RelativePath::parse(std::path::Path::new(&file.path))?;
        let path = fs::canonicalize(repository.join(relative.as_str()))?;
        if !path.starts_with(&repository) {
            return Err("source escaped repository".into());
        }
        let live = read_bounded(&path, file.byte_length)?;
        if u64::try_from(live.len())? != file.byte_length
            || rootlight_ids::content_hash(&live) != file.content_hash
        {
            return Err("live source identity differs from the indexed file".into());
        }
        let text = (file.encoding == "utf-8")
            .then(|| std::str::from_utf8(&live).ok())
            .flatten();
        let encoding = if text.is_some() {
            SourceEncoding::Utf8
        } else {
            SourceEncoding::Bytes
        };
        let mut start = 0_usize;
        loop {
            let end = text.map_or_else(
                || start.saturating_add(16 * 1024).min(live.len()),
                |text| chunk_end(text, start),
            );
            let part = SourceRef::new(
                source.repository(),
                source.generation(),
                SourceSpan::new(file.id, u64::try_from(start)?, u64::try_from(end)?)?,
                source.content_hash(),
                None,
            );
            let read = service.source_read_with_options_and_budget(
                cold.generation,
                vec![part.clone()],
                SourceReadOptions::new()
                    .with_context_lines_before(0)
                    .with_context_lines_after(0)
                    .with_encoding(encoding),
                FirstSliceBudget::default(),
                &deadline()?,
            )?;
            if read.data.generation != cold.generation || read.data.chunks.len() != 1 {
                return Err(format!(
                    "source retrieval returned an unexpected generation or chunk count: {}",
                    file.path
                )
                .into());
            }
            let chunk = &read.data.chunks[0];
            if chunk.reference != part
                || chunk.start_byte != u64::try_from(start)?
                || chunk.end_byte != u64::try_from(end)?
                || Some(chunk.bytes.as_slice()) != live.get(start..end)
            {
                return Err(format!("source retrieval differs from the live file: {} at {start}..{end}, returned {}..{} ({} bytes)",
                    file.path, chunk.start_byte, chunk.end_byte, chunk.bytes.len()).into());
            }
            if end == live.len() {
                break;
            }
            start = end;
        }
        let mut occurrences = BTreeMap::<String, usize>::new();
        for site in &document.occurrences {
            if site.source.span().file() != file.id {
                continue;
            }
            let resolution = match site.target {
                OccurrenceTarget::Resolved { .. } => "resolved",
                OccurrenceTarget::Candidates { .. } => "candidates",
                OccurrenceTarget::Unresolved { .. } => "unresolved",
            };
            *occurrences
                .entry(format!("{}:{resolution}", site.syntax_kind))
                .or_default() += 1;
        }
        let gaps: Vec<_> = document
            .skipped_regions
            .iter()
            .filter(|gap| gap.source.span().file() == file.id)
            .collect();
        rows.push(json!({"file":file,"source_exact":true,"occurrences":occurrences,"gaps":gaps}));
    }
    Ok(json!({
        "schema":"rootlight-index-coverage-diagnostic/1", "explicit_configuration":config.is_some(),
        "configuration_input_hash":rootlight_ids::content_hash(config.unwrap_or("").as_bytes()).to_string(),
        "configuration_hash":imported.generation().metadata().configuration_hash(),
        "cold":cold,"cold_wall_micros":cold_micros,"no_op_same_generation":true,"no_op_wall_micros":no_op_micros,
        "shared_bundle_bytes":exported.bundle().len(),"shared_bundle_limit":transfer_limits.max_bundle_bytes(),
        "coverage":status.coverage.iter().map(|entry| json!({"language":entry.language,"indexed_files":entry.indexed_files,
            "discovered_files":entry.discovered_files,"tier":entry.tier,"status":entry.status})).collect::<Vec<_>>(),
        "files":rows,"full_semantic_completeness_proven":false,
        "installed_mcp_qualified":false
    }))
}

fn chunk_end(text: &str, start: usize) -> usize {
    let mut end = start.saturating_add(16 * 1024).min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    end
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_reads_reject_oversized_and_missing_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("input");
        assert!(read_bounded(&path, 3).is_err());
        fs::write(&path, b"abcd").unwrap();
        assert!(read_bounded(&path, 3).is_err());
        assert_eq!(read_bounded(&path, 4).unwrap(), b"abcd");
        assert!(read_bounded(&path, u64::MAX).is_err());
        fs::write(&path, b"").unwrap();
        assert!(read_bounded(&path, 0).unwrap().is_empty());
    }

    #[test]
    fn source_chunks_preserve_utf8_boundaries_and_empty_files() {
        for tail in ["x", "é", "界", "🦀"] {
            let text = format!("{}{}\r\n", "x".repeat(16 * 1024 - 1), tail.repeat(10_000));
            let mut start = 0;
            let mut reconstructed = String::new();
            while start < text.len() {
                let end = chunk_end(&text, start);
                assert!(end > start && end - start <= 16 * 1024);
                reconstructed.push_str(text.get(start..end).unwrap());
                start = end;
            }
            assert_eq!(reconstructed, text);
        }
        assert_eq!(chunk_end("", 0), 0);
    }

    #[test]
    fn diagnostic_reads_multichunk_utf16_without_transcoding() {
        let directory = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(directory.path()).unwrap();
        let repository = root.join("repository");
        fs::create_dir(&repository).unwrap();
        for (name, little_endian) in [("little.txt", true), ("big.txt", false)] {
            let source = "λ𝄞\r\n".repeat(5_000);
            let bytes = std::iter::once(0xfeff_u16)
                .chain(source.encode_utf16())
                .flat_map(|unit| {
                    if little_endian {
                        unit.to_le_bytes()
                    } else {
                        unit.to_be_bytes()
                    }
                })
                .collect::<Vec<_>>();
            fs::write(repository.join(name), bytes).unwrap();
        }
        let report = inspect(&repository, &root.join("state"), None).unwrap();
        let files = report["files"].as_array().unwrap();
        assert_eq!(files.len(), 2);
        assert!(files.iter().all(|file| file["source_exact"] == true));
        assert!(
            files
                .iter()
                .all(|file| !file["gaps"].as_array().unwrap().is_empty())
        );
        assert_eq!(report["no_op_same_generation"], true);
        assert_eq!(report["full_semantic_completeness_proven"], false);
        for file in files {
            let expected = if file["file"]["path"] == "little.txt" {
                "utf-16le"
            } else {
                "utf-16be"
            };
            assert_eq!(file["file"]["encoding"], expected);
        }
    }

    #[test]
    fn diagnostic_checks_exact_sources_and_distinguishes_language_assignments() {
        let directory = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(directory.path()).unwrap();
        let repository = root.join("repository");
        fs::create_dir(&repository).unwrap();
        fs::write(repository.join("empty.txt"), b"").unwrap();
        fs::write(
            repository.join("notes.txt"),
            format!("{}🦀\r\n", "x".repeat(16 * 1024 - 1)),
        )
        .unwrap();
        fs::write(
            repository.join("module.bas"),
            b"Attribute VB_Name = \"Metrics\"\r\nSub Run()\r\nEnd Sub\r\n",
        )
        .unwrap();
        let configured = "version='1.3'\n[extensions.source-languages]\nversion='1.0'\ncritical=true\n[extensions.source-languages.data.extensions]\nbas='vba'\n";
        for (mode, config) in [("default", None), ("configured", Some(configured))] {
            let state = root.join(mode);
            let report = inspect(&repository, &state, config).unwrap();
            assert_eq!(report["explicit_configuration"], config.is_some());
            assert_eq!(report["no_op_same_generation"], true);
            assert_eq!(report["full_semantic_completeness_proven"], false);
            let files = report["files"].as_array().unwrap();
            assert_eq!(files.len(), 3);
            assert!(files.iter().all(|file| file["source_exact"] == true));
            let module = files
                .iter()
                .find(|file| file["file"]["path"] == "module.bas")
                .unwrap();
            assert_eq!(
                module["file"]["language"],
                if config.is_some() { "vba" } else { "unknown" }
            );
            assert!(inspect(&repository, &state, config).is_err());
        }
    }
}
