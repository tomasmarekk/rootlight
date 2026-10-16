//! Optional discovery census over caller-supplied, hash-inventoried source copies.
//! The observation retains every shared-suffix candidate and exact source identity;
//! content attribution is kept separate from parsing or semantic qualification.

use std::{fs, io::Write};

use super::*;

#[derive(Deserialize)]
struct CorpusInventory {
    repository: String,
    files: Vec<CorpusFile>,
}

#[derive(Deserialize)]
struct CorpusFile {
    path: String,
    bytes: u64,
    sha256: String,
    encoding: Option<String>,
    coq_suffix_candidate: bool,
    historical_diagnostic_file_identity: Option<FileId>,
}

fn shared_v_path(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|suffix| suffix.eq_ignore_ascii_case("v"))
}

fn environment_path(name: &str) -> PathBuf {
    std::env::var_os(name)
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("the observation requires {name}"))
}

#[test]
#[ignore = "requires caller-supplied retained corpus inputs and evidence directory"]
fn retained_corpus_discovery_preserves_every_shared_v_source() {
    let input_root = environment_path("ROOTLIGHT_COQ_CORPUS_ROOT");
    let inventory_path = environment_path("ROOTLIGHT_COQ_CORPUS_INVENTORY");
    let output_root = environment_path("ROOTLIGHT_COQ_CORPUS_OUTPUT");
    let repository: RepositoryId = std::env::var("ROOTLIGHT_COQ_CORPUS_REPOSITORY")
        .expect("the observation requires the original diagnostic repository identity")
        .parse()
        .unwrap();
    let inventory_bytes = fs::read(inventory_path).unwrap();
    let inventory: CorpusInventory = serde_json::from_slice(&inventory_bytes).unwrap();
    let mut candidates = BTreeMap::new();
    for file in &inventory.files {
        assert_eq!(file.coq_suffix_candidate, shared_v_path(&file.path));
        if file.coq_suffix_candidate {
            assert!(candidates.insert(file.path.clone(), file).is_none());
        }
    }
    assert!(
        !candidates.is_empty(),
        "the retained candidate set is nonempty"
    );
    let root = RepositoryRoot::open(repository, &input_root).unwrap();
    let config = ConfigSnapshot::resolve(&[]).unwrap();
    let cancellation = Cancellation::new();
    let result = discover_with_snapshots(
        &root,
        &config,
        &DiscoveryPolicy::build(Vec::new(), true).unwrap(),
        DiscoveryLimits::new(
            MAX_DISCOVERY_ENTRIES,
            MAX_DISCOVERY_DEPTH,
            rootlight_vfs::MAX_SNAPSHOT_BYTES,
            MAX_DISCOVERY_DIAGNOSTICS,
        )
        .unwrap(),
        BTreeMap::new(),
        &cancellation,
    )
    .unwrap();
    let (manifest, snapshots) = result.into_parts();
    let admitted: BTreeMap<_, _> = manifest
        .inputs
        .iter()
        .filter(|input| shared_v_path(&input.path))
        .map(|input| (input.path.as_str(), input))
        .collect();
    let mut rows = Vec::new();
    let mut dispositions = BTreeMap::<String, usize>::new();
    for (path, expected) in &candidates {
        let relative = RelativePath::parse(Path::new(path)).unwrap();
        assert_eq!(relative.as_str(), path);
        let physical = fs::read(input_root.join(path)).unwrap();
        assert_eq!(u64::try_from(physical.len()).unwrap(), expected.bytes);
        let input = admitted.get(path.as_str()).copied();
        let mut row = serde_json::json!({
            "path": path,
            "saved_source_sha256": expected.sha256,
            "saved_encoding": expected.encoding,
            "physical_bytes": physical.len(),
            "observed_content_hash": content_hash(&physical),
            "sha256_validation_scope": "external controller verifies saved SHA-256 before and after this process",
        });
        let disposition;
        if let Some(input) = input {
            let snapshot = snapshots.get(&input.file).unwrap();
            assert_eq!(snapshot.content(), physical);
            assert_eq!(input.content_hash, content_hash(&physical));
            assert_eq!(input.bytes, expected.bytes);
            assert_eq!(Some(input.encoding.as_str()), expected.encoding.as_deref());
            if let Some(historical_file) = expected.historical_diagnostic_file_identity {
                assert_eq!(input.file, historical_file);
            }
            let classification = encoding::classify(&physical, &cancellation)
                .unwrap()
                .expect("an admitted text source has a classification");
            let projected = classification
                .decoded_prefix
                .as_ref()
                .map_or(physical.as_slice(), |prefix| prefix.as_bytes());
            let prefix = &projected[..projected.len().min(MAX_CLASSIFICATION_BYTES)];
            let coq_signal = input.language_signals.iter().any(|signal| {
                signal.language == "coq" && signal.evidence == LanguageEvidence::Content
            });
            assert_eq!(
                coq_signal,
                coq::has_content_hint(prefix, classification.prefix_truncated)
            );
            let content_languages: BTreeSet<_> = input
                .language_signals
                .iter()
                .filter(|signal| signal.evidence == LanguageEvidence::Content)
                .map(|signal| signal.language.as_str())
                .collect();
            disposition = if content_languages.len() > 1 {
                "ambiguous-content-signals"
            } else if coq_signal {
                "coq-content-prefix"
            } else if content_languages.is_empty() {
                "unrecognized-content-prefix"
            } else {
                "other-content-signal"
            };
            let full_text = rootlight_vfs::SourceText::parse(&physical, &cancellation)
                .unwrap()
                .expect("the retained corpus source is safely decodable");
            let full_text: String = full_text.chars().collect();
            let full_diagnostic = coq::has_content_hint(full_text.as_bytes(), false);
            let explanation = if coq_signal {
                "a supported complete command occurs in the bounded prefix; attribution does not validate command syntax or semantics"
            } else if classification.prefix_truncated && full_diagnostic {
                "the bounded prefix establishes no supported complete Coq command; a full-document diagnostic finds command evidence outside that classification decision"
            } else {
                "the sampled bytes establish no supported complete Coq command; source remains retained, and unsupported commands or inert source are not declared invalid"
            };
            row["manifest_input"] = serde_json::to_value(input).unwrap();
            row["snapshot_equals_physical_source"] = true.into();
            row["classification_prefix_decoded_bytes"] = prefix.len().into();
            row["classification_prefix_truncated"] = classification.prefix_truncated.into();
            row["classification_prefix_content_hash"] = content_hash(prefix).to_string().into();
            row["full_source_coq_hint_diagnostic_only"] = full_diagnostic.into();
            row["explanation"] = explanation.into();
        } else {
            disposition = "not-admitted";
            row["exclusions"] = serde_json::to_value(
                manifest
                    .exclusions
                    .iter()
                    .filter(|exclusion| exclusion.path == *path)
                    .collect::<Vec<_>>(),
            )
            .unwrap();
            row["explanation"] =
                "the physical source exists in the saved census but is absent from discovery inputs"
                    .into();
        }
        row["disposition"] = disposition.into();
        *dispositions.entry(disposition.to_owned()).or_default() += 1;
        rows.push(row);
    }
    let summary = serde_json::json!({
        "schema": "rootlight.coq-discovery-observation/1",
        "corpus_namespace": inventory.repository,
        "repository": repository,
        "repository_identity_scope": "caller-supplied original diagnostic identity, not a registered service repository",
        "saved_inventory_content_hash": content_hash(&inventory_bytes),
        "candidate_files": candidates.len(),
        "admitted_candidate_files": admitted.len(),
        "classification_prefix_limit_bytes": MAX_CLASSIFICATION_BYTES,
        "dispositions": dispositions,
        "parsing_or_semantic_support_proven": false,
        "installed_package_qualified": false,
        "files": rows,
        "actual_discovery_manifest": manifest,
    });
    fs::create_dir_all(&output_root).unwrap();
    let mut output = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(output_root.join("observation.json"))
        .unwrap();
    output
        .write_all(&serde_json::to_vec_pretty(&summary).unwrap())
        .unwrap();
    output.write_all(b"\n").unwrap();
    println!(
        "{}",
        serde_json::json!({
            "candidate_files": candidates.len(),
            "admitted_candidate_files": admitted.len(),
            "dispositions": dispositions,
        })
    );
    assert!(manifest.coverage.complete);
    assert_eq!(
        candidates.keys().map(String::as_str).collect::<Vec<_>>(),
        admitted.keys().copied().collect::<Vec<_>>()
    );
}
