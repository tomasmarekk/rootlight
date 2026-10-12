//! Reports bounded, read-only discovery without daemon state or repository writes.
//! CLI: source_inventory REPOSITORY [CONFIG_FILE] [--probe-fsharp-admission].
//! Native admission is opt-in; ordinary inventory does not invoke a parser.
//! File IDs use a shared diagnostic namespace, not service repository or generation identities.
//! Audit exclusions describe observed entries only; complete traversal is not semantic coverage.

use std::{
    error::Error,
    ffi::OsString,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

use rootlight_cancel::Cancellation;
use rootlight_config::{ConfigLayer, ConfigSnapshot, ConfigSource, MAX_CONFIG_LAYER_BYTES};
use rootlight_discovery::{DiscoveryLimits, DiscoveryManifest, DiscoveryPolicy, discover};
use rootlight_git::{GitCollectLimits, collect_tracked_paths};
use rootlight_ids::{ContentHash, derive_repository};
use rootlight_vfs::RepositoryRoot;
use serde::Serialize;

#[path = "source_inventory_example/admission.rs"]
mod admission;

const DIAGNOSTIC_NAMESPACE: &str = "rootlight.source-inventory.diagnostic/1";
const USAGE: &str = "usage: source_inventory REPOSITORY [CONFIG_FILE] [--probe-fsharp-admission]";

#[derive(Serialize)]
struct InventoryReport<'a> {
    schema: &'static str,
    explicit_configuration: bool,
    identity_namespace: &'static str,
    identity_scope: &'static str,
    coverage_complete_scope: &'static str,
    exclusion_ledger_scope: &'static str,
    manifest_hash: ContentHash,
    manifest: &'a DiscoveryManifest,
    full_semantic_completeness_proven: bool,
    installed_mcp_qualified: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    fsharp_admission: Option<&'a admission::AdmissionReport>,
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

fn read_config(path: &Path) -> Result<String, Box<dyn Error>> {
    Ok(String::from_utf8(read_bounded(
        path,
        u64::try_from(MAX_CONFIG_LAYER_BYTES)?,
    )?)?)
}

fn resolve_config(contents: Option<&str>) -> Result<ConfigSnapshot, Box<dyn Error>> {
    let layer = contents.map(|contents| ConfigLayer {
        source: ConfigSource::User,
        contents,
    });
    Ok(ConfigSnapshot::resolve(layer.as_slice())?)
}

fn inventory(
    repository: &Path,
    config: &ConfigSnapshot,
) -> Result<DiscoveryManifest, Box<dyn Error>> {
    let cancellation = Cancellation::new();
    let repository_id = derive_repository(DIAGNOSTIC_NAMESPACE.as_bytes()).id();
    let root = RepositoryRoot::open(repository_id, repository)?;
    let tracked = collect_tracked_paths(
        root.local_path(),
        GitCollectLimits::default(),
        &cancellation,
    )?;
    let policy = DiscoveryPolicy::build(Vec::new(), true)?.with_tracked_files_in_root(
        &root,
        tracked,
        &cancellation,
    )?;
    Ok(discover(
        &root,
        config,
        &policy,
        DiscoveryLimits::from_config(config),
        &cancellation,
    )?)
}

fn write_report(
    manifest: &DiscoveryManifest,
    explicit_configuration: bool,
    fsharp_admission: Option<&admission::AdmissionReport>,
    output: &mut impl Write,
) -> Result<(), Box<dyn Error>> {
    let report = InventoryReport {
        schema: "rootlight-source-inventory-diagnostic/1",
        explicit_configuration,
        identity_namespace: DIAGNOSTIC_NAMESPACE,
        identity_scope: "shared diagnostic path identities, not registered service identities",
        coverage_complete_scope: "bounded traversal within discovery policy, not language support",
        exclusion_ledger_scope: "observed entries only; descendants of excluded directories are not enumerated",
        manifest_hash: manifest.hash()?,
        manifest,
        full_semantic_completeness_proven: false,
        installed_mcp_qualified: false,
        fsharp_admission,
    };
    let mut output = std::io::BufWriter::new(output);
    serde_json::to_writer(&mut output, &report)?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}

fn arguments(
    args: impl IntoIterator<Item = OsString>,
) -> Result<(PathBuf, Option<PathBuf>, bool), Box<dyn Error>> {
    let mut args = args.into_iter();
    let repository = PathBuf::from(args.next().ok_or(USAGE)?);
    let mut config_path = None;
    let mut probe = false;
    for argument in args {
        if argument == "--probe-fsharp-admission" && !probe {
            probe = true;
        } else if argument != "--probe-fsharp-admission" && config_path.is_none() {
            config_path = Some(PathBuf::from(argument));
        } else {
            return Err(USAGE.into());
        }
    }
    Ok((repository, config_path, probe))
}

fn main() -> Result<(), Box<dyn Error>> {
    let (repository, config_path, probe) = arguments(std::env::args_os().skip(1))?;
    let contents = config_path.as_deref().map(read_config).transpose()?;
    let config = resolve_config(contents.as_deref())?;
    let manifest = inventory(&repository, &config)?;
    let fsharp_admission = if probe {
        let root = RepositoryRoot::open(
            derive_repository(DIAGNOSTIC_NAMESPACE.as_bytes()).id(),
            &repository,
        )?;
        Some(admission::observe(
            &root,
            &manifest,
            &config,
            &Cancellation::new(),
        )?)
    } else {
        None
    };
    write_report(
        &manifest,
        contents.is_some(),
        fsharp_admission.as_ref(),
        &mut std::io::stdout().lock(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_discovery::{ExclusionReason, PolicyLayer, PolicyRule};
    use rootlight_ids::content_hash;
    use std::collections::BTreeSet;

    fn fixture(files: &[(&str, &[u8])]) -> (tempfile::TempDir, RepositoryRoot) {
        let directory = tempfile::tempdir().unwrap();
        for (relative, bytes) in files {
            let path = directory.path().join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }
        let canonical = fs::canonicalize(directory.path()).unwrap();
        let root = RepositoryRoot::open(
            derive_repository(DIAGNOSTIC_NAMESPACE.as_bytes()).id(),
            &canonical,
        )
        .unwrap();
        (directory, root)
    }

    fn audit(
        root: &RepositoryRoot,
        tracked: &[&str],
        rules: Vec<PolicyRule>,
        limits: DiscoveryLimits,
    ) -> DiscoveryManifest {
        let cancellation = Cancellation::new();
        let tracked: BTreeSet<_> = tracked.iter().map(|path| (*path).to_owned()).collect();
        let policy = DiscoveryPolicy::build(rules, true)
            .unwrap()
            .with_tracked_files_in_root(root, tracked, &cancellation)
            .unwrap();
        discover(
            root,
            &resolve_config(None).unwrap(),
            &policy,
            limits,
            &cancellation,
        )
        .unwrap()
    }

    fn limits() -> DiscoveryLimits {
        DiscoveryLimits::new(100, 16, 1024, 100).unwrap()
    }

    #[test]
    fn tracked_nested_artifact_source_does_not_admit_untracked_siblings() {
        let source = b"pub fn retained() {}\r\n";
        let (_directory, root) = fixture(&[
            ("package/target/deep/retained.rs", source),
            (
                "package/target/deep/untracked.rs",
                b"pub fn untracked() {}\n",
            ),
        ]);
        let manifest = audit(
            &root,
            &["package/target/deep/retained.rs"],
            Vec::new(),
            limits(),
        );
        assert!(manifest.coverage.complete);
        assert_eq!(manifest.inputs.len(), 1);
        let input = &manifest.inputs[0];
        assert_eq!(input.path, "package/target/deep/retained.rs");
        assert_eq!(input.content_hash, content_hash(source));
        assert_eq!(input.bytes, u64::try_from(source.len()).unwrap());
        assert_eq!(input.encoding, "utf-8");
        assert!(
            input
                .language_signals
                .iter()
                .any(|signal| signal.language == "rust")
        );
        let exclusion = manifest
            .exclusions
            .iter()
            .find(|entry| entry.path == "package/target/deep/untracked.rs")
            .unwrap();
        assert_eq!(exclusion.reason, ExclusionReason::Policy);
        assert_eq!(
            exclusion.decisive_rule.as_ref().unwrap().source,
            "rootlight-default"
        );
    }

    #[test]
    fn tracked_metadata_and_explicitly_excluded_sources_remain_excluded() {
        let (_directory, root) = fixture(&[
            (".git/private.rs", b"pub fn metadata() {}\n"),
            ("scope/build/kept.rs", b"pub fn kept() {}\n"),
            ("scope/build/denied.rs", b"pub fn denied() {}\n"),
            ("blocked/child.rs", b"pub fn blocked() {}\n"),
        ]);
        let manifest = audit(
            &root,
            &[
                ".git/private.rs",
                "scope/build/kept.rs",
                "scope/build/denied.rs",
                "blocked/child.rs",
            ],
            vec![
                PolicyRule {
                    layer: PolicyLayer::Repository,
                    pattern: "**/denied.rs".to_owned(),
                    source: "repository".to_owned(),
                },
                PolicyRule {
                    layer: PolicyLayer::Operation,
                    pattern: "blocked/".to_owned(),
                    source: "operation".to_owned(),
                },
            ],
            limits(),
        );
        assert_eq!(
            manifest
                .inputs
                .iter()
                .map(|entry| entry.path.as_str())
                .collect::<Vec<_>>(),
            ["scope/build/kept.rs"],
        );
        for path in [".git", "blocked", "scope/build/denied.rs"] {
            assert!(
                manifest
                    .exclusions
                    .iter()
                    .any(|entry| { entry.path == path && entry.reason == ExclusionReason::Policy })
            );
        }
        assert!(
            !manifest.exclusions.iter().any(|entry| {
                entry.path == ".git/private.rs" || entry.path == "blocked/child.rs"
            })
        );
        let denied = manifest
            .exclusions
            .iter()
            .find(|entry| entry.path == "scope/build/denied.rs")
            .unwrap();
        let rule = denied.decisive_rule.as_ref().unwrap();
        assert_eq!(rule.source, "repository");
        assert_eq!(rule.pattern, "**/denied.rs");
    }

    #[test]
    fn tracked_admission_preserves_binary_size_and_original_encoding_safety() {
        let utf16 = [0xff, 0xfe, b'x', 0, b'\n', 0];
        let oversized = [b'x'; 129];
        let (_directory, root) = fixture(&[
            ("scope/target/binary.dat", &[0, 1, 2, 3]),
            ("scope/target/oversized.rs", &oversized),
            ("scope/target/text.txt", &utf16),
        ]);
        let manifest = audit(
            &root,
            &[
                "scope/target/binary.dat",
                "scope/target/oversized.rs",
                "scope/target/text.txt",
            ],
            Vec::new(),
            DiscoveryLimits::new(100, 16, 128, 100).unwrap(),
        );
        assert_eq!(manifest.inputs.len(), 1);
        let input = &manifest.inputs[0];
        assert_eq!(input.path, "scope/target/text.txt");
        assert_eq!(input.content_hash, content_hash(&utf16));
        assert_eq!(input.bytes, u64::try_from(utf16.len()).unwrap());
        assert_eq!(input.encoding, "utf-16le");
        for (path, reason) in [
            ("scope/target/binary.dat", ExclusionReason::Binary),
            ("scope/target/oversized.rs", ExclusionReason::Oversized),
        ] {
            assert!(
                manifest
                    .exclusions
                    .iter()
                    .any(|entry| entry.path == path && entry.reason == reason)
            );
        }
    }

    #[test]
    fn configuration_reads_are_bounded_and_require_utf8() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("configuration.toml");
        assert!(read_config(&path).is_err());
        fs::write(&path, vec![b'x'; MAX_CONFIG_LAYER_BYTES + 1]).unwrap();
        assert!(read_config(&path).is_err());
        fs::write(&path, [0xff]).unwrap();
        assert!(read_config(&path).is_err());
        fs::write(&path, b"version='1.3'\n").unwrap();
        let text = read_config(&path).unwrap();
        assert_eq!(text, "version='1.3'\n");
        assert!(resolve_config(Some(&text)).is_ok());
        assert_eq!(
            read_bounded(&path, u64::MAX).unwrap_err().to_string(),
            "read bound overflow"
        );
    }

    #[test]
    fn report_retains_the_canonical_manifest_without_semantic_acceptance_claims() {
        let (_directory, root) = fixture(&[("source.rs", b"pub fn source() {}\n")]);
        let manifest = audit(&root, &[], Vec::new(), limits());
        let mut output = Vec::new();
        write_report(&manifest, false, None, &mut output).unwrap();
        let report: serde_json::Value = serde_json::from_slice(&output).unwrap();
        let restored: DiscoveryManifest =
            serde_json::from_value(report["manifest"].clone()).unwrap();
        assert_eq!(
            restored.canonical_bytes().unwrap(),
            manifest.canonical_bytes().unwrap()
        );
        assert_eq!(
            report["manifest_hash"],
            manifest.hash().unwrap().to_string()
        );
        assert_eq!(report["identity_namespace"], DIAGNOSTIC_NAMESPACE);
        assert_eq!(report["explicit_configuration"], false);
        assert_eq!(report["full_semantic_completeness_proven"], false);
        assert_eq!(report["installed_mcp_qualified"], false);
        assert!(report.get("fsharp_admission").is_none());
        assert!(output.ends_with(b"\n"));
    }

    #[test]
    fn optional_probe_preserves_existing_positional_arguments() {
        let parse = |args: &[&str]| arguments(args.iter().map(OsString::from));
        assert_eq!(parse(&["repo"]).unwrap(), ("repo".into(), None, false));
        assert_eq!(
            parse(&["repo", "config.toml"]).unwrap(),
            ("repo".into(), Some("config.toml".into()), false)
        );
        for args in [
            ["repo", "config.toml", "--probe-fsharp-admission"],
            ["repo", "--probe-fsharp-admission", "config.toml"],
        ] {
            assert_eq!(
                parse(&args).unwrap(),
                ("repo".into(), Some("config.toml".into()), true)
            );
        }
        assert!(
            parse(&[
                "repo",
                "--probe-fsharp-admission",
                "--probe-fsharp-admission"
            ])
            .is_err()
        );
        assert!(parse(&["repo", "one.toml", "two.toml"]).is_err());
    }

    #[test]
    fn admission_reports_exact_utf8_and_utf16_evidence_without_changing_discovery() {
        let text = "let identity value = value\n";
        let encode = |little_endian: bool| {
            "\u{feff}"
                .encode_utf16()
                .chain(text.encode_utf16())
                .flat_map(|unit| {
                    if little_endian {
                        unit.to_le_bytes()
                    } else {
                        unit.to_be_bytes()
                    }
                })
                .collect::<Vec<_>>()
        };
        let little = encode(true);
        let big = encode(false);
        let (_directory, root) = fixture(&[
            ("Plain.fs", text.as_bytes()),
            ("Little.fs", &little),
            ("Big.FS", &big),
            ("Atomic.fs", b"let value = 1\n"),
            ("Foreign.fs", b": square dup * ;\n"),
            ("Signature.fsi", b"val identity: int -> int\n"),
        ]);
        let manifest = audit(&root, &[], Vec::new(), limits());
        let original = manifest.canonical_bytes().unwrap();
        let report = admission::observe(
            &root,
            &manifest,
            &resolve_config(None).unwrap(),
            &Cancellation::new(),
        )
        .unwrap();
        assert_eq!(manifest.canonical_bytes().unwrap(), original);
        let value = serde_json::to_value(&report).unwrap();
        assert_eq!(value["totals"]["accepted"], 3);
        assert_eq!(value["totals"]["no_evidence"], 1);
        assert_eq!(value["totals"]["guarded"], 1);
        assert_eq!(value["full_semantic_completeness_proven"], false);
        assert_eq!(value["installed_mcp_qualified"], false);
        let rows = value["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 5);
        for row in rows.iter().filter(|row| row["outcome"] == "accepted") {
            let input = &manifest.inputs
                [usize::try_from(row["manifest_ordinal"].as_u64().unwrap()).unwrap()];
            assert_eq!(row["path"], input.path);
            assert_eq!(row["witness_kind"], "named_function");
            let declaration: rootlight_ir::SourceRef =
                serde_json::from_value(row["declaration"].clone()).unwrap();
            let sample: rootlight_ir::SourceRef =
                serde_json::from_value(row["sample"].clone()).unwrap();
            for source in [&declaration, &sample] {
                assert_eq!(source.repository(), root.repository());
                assert_eq!(source.content_hash(), input.content_hash);
                assert_eq!(source.span().file(), input.file);
                assert_eq!(
                    serde_json::to_value(source.generation()).unwrap(),
                    value["generation"]
                );
            }
            assert_eq!(sample.span().start_byte(), 0);
            assert_eq!(sample.span().end_byte(), input.bytes);
            assert_eq!(
                declaration.span().start_byte(),
                if input.encoding == "utf-8" { 0 } else { 2 }
            );
            assert!(declaration.span().end_byte() <= input.bytes);
        }
    }

    #[test]
    fn admission_keeps_configuration_and_contradictory_evidence_authoritative() {
        let (_directory, root) = fixture(&[("Configured.fs", b"let identity value = value\n")]);
        let mut manifest = audit(&root, &[], Vec::new(), limits());
        manifest.inputs[0].language_signals = vec![
            rootlight_discovery::LanguageSignal {
                language: "forth".to_owned(),
                evidence: rootlight_discovery::LanguageEvidence::Configuration,
            },
            rootlight_discovery::LanguageSignal {
                language: "fsharp".to_owned(),
                evidence: rootlight_discovery::LanguageEvidence::Content,
            },
        ];
        let report = admission::observe(
            &root,
            &manifest,
            &resolve_config(None).unwrap(),
            &Cancellation::new(),
        )
        .unwrap();
        let value = serde_json::to_value(&report).unwrap();
        assert_eq!(value["rows"][0]["outcome"], "guarded");
        assert_eq!(value["rows"][0]["reason"], "language_signals");
        assert_eq!(value["totals"]["accepted"], 0);
    }

    #[test]
    fn admission_rejects_source_drift_instead_of_recording_negative_evidence() {
        let (directory, root) = fixture(&[("Sample.fs", b"let identity value = value\n")]);
        let manifest = audit(&root, &[], Vec::new(), limits());
        fs::write(
            directory.path().join("Sample.fs"),
            b"let changed value = value\n",
        )
        .unwrap();
        let error = admission::observe(
            &root,
            &manifest,
            &resolve_config(None).unwrap(),
            &Cancellation::new(),
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "diagnostic snapshot differs from discovery input"
        );
    }
}
